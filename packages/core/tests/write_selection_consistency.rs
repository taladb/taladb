//! v0.13.2 M3 / R2: `update_one`, `update_many`, `delete_one` and
//! `delete_many` must choose their targets from the same transaction that
//! mutates them.
//!
//! Before the fix they planned and collected candidates in a read transaction,
//! dropped it, and only then began the write transaction. A matching document
//! committed by another writer in that gap was silently skipped (a phantom),
//! and `update_one`/`delete_one` returned `false` while a match existed.
//!
//! The race is made deterministic with a backend whose next `begin_write`
//! first commits a concurrent mutation through a separate handle on the same
//! storage — that is exactly "another writer committed between selection and
//! the write transaction". No sleeps, no timing.
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use taladb::engine::{ReadTxn, WriteTxn};
use taladb::{
    Collection, Database, Filter, RedbBackend, StorageBackend, TalaDbError, Update, Value,
};

type Injection = Box<dyn FnOnce(&Collection) -> Result<(), TalaDbError> + Send>;

/// Runs `injection` (once) at the start of the next `begin_write`, through a
/// separate `Collection` on the same storage, then opens the real write txn.
struct Inject {
    inner: Arc<dyn StorageBackend>,
    armed: AtomicBool,
    injection: Mutex<Option<Injection>>,
}

impl StorageBackend for Inject {
    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        self.inner.begin_read()
    }
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        if self.armed.swap(false, Ordering::SeqCst) {
            let f = self.injection.lock().unwrap().take();
            if let Some(f) = f {
                f(&Collection::new("items", Arc::clone(&self.inner)))?;
            }
        }
        self.inner.begin_write()
    }
}

struct Shared(Arc<Inject>);

impl StorageBackend for Shared {
    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        self.0.begin_read()
    }
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        self.0.begin_write()
    }
}

struct Fixture {
    db: Database,
    inject: Arc<Inject>,
}

impl Fixture {
    /// `pending` documents with `status = "pending"` and one `"other"`.
    fn new(indexed: bool, pending: usize) -> Self {
        let inner: Arc<dyn StorageBackend> = Arc::new(RedbBackend::open_in_memory().unwrap());
        let inject = Arc::new(Inject {
            inner,
            armed: AtomicBool::new(false),
            injection: Mutex::new(None),
        });
        let db = Database::open_with_backend(Box::new(Shared(Arc::clone(&inject)))).unwrap();
        let col = db.collection("items").unwrap();
        if indexed {
            col.create_index("status").unwrap();
        }
        let mut docs: Vec<_> = (0..pending).map(|_| status("pending")).collect();
        docs.push(status("other"));
        col.insert_many(docs).unwrap();
        Self { db, inject }
    }

    fn col(&self) -> Collection {
        self.db.collection("items").unwrap()
    }

    /// Commit `f` immediately before the next write transaction opens.
    fn before_next_write(
        &self,
        f: impl FnOnce(&Collection) -> Result<(), TalaDbError> + Send + 'static,
    ) {
        *self.inject.injection.lock().unwrap() = Some(Box::new(f));
        self.inject.armed.store(true, Ordering::SeqCst);
    }

    fn injection_ran(&self) -> bool {
        !self.inject.armed.load(Ordering::SeqCst)
    }

    fn count(&self, s: &str) -> u64 {
        self.col().count(is(s)).unwrap()
    }
}

fn status(s: &str) -> Vec<(String, Value)> {
    vec![("status".into(), Value::Str(s.into()))]
}

fn is(s: &str) -> Filter {
    Filter::Eq("status".into(), Value::Str(s.into()))
}

fn set_done() -> Update {
    Update::Set(vec![("status".into(), Value::Str("done".into()))])
}

#[test]
fn update_many_includes_a_match_committed_before_its_write_txn() {
    for indexed in [false, true] {
        let fx = Fixture::new(indexed, 3);
        fx.before_next_write(|c| c.insert(status("pending")).map(|_| ()));
        let n = fx.col().update_many(is("pending"), set_done()).unwrap();
        assert!(fx.injection_ran());
        assert_eq!(n, 4, "indexed={indexed}: the concurrent match is updated");
        assert_eq!(fx.count("pending"), 0, "indexed={indexed}: no phantom left");
        assert_eq!(fx.count("done"), 4, "indexed={indexed}");
        assert_eq!(
            fx.count("other"),
            1,
            "indexed={indexed}: non-match untouched"
        );
    }
}

#[test]
fn delete_many_includes_a_match_committed_before_its_write_txn() {
    for indexed in [false, true] {
        let fx = Fixture::new(indexed, 3);
        fx.before_next_write(|c| c.insert(status("pending")).map(|_| ()));
        let n = fx.col().delete_many(is("pending")).unwrap();
        assert!(fx.injection_ran());
        assert_eq!(n, 4, "indexed={indexed}");
        assert_eq!(fx.count("pending"), 0, "indexed={indexed}: no phantom left");
        assert_eq!(fx.col().count(Filter::All).unwrap(), 1, "indexed={indexed}");
    }
}

