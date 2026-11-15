//! Reopen after an interrupted write, at the level the browser OPFS backend
//! sees storage: a byte-addressed `redb::StorageBackend` whose writes are
//! durable only once `sync_data` returns.
//!
//! `CrashDisk` keeps the last synced image plus the writes issued since. A
//! "crash" reopens from the synced image with any ordered subset of the pending
//! writes applied, which is what a lost tab, killed worker or power cut can
//! leave behind. The commit's own `sync_data` is made to fail, so the
//! transaction under test is one the caller was never told had committed.

use std::sync::{Arc, Mutex};

use taladb::{Database, Filter, RedbBackend, Value};

#[derive(Debug, Clone)]
enum Op {
    Write(u64, Vec<u8>),
    SetLen(u64),
}

#[derive(Debug, Default)]
struct Disk {
    durable: Vec<u8>,
    live: Vec<u8>,
    pending: Vec<Op>,
    fail_sync: bool,
    bytes_written: u64,
}

#[derive(Debug, Clone, Default)]
struct CrashDisk(Arc<Mutex<Disk>>);

impl CrashDisk {
    fn from_image(image: Vec<u8>) -> Self {
        let disk = Disk {
            durable: image.clone(),
            live: image,
            ..Disk::default()
        };
        Self(Arc::new(Mutex::new(disk)))
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Disk> {
        self.0.lock().unwrap()
    }
}

fn apply(image: &mut Vec<u8>, op: &Op) {
    match op {
        Op::SetLen(len) => image.resize(*len as usize, 0),
        Op::Write(offset, data) => {
            let end = *offset as usize + data.len();
            if image.len() < end {
                image.resize(end, 0);
            }
            image[*offset as usize..end].copy_from_slice(data);
        }
    }
}

impl redb::StorageBackend for CrashDisk {
    fn len(&self) -> Result<u64, std::io::Error> {
        Ok(self.lock().live.len() as u64)
    }
    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), std::io::Error> {
        let disk = self.lock();
        let start = offset as usize;
        let slice = disk
            .live
            .get(start..start + out.len())
            .ok_or_else(|| std::io::Error::other("short read"))?;
        out.copy_from_slice(slice);
        Ok(())
    }
    fn set_len(&self, len: u64) -> Result<(), std::io::Error> {
        let mut disk = self.lock();
        let op = Op::SetLen(len);
        apply(&mut disk.live, &op);
        disk.pending.push(op);
        Ok(())
    }
    fn sync_data(&self) -> Result<(), std::io::Error> {
        let mut disk = self.lock();
        if disk.fail_sync {
            return Err(std::io::Error::other("injected flush failure"));
        }
        disk.durable = disk.live.clone();
        disk.pending.clear();
        Ok(())
    }
    fn write(&self, offset: u64, data: &[u8]) -> Result<(), std::io::Error> {
        let mut disk = self.lock();
        let op = Op::Write(offset, data.to_vec());
        apply(&mut disk.live, &op);
        disk.pending.push(op);
        disk.bytes_written += data.len() as u64;
        Ok(())
    }
}

fn open(disk: CrashDisk) -> Database {
    let backend = RedbBackend::open_with_redb_backend(disk).expect("redb opens the image");
    Database::open_with_backend(Box::new(backend)).expect("TalaDB opens the image")
}

fn doc(group: &str, n: i64, word: &str) -> Vec<(String, Value)> {
    vec![
        ("group".into(), Value::Str(group.into())),
        // `n` is indexed and `m` is not: equal answers mean the index matches the data.
        ("n".into(), Value::Int(n)),
        ("m".into(), Value::Int(n)),
        ("body".into(), Value::Str(format!("{word} note {n}"))),
        (
            "v".into(),
            Value::Array(vec![Value::Float(1.0), Value::Float(n as f64)]),
        ),
    ]
}

const ACKED: usize = 20;
const TORN: usize = 5;

/// Build a database with all three index kinds and `ACKED` committed documents,
/// then attempt a `TORN`-document insert whose commit cannot be made durable.
fn interrupted_insert() -> (Vec<u8>, Vec<Op>) {
    let disk = CrashDisk::default();
    let db = open(disk.clone());
    let col = db.collection("docs").unwrap();
    col.create_index("n").unwrap();
    col.create_fts_index("body").unwrap();
    col.create_vector_index("v", 2, None, None).unwrap();
    col.insert_many(
        (0..ACKED as i64)
            .map(|n| doc("acked", n, "ackedword"))
            .collect(),
    )
    .unwrap();

    disk.lock().fail_sync = true;
    let torn = col.insert_many(
        (0..TORN as i64)
            .map(|n| doc("torn", 1000 + n, "tornword"))
            .collect(),
    );
    assert!(
        torn.is_err(),
        "a commit whose flush failed must not be acknowledged"
    );

    // Captured before the handle is dropped: nothing after the failed flush runs.
    let d = disk.lock();
    (d.durable.clone(), d.pending.clone())
}

