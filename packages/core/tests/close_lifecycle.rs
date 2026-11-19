//! v0.13.2 M3 / R4: closing a database releases its storage and file lock,
//! even while `Collection` and `WatchHandle` objects from it are still alive.
//!
//! Every `Collection` (and every watch's re-query closure) holds the backend
//! through an `Arc`, so dropping the `Database` — which is all the Node
//! `close()` and FFI `taladb_close` did — left the redb file open and locked:
//! reopening the path failed with "Database already open", a watch never
//! reported `WatchClosed`, and a blocking `next()` waited forever.
//!
//! `Database::close` now defines the behaviour:
//! - the redb database is dropped at once, so the file lock goes as soon as
//!   transactions already running on other threads finish;
//! - retained handles (and clones of the `Database`) fail every operation with
//!   `InvalidOperation("database is closed")` instead of touching storage;
//! - every watch from it reports `WatchClosed` without waiting for a write,
//!   and a watch created afterwards is closed from the start.
use std::sync::mpsc;
use std::time::Duration;

use taladb::{Database, Filter, TalaDbError, Value};

fn x(i: i64) -> Vec<(String, Value)> {
    vec![("x".into(), Value::Int(i))]
}

fn is_closed_error<T: std::fmt::Debug>(r: Result<T, TalaDbError>) -> bool {
    matches!(r, Err(TalaDbError::InvalidOperation(ref m)) if m.contains("closed"))
}

#[test]
fn close_releases_the_file_lock_while_a_collection_is_retained() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.db");
    let db = Database::open(&path).unwrap();
    let col = db.collection("c").unwrap();
    col.insert(x(1)).unwrap();

    db.close().unwrap();
    let reopened = Database::open(&path).expect("lock released by close");
    assert_eq!(
        reopened
            .collection("c")
            .unwrap()
            .count(Filter::All)
            .unwrap(),
        1,
        "the committed write survives close"
    );

    // The retained handle refuses to touch storage — it must not silently
    // read or write the *reopened* database either.
    assert!(is_closed_error(col.count(Filter::All)));
    assert!(is_closed_error(col.find(Filter::All)));
    assert!(is_closed_error(col.insert(x(2))));
    assert!(is_closed_error(col.delete_many(Filter::All)));
    assert_eq!(
        reopened
            .collection("c")
            .unwrap()
            .count(Filter::All)
            .unwrap(),
        1
    );
}

#[test]
fn watches_report_closed_instead_of_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.db");
    let db = Database::open(&path).unwrap();
    let col = db.collection("c").unwrap();
    let polled = col.watch(Filter::All);
    let blocked = col.watch(Filter::All);

    // A blocking `next()` already waiting when close happens must wake up.
    let (tx, rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        tx.send(blocked.next().map(|d| d.len())).unwrap();
    });
    std::thread::sleep(Duration::from_millis(50));
    db.close().unwrap();
    let woke = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("a waiting next() must return once the database closes");
    assert!(matches!(woke, Err(TalaDbError::WatchClosed)), "{woke:?}");
    waiter.join().unwrap();

    assert!(matches!(
        polled.next_timeout(Duration::from_millis(10)),
        Err(TalaDbError::WatchClosed)
    ));
    assert!(matches!(polled.try_next(), Err(TalaDbError::WatchClosed)));

    // The lock is gone while both the watch and the collection are alive.
    let reopened = Database::open(&path).expect("watches do not hold the lock");
    // A write through the new database does not wake the old watch.
    reopened.collection("c").unwrap().insert(x(1)).unwrap();
    assert!(matches!(polled.try_next(), Err(TalaDbError::WatchClosed)));

    // Subscribing on a retained handle after close is closed from the start,
    // and so is a collection first named after close.
    let late = col.watch(Filter::All);
    assert!(matches!(
        late.next_timeout(Duration::from_secs(5)),
        Err(TalaDbError::WatchClosed)
    ));
    let fresh = db.collection("never_seen").unwrap().watch(Filter::All);
    assert!(matches!(
        fresh.next_timeout(Duration::from_secs(5)),
        Err(TalaDbError::WatchClosed)
    ));
}

