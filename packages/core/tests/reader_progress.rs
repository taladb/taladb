//! v0.13.2 M3 / R3: readers must not wait behind a writer that is itself
//! waiting for redb's single write slot.
//!
//! `RedbBackend` used to keep its `redb::Database` behind a `Mutex` and hold
//! that mutex across redb's *blocking* `begin_write`. A second writer queued
//! behind an active one therefore sat on the mutex, and every `begin_read` —
//! which needs the same mutex only to start an MVCC snapshot — waited for the
//! first writer to commit. M1 measured reader latency going from 119 µs to
//! 1.3 s; on Node a synchronous `find` stalls the event loop for that long.
//!
//! Each test parks writer A inside an open write transaction, starts writer B
//! (which queues in redb behind A), and then requires reads to complete while
//! A is still open. A is only released after the reads are judged, so on the
//! broken engine the reads cannot finish and the bounded wait fails the test
//! instead of hanging it. On the fixed engine the result does not depend on
//! timing at all; the short grace period only gives B time to reach redb so
//! that the *broken* engine reliably shows the stall.
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

use taladb::{Database, Filter, Value};

/// How long a read may take while a writer is queued. Far above an
/// uncontended read (well under a millisecond) and far below "blocked until
/// the active writer commits", which here is never.
const READ_DEADLINE: Duration = Duration::from_secs(10);
/// Time for writer B to reach redb's write-slot wait after it signals.
const QUEUE_GRACE: Duration = Duration::from_millis(150);

fn seeded(path: Option<&std::path::Path>) -> Arc<Database> {
    let db = match path {
        Some(p) => Database::open(p).unwrap(),
        None => Database::open_in_memory().unwrap(),
    };
    let col = db.collection("c").unwrap();
    col.create_index("x").unwrap();
    for i in 0..100 {
        col.insert(vec![("x".into(), Value::Int(i))]).unwrap();
    }
    Arc::new(db)
}

/// Writer A holds a write transaction until `release` fires; writer B is
/// queued behind it. Returns once B has been given time to queue.
struct Contention {
    release: mpsc::Sender<()>,
    a: std::thread::JoinHandle<()>,
    b: std::thread::JoinHandle<()>,
}

impl Contention {
    fn start(db: &Arc<Database>) -> Self {
        let (a_open_tx, a_open) = mpsc::channel();
        let (release, a_release) = mpsc::channel::<()>();
        let dba = Arc::clone(db);
        let a = std::thread::spawn(move || {
            let mut w = dba.backend().begin_write().unwrap();
            w.put("a_marker", b"k", b"v").unwrap();
            a_open_tx.send(()).unwrap();
            a_release.recv().unwrap();
            w.commit().unwrap();
        });
        a_open.recv().unwrap();

        let (b_go_tx, b_go) = mpsc::channel();
        let dbb = Arc::clone(db);
        let b = std::thread::spawn(move || {
            b_go_tx.send(()).unwrap();
            dbb.collection("c")
                .unwrap()
                .insert(vec![("x".into(), Value::Int(-1))])
                .unwrap();
        });
        b_go.recv().unwrap();
        std::thread::sleep(QUEUE_GRACE);
        Self { release, a, b }
    }

    fn finish(self) {
        self.release.send(()).unwrap();
        self.a.join().unwrap();
        self.b.join().unwrap();
    }
}

/// Run `read` on another thread and report whether it finished in time.
fn completes_in_time(db: &Arc<Database>, read: impl FnOnce(&Database) + Send + 'static) -> bool {
    let (done_tx, done) = mpsc::channel();
    let db = Arc::clone(db);
    std::thread::spawn(move || {
        read(&db);
        let _ = done_tx.send(());
    });
    done.recv_timeout(READ_DEADLINE).is_ok()
}

fn check_reads_progress(db: &Arc<Database>) {
    let contention = Contention::start(db);
    let results = [
        completes_in_time(db, |db| {
            db.collection("c").unwrap().count(Filter::All).unwrap();
        }),
        completes_in_time(db, |db| {
            let n = db
                .collection("c")
                .unwrap()
                .find(Filter::Eq("x".into(), Value::Int(7)))
                .unwrap();
            assert_eq!(n.len(), 1);
        }),
        completes_in_time(db, |db| {
            // The writers' uncommitted work is invisible: MVCC snapshot.
            let r = db.backend().begin_read().unwrap();
            assert!(r.get("a_marker", b"k").unwrap().is_none());
        }),
    ];
    // Release the writers before asserting, so a failure cannot leave the
    // test process wedged on them.
    contention.finish();
    assert_eq!(
        results, [true; 3],
        "reads must progress while a writer waits for redb's write slot"
    );
    // Both writers committed once released.
    let col = db.collection("c").unwrap();
    assert_eq!(col.count(Filter::All).unwrap(), 101);
    let r = db.backend().begin_read().unwrap();
    assert!(r.get("a_marker", b"k").unwrap().is_some());
}

