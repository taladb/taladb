use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use taladb_core::engine::{ReadTxn, WriteTxn};
use taladb_core::{
    Database, Document, Filter, RedbBackend, StorageBackend, TalaDbError, Update, Value,
};

struct Gate {
    armed: AtomicBool,
    entered: mpsc::Sender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
}
impl Gate {
    fn wait(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            self.resume
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
        }
    }
}
struct ControlledBackend {
    inner: RedbBackend,
    gate: Arc<Gate>,
    pause_read: bool,
}
impl StorageBackend for ControlledBackend {
    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        let txn = self.inner.begin_read()?;
        if self.pause_read {
            self.gate.wait();
        }
        Ok(txn)
    }
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        if !self.pause_read {
            self.gate.wait();
        }
        self.inner.begin_write()
    }
}
fn controlled(pause_read: bool) -> (Database, Arc<Gate>, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered, rx) = mpsc::channel();
    let (tx, resume) = mpsc::channel();
    let gate = Arc::new(Gate {
        armed: AtomicBool::new(false),
        entered,
        resume: Mutex::new(resume),
    });
    let db = Database::open_with_backend(Box::new(ControlledBackend {
        inner: RedbBackend::open_in_memory().unwrap(),
        gate: gate.clone(),
        pause_read,
    }))
    .unwrap();
    (db, gate, rx, tx)
}
fn vector(x: f64, y: f64) -> Value {
    Value::Array(vec![Value::Float(x), Value::Float(y)])
}
fn wait(rx: &mpsc::Receiver<()>) {
    rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
}

#[test]
fn insert_uses_schema_from_its_write_transaction() {
    let (db, gate, entered, resume) = controlled(false);
    let writer = db.collection("docs").unwrap();
    gate.armed.store(true, Ordering::SeqCst);
    let writing = std::thread::spawn(move || {
        writer
            .insert(vec![("title".into(), Value::Str("new".into()))])
            .unwrap()
    });
    wait(&entered);
    let col = db.collection("docs").unwrap();
    col.create_index("title").unwrap();
    resume.send(()).unwrap();
    let id = writing.join().unwrap();
    let found = col
        .find(Filter::Eq("title".into(), Value::Str("new".into())))
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, id);
}

#[test]
fn vector_score_and_body_share_a_snapshot_even_when_cache_is_warm() {
    let (db, gate, entered, resume) = controlled(true);
    let col = db.collection("docs").unwrap();
    col.create_vector_index("v", 2, None, None).unwrap();
    let id = col.insert(vec![("v".into(), vector(1.0, 0.0))]).unwrap();
    col.find_nearest("v", &[1.0, 0.0], 1, None).unwrap();
    gate.armed.store(true, Ordering::SeqCst);
    let reader = db.collection("docs").unwrap();
    let reading =
        std::thread::spawn(move || reader.find_nearest("v", &[1.0, 0.0], 1, None).unwrap());
    wait(&entered);
    col.update_one(
        Filter::Eq("_id".into(), Value::Str(id.to_string())),
        Update::Set(vec![("v".into(), vector(0.0, 1.0))]),
    )
    .unwrap();
    resume.send(()).unwrap();
    let old = reading.join().unwrap();
    assert!(old[0].score > 0.99);
    assert_eq!(old[0].document.get("v"), Some(&vector(1.0, 0.0)));
    let current = col.find_nearest("v", &[1.0, 0.0], 1, None).unwrap();
    assert!(current[0].score < 0.01);
    assert_eq!(current[0].document.get("v"), Some(&vector(0.0, 1.0)));
}