/// Every invariant a reopened browser database must hold, whatever subset of
/// the interrupted transaction's writes reached storage.
fn check(image: Vec<u8>, label: &str) -> usize {
    let db = open(CrashDisk::from_image(image));
    let col = db.collection("docs").unwrap();
    let group = |g: &str| Filter::Eq("group".into(), Value::Str(g.into()));

    let acked = col.find(group("acked")).unwrap().len();
    assert_eq!(acked, ACKED, "{label}: acknowledged documents survive");

    let torn = col.find(group("torn")).unwrap().len();
    assert!(
        torn == 0 || torn == TORN,
        "{label}: the interrupted transaction is all or nothing, saw {torn}/{TORN}"
    );
    let total = ACKED + torn;
    assert_eq!(
        col.count(Filter::All).unwrap() as usize,
        total,
        "{label}: count"
    );

    for n in [0_i64, 7, 19, 1000, 1004] {
        assert_eq!(
            col.count(Filter::Eq("n".into(), Value::Int(n))).unwrap(),
            col.count(Filter::Eq("m".into(), Value::Int(n))).unwrap(),
            "{label}: scalar index agrees with a scan for n={n}"
        );
    }
    assert_eq!(
        col.search_text("body", "tornword", 50).unwrap().len(),
        torn,
        "{label}: FTS index matches the documents present"
    );
    assert_eq!(
        col.search_text("body", "ackedword", 50).unwrap().len(),
        ACKED,
        "{label}: FTS index keeps committed documents"
    );
    let hits = col.find_nearest("v", &[1.0, 0.0], 100, None).unwrap();
    assert_eq!(
        hits.len(),
        total,
        "{label}: vector index matches the documents present"
    );

    let indexes = col.list_indexes().unwrap();
    assert!(
        indexes.btree.iter().any(|f| f == "n"),
        "{label}: scalar index kept"
    );
    assert!(
        indexes.fts.iter().any(|f| f == "body"),
        "{label}: FTS index kept"
    );
    assert!(
        indexes.vector.iter().any(|f| f == "v"),
        "{label}: vector index kept"
    );
    torn
}

#[test]
fn every_crash_point_of_an_unacknowledged_commit_reopens_consistently() {
    let (durable, pending) = interrupted_insert();
    assert!(!pending.is_empty(), "the interrupted commit issued writes");
    let mut visible = 0;
    for k in 0..=pending.len() {
        let mut image = durable.clone();
        for op in &pending[..k] {
            apply(&mut image, op);
        }
        if check(image, &format!("prefix {k}/{}", pending.len())) > 0 {
            visible += 1;
        }
    }
    // With no write landed the transaction is absent; the run also reports
    // whether a fully written but unflushed commit was recovered.
    eprintln!(
        "interrupted commit: {} writes, visible at {visible} of {} crash points",
        pending.len(),
        pending.len() + 1
    );
}

#[test]
fn reordered_partial_writes_of_an_unacknowledged_commit_reopen_consistently() {
    let (durable, pending) = interrupted_insert();
    // Storage may persist unsynced writes out of order. A fixed LCG keeps the
    // subsets reproducible without adding a dependency.
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    for round in 0..64 {
        let mut image = durable.clone();
        for op in &pending {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            if seed >> 63 == 1 {
                apply(&mut image, op);
            }
        }
        check(image, &format!("subset round {round}"));
    }
}

#[test]
fn a_clean_reopen_does_not_rebuild_indexes() {
    let disk = CrashDisk::default();
    {
        let db = open(disk.clone());
        let col = db.collection("docs").unwrap();
        col.create_index("n").unwrap();
        col.create_fts_index("body").unwrap();
        col.create_vector_index("v", 2, None, None).unwrap();
        col.insert_many((0..2_000).map(|n| doc("acked", n, "ackedword")).collect())
            .unwrap();
    }
    let (image, data_bytes) = {
        let d = disk.lock();
        (d.live.clone(), d.bytes_written)
    };
    let reopened = CrashDisk::from_image(image);
    let db = open(reopened.clone());
    let col = db.collection("docs").unwrap();
    assert_eq!(
        col.count(Filter::Eq("n".into(), Value::Int(1234))).unwrap(),
        1
    );
    assert_eq!(col.search_text("body", "ackedword", 5).unwrap().len(), 5);
    assert_eq!(
        col.find_nearest("v", &[1.0, 0.0], 3, None).unwrap().len(),
        3
    );
    let reopen_bytes = reopened.lock().bytes_written;
    eprintln!("initial load wrote {data_bytes} bytes; reopen and queries wrote {reopen_bytes}");
    // Rebuilding any of the three indexes for 2,000 documents rewrites a large
    // share of the file. Opening and reading must not.
    assert!(
        reopen_bytes * 20 < data_bytes,
        "reopen wrote {reopen_bytes} of {data_bytes} bytes; an index was rebuilt"
    );
}