/// A wake-up already queued when the database closes is not served: the
/// watch reports `WatchClosed`, not a query error.
#[test]
fn a_wake_up_queued_before_close_reports_closed() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("c").unwrap();
    let watch = col.watch(Filter::All);
    col.insert(x(1)).unwrap();
    db.close().unwrap();
    assert!(matches!(watch.try_next(), Err(TalaDbError::WatchClosed)));
    assert!(matches!(watch.try_next(), Err(TalaDbError::WatchClosed)));
}

#[test]
fn close_is_idempotent_and_every_entry_point_reports_closed() {
    let db = Database::open_in_memory().unwrap();
    let clone = db.clone();
    db.collection("c").unwrap().insert(x(1)).unwrap();
    db.close().unwrap();
    db.close().unwrap();
    clone.close().unwrap();

    // Clones share the backend, so they are closed too.
    assert!(is_closed_error(
        clone.collection("c").unwrap().count(Filter::All)
    ));
    assert!(is_closed_error(db.list_collection_names()));
    assert!(is_closed_error(db.user_version()));
    assert!(is_closed_error(db.set_user_version(1)));
    assert!(is_closed_error(db.export_snapshot()));
    assert!(is_closed_error(db.compact()));
    assert!(is_closed_error(db.flush()));
    assert!(is_closed_error(db.backend().begin_read().map(|_| ())));
    assert!(is_closed_error(db.backend().begin_write().map(|_| ())));
}

/// A write transaction already running on another thread when `close` is
/// called finishes and commits normally (redb defers its own close until the
/// writer ends); the lock is released when it does.
#[test]
fn close_lets_an_in_flight_write_commit_then_releases_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.db");
    let db = Database::open(&path).unwrap();
    db.collection("c").unwrap().insert(x(1)).unwrap();

    let (open_tx, open_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let writer_db = db.clone();
    let writer = std::thread::spawn(move || {
        let mut w = writer_db.backend().begin_write().unwrap();
        w.put("side", b"k", b"v").unwrap();
        open_tx.send(()).unwrap();
        go_rx.recv().unwrap();
        w.commit()
    });
    open_rx.recv().unwrap();
    db.close().unwrap();
    assert!(
        Database::open(&path).is_err(),
        "the in-flight writer still holds the file"
    );
    go_tx.send(()).unwrap();
    writer.join().unwrap().expect("the in-flight write commits");

    let reopened = Database::open(&path).expect("lock released once it ends");
    let r = reopened.backend().begin_read().unwrap();
    assert_eq!(r.get("side", b"k").unwrap(), Some(b"v".to_vec()));
}

#[test]
fn repeated_open_close_with_retained_handles_never_leaks_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("d.db");
    let mut retained = Vec::new();
    for i in 0..50 {
        let db = Database::open(&path).unwrap();
        let col = db.collection("c").unwrap();
        col.insert(x(i)).unwrap();
        let watch = col.watch(Filter::All);
        retained.push((col, watch));
        db.close().unwrap();
    }
    let db = Database::open(&path).unwrap();
    assert_eq!(db.collection("c").unwrap().count(Filter::All).unwrap(), 50);
    for (col, watch) in &retained {
        assert!(is_closed_error(col.count(Filter::All)));
        assert!(matches!(watch.try_next(), Err(TalaDbError::WatchClosed)));
    }
}

/// Dropping without `close` keeps its old meaning: a retained handle keeps
/// the database usable (and open) until it is dropped too.
#[test]
fn drop_without_close_is_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("e.db");
    let db = Database::open(&path).unwrap();
    let col = db.collection("c").unwrap();
    drop(db);
    col.insert(x(1)).unwrap();
    assert_eq!(col.count(Filter::All).unwrap(), 1);
    assert!(Database::open(&path).is_err());
    drop(col);
    assert!(Database::open(&path).is_ok());
}

#[cfg(feature = "encryption")]
#[test]
fn closing_an_encrypted_database_releases_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f.db");
    let db = Database::open_encrypted(&path, "correct horse").unwrap();
    let col = db.collection("c").unwrap();
    col.insert(x(1)).unwrap();
    db.close().unwrap();
    assert!(is_closed_error(col.count(Filter::All)));
    let reopened = Database::open_encrypted(&path, "correct horse").unwrap();
    assert_eq!(
        reopened
            .collection("c")
            .unwrap()
            .count(Filter::All)
            .unwrap(),
        1
    );
}