#[test]
fn cached_index_plan_is_compatible_with_the_read_snapshot() {
    let (db, gate, entered, resume) = controlled(true);
    let col = db.collection("docs").unwrap();
    col.insert(vec![("title".into(), Value::Str("present".into()))])
        .unwrap();
    col.create_index("title").unwrap();
    col.find(Filter::All).unwrap(); // cache indexed schema
    gate.armed.store(true, Ordering::SeqCst);
    let reader = db.collection("docs").unwrap();
    let reading = std::thread::spawn(move || {
        reader
            .find(Filter::Eq("title".into(), Value::Str("present".into())))
            .unwrap()
    });
    wait(&entered);
    col.drop_index("title").unwrap();
    resume.send(()).unwrap();
    assert_eq!(reading.join().unwrap().len(), 1);
    assert_eq!(
        col.find(Filter::Eq("title".into(), Value::Str("present".into())))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn supplied_id_insertion_cannot_overwrite_existing_document() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    let id = col
        .insert(vec![("title".into(), Value::Str("old".into()))])
        .unwrap();
    col.create_index("title").unwrap();
    assert!(
        col.insert_with_id(Document::with_id(
            id,
            vec![("title".into(), Value::Str("new".into()))]
        ))
        .is_err()
    );
    assert_eq!(
        col.find(Filter::Eq("title".into(), Value::Str("old".into())))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn delete_by_id_cleans_up_the_latest_index_entries() {
    let (db, gate, entered, resume) = controlled(false);
    let col = db.collection("docs").unwrap();
    let id = col
        .insert(vec![("title".into(), Value::Str("old".into()))])
        .unwrap();
    col.create_index("title").unwrap();
    gate.armed.store(true, Ordering::SeqCst);
    let deleter = db.collection("docs").unwrap();
    let deleting = std::thread::spawn(move || deleter.delete_by_id(id).unwrap());
    wait(&entered);
    col.update_one(
        Filter::Eq("_id".into(), Value::Str(id.to_string())),
        Update::Set(vec![("title".into(), Value::Str("orphaned-title".into()))]),
    )
    .unwrap();
    resume.send(()).unwrap();
    assert!(deleting.join().unwrap());
    assert!(
        !db.export_snapshot()
            .unwrap()
            .windows(14)
            .any(|w| w == b"orphaned-title")
    );
}

#[test]
fn stale_hnsw_falls_back_to_current_exact_results() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.insert(vec![("v".into(), vector(0.0, 1.0))]).unwrap();
    col.create_vector_index("v", 2, None, Some(taladb_core::HnswOptions::default()))
        .unwrap();
    let id = col.insert(vec![("v".into(), vector(1.0, 0.0))]).unwrap();
    assert_eq!(
        col.find_nearest("v", &[1.0, 0.0], 1, None).unwrap()[0]
            .document
            .id,
        id
    );
}

#[test]
fn snapshot_budget_rejects_overflow_and_preserves_valid_exports() {
    let db = Database::open_in_memory().unwrap();
    db.collection("docs")
        .unwrap()
        .insert(vec![("body".into(), Value::Str("x".repeat(8192)))])
        .unwrap();
    let snapshot = db.export_snapshot().unwrap();
    assert!(db.export_snapshot_with_limit(snapshot.len() - 1).is_err());
    assert_eq!(
        db.export_snapshot_with_limit(snapshot.len()).unwrap(),
        snapshot
    );
}

#[test]
fn invalid_vector_components_roll_back_the_document_write() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.create_vector_index("v", 2, None, None).unwrap();
    for bad in [f64::NAN, f64::INFINITY, f64::MAX] {
        assert!(col.insert(vec![("v".into(), vector(bad, 0.0))]).is_err());
    }
    assert_eq!(col.count(Filter::All).unwrap(), 0);
}

#[test]
fn invalid_hnsw_connectivity_is_rejected() {
    use taladb_core::vector::HnswOptions;
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    assert!(
        col.create_vector_index(
            "v",
            2,
            None,
            Some(HnswOptions {
                m: 1,
                ef_construction: 200
            })
        )
        .is_err()
    );
    assert!(col.list_indexes().unwrap().vector.is_empty());
}