/// With no match in the (old) read snapshot, `update_one` used to return
/// `false` without ever opening a write transaction, although a match had
/// been committed by then.
#[test]
fn update_one_finds_a_match_committed_before_its_write_txn() {
    for indexed in [false, true] {
        let fx = Fixture::new(indexed, 0);
        fx.before_next_write(|c| c.insert(status("pending")).map(|_| ()));
        let updated = fx.col().update_one(is("pending"), set_done()).unwrap();
        assert!(
            fx.injection_ran(),
            "indexed={indexed}: a write txn was opened"
        );
        assert!(updated, "indexed={indexed}: a match existed when it ran");
        assert_eq!(fx.count("done"), 1, "indexed={indexed}");
        assert_eq!(fx.count("pending"), 0, "indexed={indexed}");
    }
}

#[test]
fn delete_one_finds_a_match_committed_before_its_write_txn() {
    for indexed in [false, true] {
        let fx = Fixture::new(indexed, 0);
        fx.before_next_write(|c| c.insert(status("pending")).map(|_| ()));
        let deleted = fx.col().delete_one(is("pending")).unwrap();
        assert!(fx.injection_ran(), "indexed={indexed}");
        assert!(deleted, "indexed={indexed}");
        assert_eq!(fx.count("pending"), 0, "indexed={indexed}");
    }
}

/// A concurrent update that makes a non-matching document match is a phantom
/// too: it must be included.
#[test]
fn update_many_includes_a_document_updated_into_the_filter() {
    for indexed in [false, true] {
        let fx = Fixture::new(indexed, 2);
        fx.before_next_write(|c| {
            c.update_many(is("other"), Update::Set(status("pending")))
                .map(|_| ())
        });
        let n = fx.col().update_many(is("pending"), set_done()).unwrap();
        assert_eq!(n, 3, "indexed={indexed}");
        assert_eq!(fx.count("done"), 3, "indexed={indexed}");
        assert_eq!(fx.count("other"), 0, "indexed={indexed}");
    }
}

/// The re-check semantics that already held must still hold: a candidate
/// deleted, or changed out of the filter, by a concurrent writer is neither
/// counted nor mutated.
#[test]
fn concurrent_delete_and_unmatching_update_are_excluded() {
    for indexed in [false, true] {
        let fx = Fixture::new(indexed, 4);
        fx.before_next_write(|c| {
            c.delete_one(is("pending"))?;
            c.update_one(is("pending"), Update::Set(status("other")))?;
            Ok(())
        });
        let n = fx.col().update_many(is("pending"), set_done()).unwrap();
        assert_eq!(n, 2, "indexed={indexed}: 4 - 1 deleted - 1 moved out");
        assert_eq!(fx.count("done"), 2, "indexed={indexed}");
        assert_eq!(fx.count("other"), 2, "indexed={indexed}");
        assert_eq!(fx.col().count(Filter::All).unwrap(), 4, "indexed={indexed}");
    }
}

/// Index maintenance stays exact across the new ordering: every document is
/// reachable through the index under its new value and none under its old.
#[test]
fn index_entries_follow_the_mutations() {
    let fx = Fixture::new(true, 3);
    fx.before_next_write(|c| c.insert(status("pending")).map(|_| ()));
    fx.col().update_many(is("pending"), set_done()).unwrap();
    let col = fx.col();
    assert_eq!(col.find(is("done")).unwrap().len(), 4);
    assert!(col.find(is("pending")).unwrap().is_empty());
    fx.before_next_write(|c| c.insert(status("done")).map(|_| ()));
    assert_eq!(col.delete_many(is("done")).unwrap(), 5);
    assert!(col.find(is("done")).unwrap().is_empty());
    assert_eq!(col.find(is("other")).unwrap().len(), 1);
}

/// Real threads, no injection: writers insert `pending` documents while a
/// sweeper repeatedly runs `update_many(pending -> done)`. Every insert lands
/// in exactly one sweep or after the last, and the per-sweep counts add up.
#[test]
fn concurrent_sweeps_account_for_every_insert() {
    const WRITERS: usize = 4;
    const PER_WRITER: usize = 200;
    let db = Arc::new(Database::open_in_memory().unwrap());
    db.collection("items")
        .unwrap()
        .create_index("status")
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let sweeper = {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let col = db.collection("items").unwrap();
            let mut swept = 0u64;
            while !stop.load(Ordering::SeqCst) {
                swept += col.update_many(is("pending"), set_done()).unwrap();
            }
            swept
        })
    };
    let writers: Vec<_> = (0..WRITERS)
        .map(|_| {
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                let col = db.collection("items").unwrap();
                for _ in 0..PER_WRITER {
                    col.insert(status("pending")).unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    let col = db.collection("items").unwrap();
    let swept = sweeper.join().unwrap() + col.update_many(is("pending"), set_done()).unwrap();
    let total = (WRITERS * PER_WRITER) as u64;
    assert_eq!(swept, total, "each insert is swept exactly once");
    assert_eq!(col.count(is("done")).unwrap(), total);
    assert_eq!(col.count(is("pending")).unwrap(), 0);
}