#[test]
fn reads_progress_while_a_writer_is_queued_in_memory() {
    check_reads_progress(&seeded(None));
}

#[test]
fn reads_progress_while_a_writer_is_queued_on_a_file() {
    let dir = tempfile::tempdir().unwrap();
    check_reads_progress(&seeded(Some(&dir.path().join("r3.db"))));
}

/// Many readers and writers at once: every write lands exactly once and
/// readers always see a consistent count (monotonic, never past the total).
#[test]
fn concurrent_readers_and_writers_stay_consistent() {
    const WRITERS: usize = 4;
    const PER_WRITER: i64 = 100;
    const READERS: usize = 4;
    let db = seeded(None);
    let start = Arc::new(Barrier::new(WRITERS + READERS));
    let mut handles = Vec::new();
    for w in 0..WRITERS {
        let db = Arc::clone(&db);
        let start = Arc::clone(&start);
        handles.push(std::thread::spawn(move || {
            start.wait();
            let col = db.collection("c").unwrap();
            for i in 0..PER_WRITER {
                col.insert(vec![
                    ("x".into(), Value::Int(1000 + i)),
                    ("w".into(), Value::Int(w as i64)),
                ])
                .unwrap();
            }
        }));
    }
    let total = 100 + WRITERS as u64 * PER_WRITER as u64;
    for _ in 0..READERS {
        let db = Arc::clone(&db);
        let start = Arc::clone(&start);
        handles.push(std::thread::spawn(move || {
            start.wait();
            let col = db.collection("c").unwrap();
            let mut last = 0;
            for _ in 0..200 {
                let n = col.count(Filter::All).unwrap();
                assert!(n >= last && n <= total, "count went {last} -> {n}");
                last = n;
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(
        db.collection("c").unwrap().count(Filter::All).unwrap(),
        total
    );
}

/// `compact` needs exclusive access to `redb::Database`. With readers and
/// writers running it must neither deadlock nor corrupt anything: it either
/// succeeds or reports that a transaction is in progress, readers and writers
/// keep making progress between compactions, and all data is intact
/// afterwards.
#[test]
fn compact_under_concurrent_reads_and_writes_never_deadlocks() {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
    let dir = tempfile::tempdir().unwrap();
    let db = seeded(Some(&dir.path().join("compact.db")));
    let stop = Arc::new(AtomicBool::new(false));
    let inserts = Arc::new(AtomicU64::new(0));
    let reads = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    for t in 0..3 {
        let (db, stop) = (Arc::clone(&db), Arc::clone(&stop));
        let (inserts, reads) = (Arc::clone(&inserts), Arc::clone(&reads));
        handles.push(std::thread::spawn(move || {
            let col = db.collection("c").unwrap();
            while !stop.load(SeqCst) {
                if t == 0 {
                    col.insert(vec![("x".into(), Value::Int(5000))]).unwrap();
                    inserts.fetch_add(1, SeqCst);
                } else {
                    col.count(Filter::Eq("x".into(), Value::Int(5000))).unwrap();
                    reads.fetch_add(1, SeqCst);
                }
            }
        }));
    }
    let (done_tx, done) = mpsc::channel();
    {
        let (db, inserts, reads) = (Arc::clone(&db), Arc::clone(&inserts), Arc::clone(&reads));
        std::thread::spawn(move || {
            let mut outcomes = Vec::new();
            for _ in 0..10 {
                // Each compaction starts only once both readers and the writer
                // have made progress since the previous one.
                let (i0, r0) = (inserts.load(SeqCst), reads.load(SeqCst));
                while inserts.load(SeqCst) < i0 + 5 || reads.load(SeqCst) < r0 + 5 {
                    std::thread::yield_now();
                }
                outcomes.push(db.compact().map_err(|e| e.to_string()));
            }
            let _ = done_tx.send(outcomes);
        });
    }
    let outcomes = done.recv_timeout(Duration::from_secs(120));
    stop.store(true, SeqCst);
    for h in handles {
        h.join().unwrap();
    }
    let outcomes = outcomes.expect("compact must not deadlock with live readers/writers");
    let ok = outcomes.iter().filter(|o| o.is_ok()).count();
    let inserted = inserts.load(SeqCst);
    eprintln!(
        "compact under load: {ok} ok, {} busy, {inserted} inserts, {} reads",
        outcomes.len() - ok,
        reads.load(SeqCst)
    );
    for o in &outcomes {
        if let Err(e) = o {
            assert!(
                e.contains("transaction") || e.contains("Transaction"),
                "unexpected compact error: {e}"
            );
        }
    }
    assert!(inserted >= 50, "writers progressed between compactions");
    let col = db.collection("c").unwrap();
    assert_eq!(col.count(Filter::All).unwrap(), 100 + inserted);
    assert_eq!(
        col.count(Filter::Eq("x".into(), Value::Int(5000))).unwrap(),
        inserted
    );
    // A quiescent compact still works.
    db.compact().unwrap();
}
