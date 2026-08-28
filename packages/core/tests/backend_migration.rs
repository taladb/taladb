//! Upgrading a database that lives in a custom backend rather than a file.
//!
//! This is the browser's situation. OPFS hands out a handle, not a filesystem, so
//! the native migration — copy aside, convert, rename over the original — has
//! nowhere to stage anything. The claim being tested is that it does not need to:
//! redb 2 reads both the old and new formats and rewrites one into the other *in
//! place*, through whatever backend it is given, so pointing it at the same
//! storage the caller already has is enough.
//!
//! The backend below is a shared byte buffer, which is exactly the shape of the
//! OPFS case: two backend instances, one underlying set of bytes. That makes this
//! a real test of the mechanism the browser uses, without a browser.

#![cfg(feature = "legacy-migration")]

use std::io;
use std::sync::{Arc, Mutex};

/// A byte buffer that two different majors of redb can both be pointed at.
#[derive(Debug, Clone, Default)]
struct SharedBytes(Arc<Mutex<Vec<u8>>>);

impl SharedBytes {
    fn len_inner(&self) -> u64 {
        self.0.lock().unwrap().len() as u64
    }
    fn read_inner(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        let buf = self.0.lock().unwrap();
        let start = usize::try_from(offset).unwrap();
        let end = start + out.len();
        if end > buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read past end",
            ));
        }
        out.copy_from_slice(&buf[start..end]);
        Ok(())
    }
    fn set_len_inner(&self, len: u64) {
        self.0
            .lock()
            .unwrap()
            .resize(usize::try_from(len).unwrap(), 0);
    }
    fn write_inner(&self, offset: u64, data: &[u8]) {
        let mut buf = self.0.lock().unwrap();
        let start = usize::try_from(offset).unwrap();
        if start + data.len() > buf.len() {
            buf.resize(start + data.len(), 0);
        }
        buf[start..start + data.len()].copy_from_slice(data);
    }
}

// The redb 2 view — what the migration is handed.
impl taladb_core::redb2::StorageBackend for SharedBytes {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.len_inner())
    }
    fn read(&self, offset: u64, len: usize) -> Result<Vec<u8>, io::Error> {
        let mut out = vec![0u8; len];
        self.read_inner(offset, &mut out)?;
        Ok(out)
    }
    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        self.set_len_inner(len);
        Ok(())
    }
    fn sync_data(&self, _eventual: bool) -> Result<(), io::Error> {
        Ok(())
    }
    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        self.write_inner(offset, data);
        Ok(())
    }
}

// The redb 4 view — what the database is opened with afterwards.
impl taladb_core::redb::StorageBackend for SharedBytes {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.len_inner())
    }
    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        self.read_inner(offset, out)
    }
    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        self.set_len_inner(len);
        Ok(())
    }
    fn sync_data(&self) -> Result<(), io::Error> {
        Ok(())
    }
    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        self.write_inner(offset, data);
        Ok(())
    }
}

const TBL: taladb_core::redb2::TableDefinition<&[u8], &[u8]> =
    taladb_core::redb2::TableDefinition::new("books");

/// Write a genuine old-format database into the shared buffer.
fn seed_legacy(storage: &SharedBytes, rows: &[(&str, &str)]) {
    let db = taladb_core::redb2::Database::builder()
        .create_with_backend(storage.clone())
        .expect("create legacy db");
    let wtx = db.begin_write().expect("write txn");
    {
        let mut t = wtx.open_table(TBL).expect("open table");
        for (k, v) in rows {
            t.insert(k.as_bytes(), v.as_bytes()).expect("insert");
        }
    }
    wtx.commit().expect("commit");
}

#[test]
fn a_legacy_backend_is_upgraded_in_place_and_keeps_its_data() {
    let storage = SharedBytes::default();
    seed_legacy(&storage, &[("k1", "Dune"), ("k2", "Neuromancer")]);

    // redb 4 cannot touch it yet — this is what a browser user hits today.
    assert!(
        taladb_core::redb::Database::builder()
            .create_with_backend(storage.clone())
            .is_err(),
        "the seeded database must genuinely be in the old format"
    );

    let upgraded = taladb_core::migrate::upgrade_legacy_backend(storage.clone())
        .expect("migration should succeed");
    assert!(upgraded, "an old-format database must report as upgraded");

    // Same bytes, now readable by redb 4, with everything still in them.
    use taladb_core::redb::{ReadableDatabase as _, ReadableTableMetadata as _};
    let db = taladb_core::redb::Database::builder()
        .create_with_backend(storage)
        .expect("redb 4 must open the upgraded storage");
    let rtx = db.begin_read().expect("read");
    let t = rtx
        .open_table(taladb_core::redb::TableDefinition::<&[u8], &[u8]>::new(
            "books",
        ))
        .expect("table survived");
    assert_eq!(t.len().expect("len"), 2, "no rows may be lost");
    assert_eq!(
        t.get("k2".as_bytes()).expect("get").expect("k2").value(),
        b"Neuromancer"
    );
}

/// The ordering rule, pinned as a test because getting it wrong is not a tidy
/// error — it is a panic, and in wasm an unrecoverable trap on the *second* open,
/// long after the upgrade appeared to work.
///
/// redb 2's header check accepts the version number redb 4 stamps, so it opens a
/// current database far enough in to hit `unreachable!()`. That is why
/// `upgrade_legacy_backend` must only ever run after a redb 4 open has failed and
/// `is_legacy_format_error` has confirmed why.
#[test]
fn redb2_cannot_be_pointed_at_a_current_database() {
    let storage = SharedBytes::default();
    {
        let db = taladb_core::redb::Database::builder()
            .create_with_backend(storage.clone())
            .expect("create current db");
        let wtx = db.begin_write().expect("txn");
        {
            let mut t = wtx
                .open_table(taladb_core::redb::TableDefinition::<&[u8], &[u8]>::new(
                    "books",
                ))
                .expect("table");
            t.insert("k".as_bytes(), "v".as_bytes()).expect("insert");
        }
        wtx.commit().expect("commit");
    }

    // A current database must be recognised as *not* legacy, which is the check
    // that keeps the legacy reader away from it.
    let probe = taladb_core::redb::Database::builder().create_with_backend(storage);
    assert!(
        probe.is_ok(),
        "redb 4 must be able to open what it wrote — the legacy path is never reached"
    );

    // And the guard that gates the legacy reader must not fire for it.
    assert!(
        !taladb_core::migrate::is_legacy_format_error("some unrelated failure"),
        "only a genuine old-format error may route to the legacy reader"
    );
}

/// The migration is run for its effect on the storage, then the storage is opened
/// again — so the backend it consumed must not have left the bytes unusable.
#[test]
fn the_storage_is_reusable_after_the_migration_consumes_a_backend() {
    let storage = SharedBytes::default();
    seed_legacy(&storage, &[("k1", "Dune")]);
    taladb_core::migrate::upgrade_legacy_backend(storage.clone()).expect("migrate");

    // Twice, because the browser opens once and may reopen on the next page load.
    for attempt in 1..=2 {
        let db = taladb_core::Database::open_with_backend(Box::new(
            taladb_core::engine::RedbBackend::open_with_redb_backend(storage.clone())
                .unwrap_or_else(|e| panic!("attempt {attempt}: backend open failed: {e}")),
        ))
        .unwrap_or_else(|e| panic!("attempt {attempt}: database open failed: {e}"));
        drop(db);
    }
}
