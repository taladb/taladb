//! Filtered full scans decode a projection first and the whole document only
//! for a match. Every query here is checked against the full scan it replaced:
//! decode every stored document whole, in key order, and test it.
//!
//! Results must be the same documents (ids, order and every field value,
//! compared as encoded bytes so NaN compares too) and the same errors.

use std::ops::Bound;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use taladb::engine::{ReadTxn, ScanFlow, WriteTxn};
use taladb::json::filter_from_json;
use taladb::query::{FindOptions, SortSpec};
use taladb::{
    Collection, Database, Document, Filter, RedbBackend, StorageBackend, TalaDbError, Ulid, Update,
    Value,
};

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

fn id(i: usize) -> String {
    Ulid::from(i as u128 + 1).to_string()
}

fn encoded(docs: &[Document]) -> Vec<Vec<u8>> {
    docs.iter()
        .map(|d| postcard::to_allocvec(d).unwrap())
        .collect()
}

/// A backend whose raw storage the test can also write, to plant corrupt
/// records next to valid ones.
#[derive(Clone)]
struct Shared(Arc<RedbBackend>);
impl StorageBackend for Shared {
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        self.0.begin_write()
    }
    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        self.0.begin_read()
    }
}

/// The stored records of a collection, in scan order.
fn raw(backend: &RedbBackend, collection: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    let txn = backend.begin_read().unwrap();
    let mut out = Vec::new();
    txn.scan(
        &format!("docs::{collection}"),
        Bound::Unbounded,
        Bound::Unbounded,
        &mut |k, v| {
            out.push((k.to_vec(), v.to_vec()));
            Ok(ScanFlow::Continue)
        },
    )
    .unwrap();
    out
}

/// The full scan before projection, from the stored bytes: decode each
/// record whole in key order, stop at the first error, test, stop at `limit`.
fn reference_scan(
    records: &[(Vec<u8>, Vec<u8>)],
    filter: &Filter,
    limit: Option<usize>,
) -> Result<Vec<Document>, String> {
    let mut out = Vec::new();
    for (_, bytes) in records {
        if limit.is_some_and(|n| out.len() >= n) {
            break;
        }
        let doc: Document =
            postcard::from_bytes(bytes).map_err(|e| TalaDbError::from(e).to_string())?;
        if filter.matches(&doc).map_err(|e| e.to_string())? {
            out.push(doc);
        }
    }
    Ok(out)
}

const SORT: [(&str, bool); 2] = [("n", false), ("i", true)];

fn sort() -> Vec<SortSpec> {
    SORT.iter()
        .map(|&(f, asc)| {
            if asc {
                SortSpec::asc(f)
            } else {
                SortSpec::desc(f)
            }
        })
        .collect()
}

/// The full-decode oracle for one collection: every document in scan order
/// (`find(All)`, which keeps the direct whole-document loop) and in [`SORT`]
/// order. Filters are applied with `Filter::matches`, which shares the
/// scan's evaluator.
struct Oracle {
    all: Vec<Document>,
    sorted: Vec<Document>,
}

impl Oracle {
    fn of(col: &Collection) -> Self {
        Self {
            all: col.find(Filter::All).unwrap(),
            sorted: col
                .find_with_options(
                    Filter::All,
                    FindOptions {
                        sort: sort(),
                        ..Default::default()
                    },
                )
                .unwrap(),
        }
    }
}

/// Check `find`, `find_one`, `count`, a page, and a sorted page against the
/// oracle.
fn check(col: &Collection, oracle: &Oracle, filter: &Filter) {
    let expected: Vec<Document> = oracle
        .all
        .iter()
        .filter(|d| filter.matches(d).unwrap())
        .cloned()
        .collect();
    let got = col.find(filter.clone()).unwrap();
    assert_eq!(encoded(&got), encoded(&expected), "find {filter:?}");
    assert_eq!(
        col.count(filter.clone()).unwrap(),
        expected.len() as u64,
        "count {filter:?}"
    );
    assert_eq!(
        encoded(
            &col.find_one(filter.clone())
                .unwrap()
                .into_iter()
                .collect::<Vec<_>>()
        ),
        encoded(&expected[..expected.len().min(1)]),
        "find_one {filter:?}"
    );
    let page = col
        .find_with_options(
            filter.clone(),
            FindOptions {
                skip: 3,
                limit: Some(5),
                ..Default::default()
            },
        )
        .unwrap();
    let window = &expected[expected.len().min(3)..expected.len().min(8)];
    assert_eq!(encoded(&page), encoded(window), "page {filter:?}");

    // Sorting is the existing comparator over the same candidates: sort the
    // whole collection, then keep the matches, in order.
    let sorted: Vec<Document> = oracle
        .sorted
        .iter()
        .filter(|d| filter.matches(d).unwrap())
        .skip(2)
        .take(7)
        .cloned()
        .collect();
    let got = col
        .find_with_options(
            filter.clone(),
            FindOptions {
                sort: sort(),
                skip: 2,
                limit: Some(7),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(encoded(&got), encoded(&sorted), "sorted page {filter:?}");
}

/// Varied documents: missing versus null, nested objects, arrays, mixed
/// numbers, NaN, signed zero, strings, bytes and large skipped payloads.
fn varied(i: usize) -> Vec<(String, Value)> {
    let mut fields = vec![
        ("_id".into(), s(&id(i))),
        ("i".into(), Value::Int(i as i64)),
    ];
    let n = match i % 9 {
        0 => Some(Value::Int(3)),
        1 => Some(Value::Float(3.0)),
        2 => Some(Value::Float(3.5)),
        3 => Some(Value::Float(f64::NAN)),
        4 => Some(Value::Float(-0.0)),
        5 => Some(Value::Int(0)),
        6 => Some(s("3")),
        7 => Some(Value::Null),
        _ => None,
    };
    if let Some(n) = n {
        fields.push(("n".into(), n));
    }
    if !i.is_multiple_of(5) {
        fields.push(("s".into(), s(["a", "a\0b", "ünï", "", "zz"][i % 5])));
    }
    if i.is_multiple_of(4) {
        fields.push(("b".into(), Value::Bytes(vec![(i % 3) as u8, 0xFF])));
    }
    fields.push((
        "tags".into(),
        match i % 6 {
            0 => Value::Array(vec![s("rust"), s("db")]),
            1 => s("rust"),
            2 => Value::Array(vec![Value::Array(vec![s("rust")]), Value::Int(1)]),
            3 => Value::Array(vec![]),
            4 => Value::Null,
            _ => Value::Array(vec![Value::Int(3), Value::Float(0.0)]),
        },
    ));
    if i % 7 != 3 {
        fields.push((
            "meta".into(),
            if i % 7 == 6 {
                Value::Int(1)
            } else {
                Value::Object(vec![
                    (
                        "tag".into(),
                        s(if i.is_multiple_of(2) { "yes" } else { "no" }),
                    ),
                    (
                        "depth".into(),
                        Value::Object(vec![("x".into(), Value::Int((i % 4) as i64))]),
                    ),
                ])
            },
        ));
    }
    fields.push((
        "body".into(),
        s(&format!("rust database storage token{}", i % 5)),
    ));
    fields.push(("flag".into(), Value::Bool(i.is_multiple_of(2))));
    fields.push((
        "payload".into(),
        s(&"x".repeat(if i.is_multiple_of(97) { 70_000 } else { 1_024 })),
    ));
    fields.push((
        "vec".into(),
        Value::Array((0..64).map(|j| Value::Float(f64::from(j))).collect()),
    ));
    fields
}

fn varied_filters() -> Vec<Filter> {
    let f = Value::Float;
    let mut out = Vec::new();
    for v in [
        Value::Int(3),
        f(3.0),
        f(3.5),
        f(f64::NAN),
        f(0.0),
        f(-0.0),
        s("3"),
        Value::Null,
    ] {
        out.extend([
            Filter::Eq("n".into(), v.clone()),
            Filter::Ne("n".into(), v.clone()),
            Filter::Gt("n".into(), v.clone()),
            Filter::Gte("n".into(), v.clone()),
            Filter::Lt("n".into(), v.clone()),
            Filter::Lte("n".into(), v),
        ]);
    }
    out.extend([
        Filter::Eq("s".into(), s("a")),
        Filter::Gt("s".into(), s("a")),
        Filter::Lte("s".into(), s("a\0b")),
        Filter::In("s".into(), vec![s(""), s("zz"), Value::Null]),
        Filter::Nin("n".into(), vec![Value::Int(3), f(f64::NAN)]),
        Filter::Exists("n".into(), true),
        Filter::Exists("n".into(), false),
        Filter::Eq("n".into(), Value::Null),
        Filter::Ne("n".into(), Value::Null),
        Filter::Exists("zz".into(), false),
        Filter::Eq("zz".into(), Value::Null),
        Filter::Eq("tags".into(), s("rust")),
        Filter::Eq("tags".into(), Value::Array(vec![s("rust"), s("db")])),
        Filter::Ne("tags".into(), s("rust")),
        Filter::In("tags".into(), vec![s("db"), Value::Int(3)]),
        Filter::Nin("tags".into(), vec![s("db")]),
        Filter::Eq("tags".into(), f(-0.0)),
        Filter::Eq("meta.tag".into(), s("yes")),
        Filter::Gte("meta.depth.x".into(), Value::Int(2)),
        Filter::Exists("meta.tag".into(), false),
        Filter::Exists("meta.depth.y".into(), false),
        Filter::Eq("b".into(), Value::Bytes(vec![1, 0xFF])),
        Filter::Ne("b".into(), Value::Bytes(vec![1, 0xFF])),
        Filter::Gt("b".into(), Value::Bytes(vec![0])),
        Filter::Eq("flag".into(), Value::Bool(true)),
        Filter::And(vec![
            Filter::Eq("flag".into(), Value::Bool(true)),
            Filter::Lt("i".into(), Value::Int(300)),
        ]),
        Filter::Or(vec![
            Filter::Eq("meta.tag".into(), s("no")),
            Filter::Exists("b".into(), true),
        ]),
        Filter::Not(Box::new(Filter::Exists("s".into(), true))),
        Filter::Not(Box::new(Filter::Or(vec![
            Filter::Eq("n".into(), Value::Int(3)),
            Filter::Eq("zz".into(), Value::Null),
        ]))),
        Filter::Not(Box::new(Filter::Eq("meta.depth.x".into(), Value::Int(1)))),
        Filter::Regex("s".into(), "^a".into()),
        Filter::Regex("tags".into(), "^ru".into()),
        Filter::Not(Box::new(Filter::Regex("meta.tag".into(), "y".into()))),
        Filter::Contains("body".into(), "rust token3".into()),
        Filter::Contains("body".into(), String::new()),
        Filter::Contains("zz".into(), "rust".into()),
        Filter::Ne("_id".into(), s(&id(5))),
        Filter::Nin("_id".into(), vec![s(&id(5)), s(&id(8))]),
        Filter::Exists("_id".into(), false),
        Filter::Eq("payload".into(), s(&"x".repeat(1_024))),
        Filter::Lt("i".into(), Value::Int(1)),
        Filter::Gte("i".into(), Value::Int(0)),
    ]);
    // The parser-parity fixture's valid filters, as the bindings build them.
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/parser_parity.json")).unwrap();
    for case in fixture["filters"].as_array().unwrap() {
        if case["valid"].as_bool().unwrap() {
            out.push(filter_from_json(&case["input"]).unwrap());
        }
    }
    out
}

#[test]
fn every_filter_shape_matches_the_full_scan() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("varied").unwrap();
    col.insert_many((0..600).map(varied).collect()).unwrap();
    let oracle = Oracle::of(&col);
    for filter in varied_filters() {
        check(&col, &oracle, &filter);
    }
}

/// Wide documents (a 1 KiB payload) and narrow ones, with matches spread
/// periodically, clustered at either end, or scattered, at 0% to 100%.
#[test]
fn selectivity_and_distribution_sweep_matches_the_full_scan() {
    const N: usize = 3_000;
    for wide in [true, false] {
        let db = Database::open_in_memory().unwrap();
        let col = db.collection("sweep").unwrap();
        col.insert_many(
            (0..N)
                .map(|i| {
                    let mut fields = vec![
                        ("_id".into(), s(&id(i))),
                        ("i".into(), Value::Int(i as i64)),
                        ("n".into(), Value::Int((i % 7) as i64)),
                        ("periodic".into(), Value::Int((i % 100) as i64)),
                        ("start".into(), Value::Int((i * 100 / N) as i64)),
                        ("end".into(), Value::Int(99 - (i * 100 / N) as i64)),
                        (
                            "scattered".into(),
                            Value::Int(((i as u64).wrapping_mul(0x9E37_79B9) % 100) as i64),
                        ),
                    ];
                    if wide {
                        fields.push(("payload".into(), s(&"x".repeat(1_024))));
                    }
                    fields
                })
                .collect(),
        )
        .unwrap();
        let oracle = Oracle::of(&col);
        for field in ["periodic", "start", "end", "scattered"] {
            for pct in [0, 1, 10, 50, 100] {
                check(&col, &oracle, &Filter::Lt(field.into(), Value::Int(pct)));
            }
        }
    }
}

#[test]
fn invalid_regex_is_an_error_never_an_empty_or_full_result() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("regex").unwrap();
    col.insert_many((0..100).map(varied).collect()).unwrap();
    for filter in [
        Filter::Regex("s".into(), "(".into()),
        Filter::Not(Box::new(Filter::Regex("s".into(), "(".into()))),
        Filter::And(vec![
            Filter::Eq("flag".into(), Value::Bool(true)),
            Filter::Not(Box::new(Filter::Regex("zz".into(), "[".into()))),
        ]),
    ] {
        let invalid = |r: Result<(), TalaDbError>| {
            assert!(
                matches!(r, Err(TalaDbError::InvalidFilter(_))),
                "{filter:?}: {r:?}"
            );
        };
        invalid(col.find(filter.clone()).map(drop));
        invalid(col.find_one(filter.clone()).map(drop));
        invalid(col.count(filter.clone()).map(drop));
        invalid(
            col.find_with_options(
                filter.clone(),
                FindOptions {
                    limit: Some(1),
                    ..Default::default()
                },
            )
            .map(drop),
        );
    }
}

/// Corrupt one stored record in many ways and compare `find`, `find_one` and
/// paged reads with the full scan over the same bytes: the same documents,
/// or the same error. `count` decoded projections before this change too; it
/// must fail exactly when the full decode fails.
#[test]
fn malformed_records_fail_exactly_as_the_full_scan_does() {
    let backend = Arc::new(RedbBackend::open_in_memory().unwrap());
    let db = Database::open_with_backend(Box::new(Shared(backend.clone()))).unwrap();
    let col = db.collection("bad").unwrap();
    col.insert_many((0..200).map(varied).collect()).unwrap();
    let records = raw(&backend, "bad");
    // A record past the first match of every filter below.
    let (key, good) = records[50].clone();
    assert_eq!(&good[1..27], id(50).as_bytes());

    let mut variants: Vec<Vec<u8>> = [0, 1, 10, 27, 28, 40, good.len() / 2, good.len() - 1]
        .iter()
        .map(|&n| good[..n].to_vec())
        .collect();
    let mut at = |pos: usize, byte: u8| {
        let mut v = good.clone();
        v[pos] = byte;
        variants.push(v);
    };
    // An invalid ULID character, and invalid UTF-8 in the id.
    at(5, b'U');
    at(5, 0xFF);
    let find = |needle: &[u8]| {
        good.windows(needle.len())
            .position(|w| w == needle)
            .unwrap()
    };
    // Invalid UTF-8 in a field name and in a skipped string value, an
    // invalid `Value` tag before the payload, a bool that is neither 0 nor 1.
    at(find(b"payload"), 0xFF);
    at(find(b"xxxx"), 0xFF);
    at(find(b"payload") + 7, 9);
    at(find(b"flag") + 5, 2);
    let mut trailing = good.clone();
    trailing.extend_from_slice(&[0xFF, 0]);
    variants.push(trailing);

    let filters = [
        Filter::Eq("i".into(), Value::Int(50)),
        Filter::Eq("i".into(), Value::Int(-1)),
        Filter::Eq("flag".into(), Value::Bool(true)),
        Filter::Regex("s".into(), "^a".into()),
    ];
    let mut errors = 0;
    for v in &variants {
        let mut txn = backend.begin_write().unwrap();
        txn.put("docs::bad", &key, v).unwrap();
        txn.commit().unwrap();
        let records = raw(&backend, "bad");
        let full_ok = postcard::from_bytes::<Document>(v).is_ok();
        errors += usize::from(!full_ok);
        for filter in &filters {
            let same = |got: Result<Vec<Document>, TalaDbError>, limit: Option<usize>| {
                let expected = reference_scan(&records, filter, limit).map(|d| encoded(&d));
                let got = got.map(|d| encoded(&d)).map_err(|e| e.to_string());
                assert_eq!(got, expected, "{filter:?} {v:?}");
            };
            same(col.find(filter.clone()), None);
            same(
                col.find_one(filter.clone())
                    .map(|d| d.into_iter().collect()),
                Some(1),
            );
            same(
                col.find_with_options(
                    filter.clone(),
                    FindOptions {
                        limit: Some(60),
                        ..Default::default()
                    },
                ),
                Some(60),
            );
            assert_eq!(
                col.count(filter.clone()).is_ok(),
                full_ok,
                "{filter:?} {v:?}"
            );
        }
    }
    assert!(errors >= 10, "{errors}");
}

/// A scan holds one read snapshot: a write committed after the read began
/// is invisible to both the projection and the whole-document decode.
#[test]
fn a_filtered_scan_reads_one_snapshot_under_concurrent_writes() {
    const N: usize = 400;
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("snap").unwrap();
    col.insert_many(
        (0..N)
            .map(|i| {
                vec![
                    ("_id".into(), s(&id(i))),
                    ("phase".into(), s("even")),
                    ("generation".into(), Value::Int(0)),
                    ("payload".into(), s(&"x".repeat(1_024))),
                ]
            })
            .collect(),
    )
    .unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let col = db.collection("snap").unwrap();
            for generation in 1..=60i64 {
                let phase = if generation % 2 == 0 { "even" } else { "odd" };
                col.update_many(
                    Filter::All,
                    Update::Set(vec![
                        ("phase".into(), s(phase)),
                        ("generation".into(), Value::Int(generation)),
                    ]),
                )
                .unwrap();
            }
            stop.store(true, Ordering::SeqCst);
        });
        for _ in 0..2 {
            scope.spawn(|| {
                let col = db.collection("snap").unwrap();
                let mut reads = 0;
                while !stop.load(Ordering::SeqCst) || reads < 20 {
                    reads += 1;
                    let docs = col.find(Filter::Eq("phase".into(), s("even"))).unwrap();
                    // Every update flips every document in one transaction,
                    // so a snapshot holds all of them or none, and each has
                    // the generation its phase was written with.
                    assert!(docs.is_empty() || docs.len() == N, "{}", docs.len());
                    let generations: Vec<_> = docs.iter().map(|d| d.get("generation")).collect();
                    assert!(
                        generations.windows(2).all(|w| w[0] == w[1]),
                        "mixed generations"
                    );
                    assert!(docs.iter().all(|d| {
                        d.get("phase") == Some(&s("even"))
                            && matches!(d.get("generation"), Some(Value::Int(g)) if g % 2 == 0)
                    }));
                    let one = col.find_one(Filter::Eq("phase".into(), s("odd"))).unwrap();
                    assert!(one.is_none_or(|d| {
                        matches!(d.get("generation"), Some(Value::Int(g)) if g % 2 == 1)
                    }));
                    let count = col.count(Filter::Eq("phase".into(), s("odd"))).unwrap();
                    assert!(count == 0 || count == N as u64, "{count}");
                }
            });
        }
    });
}

/// Holds the next read transaction, once it has begun, until released.
struct Gate {
    armed: AtomicBool,
    began: Mutex<mpsc::Sender<()>>,
    resume: Mutex<mpsc::Receiver<()>>,
}
struct Gated {
    inner: RedbBackend,
    gate: Arc<Gate>,
}
impl StorageBackend for Gated {
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        self.inner.begin_write()
    }
    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        let txn = self.inner.begin_read()?;
        if self.gate.armed.swap(false, Ordering::SeqCst) {
            self.gate.began.lock().unwrap().send(()).unwrap();
            self.gate
                .resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
        Ok(txn)
    }
}

/// A write that commits while a filtered scan's snapshot is open is seen by
/// neither its projections nor its whole-document decodes.
#[test]
fn a_write_committed_mid_query_is_invisible_to_it() {
    const N: usize = 300;
    let (began, began_rx) = mpsc::channel();
    let (resume_tx, resume) = mpsc::channel();
    let gate = Arc::new(Gate {
        armed: AtomicBool::new(false),
        began: Mutex::new(began),
        resume: Mutex::new(resume),
    });
    let db = Database::open_with_backend(Box::new(Gated {
        inner: RedbBackend::open_in_memory().unwrap(),
        gate: gate.clone(),
    }))
    .unwrap();
    let col = db.collection("gated").unwrap();
    col.insert_many(
        (0..N)
            .map(|i| {
                vec![
                    ("_id".into(), s(&id(i))),
                    ("phase".into(), s("old")),
                    ("generation".into(), Value::Int(0)),
                    ("payload".into(), s(&"x".repeat(1_024))),
                ]
            })
            .collect(),
    )
    .unwrap();
    let old = Filter::Eq("phase".into(), s("old"));
    let queries: [&(dyn Fn() -> (usize, Vec<Document>) + Sync); 3] = [
        &|| {
            let docs = col.find(old.clone()).unwrap();
            (docs.len(), docs)
        },
        &|| {
            let docs: Vec<_> = col.find_one(old.clone()).unwrap().into_iter().collect();
            (docs.len(), docs)
        },
        &|| (col.count(old.clone()).unwrap() as usize, Vec::new()),
    ];
    for (generation, query) in (1..).zip(queries) {
        gate.armed.store(true, Ordering::SeqCst);
        let (n, docs) = std::thread::scope(|scope| {
            let reader = scope.spawn(query);
            began_rx.recv_timeout(Duration::from_secs(10)).unwrap();
            col.update_many(
                Filter::All,
                Update::Set(vec![
                    ("phase".into(), s("new")),
                    ("generation".into(), Value::Int(generation)),
                ]),
            )
            .unwrap();
            resume_tx.send(()).unwrap();
            reader.join().unwrap()
        });
        assert!(n == N || (n == 1 && docs.len() == 1), "{n}");
        assert!(
            docs.iter()
                .all(|d| d.get("generation") == Some(&Value::Int(0)))
        );
        // The next query starts after the write and sees none of the old phase.
        assert_eq!(col.count(old.clone()).unwrap(), 0);
        col.update_many(
            Filter::All,
            Update::Set(vec![
                ("phase".into(), s("old")),
                ("generation".into(), Value::Int(0)),
            ]),
        )
        .unwrap();
    }
}

#[cfg(feature = "encryption")]
mod encrypted {
    use super::*;
    use taladb::crypto::EncryptedBackend;
    use zeroize::Zeroizing;

    /// Encrypted fields are filtered as stored (ciphertext), then decrypted:
    /// compare with a handle that does not decrypt.
    #[test]
    fn field_encryption_filters_the_stored_values_then_decrypts() {
        let db = Database::open_in_memory().unwrap();
        let col = db
            .collection("users")
            .unwrap()
            .with_field_encryption(vec!["secret".into()], Zeroizing::new([42u8; 32]));
        let stored = db.collection("users").unwrap();
        col.insert_many(
            (0..300)
                .map(|i| {
                    let mut fields = varied(i);
                    if i % 3 != 0 {
                        fields.push(("secret".into(), s(&format!("secret-{i}"))));
                    }
                    fields
                })
                .collect(),
        )
        .unwrap();
        let decrypted = col.find(Filter::All).unwrap();
        for filter in [
            Filter::Eq("i".into(), Value::Int(7)),
            Filter::Lt("i".into(), Value::Int(30)),
            Filter::Exists("secret".into(), true),
            Filter::Eq("secret".into(), s("secret-7")),
            Filter::Ne("secret".into(), s("secret-7")),
            Filter::And(vec![
                Filter::Exists("secret".into(), false),
                Filter::Eq("flag".into(), Value::Bool(true)),
            ]),
        ] {
            let ids: Vec<Ulid> = stored
                .find(Filter::All)
                .unwrap()
                .into_iter()
                .filter(|d| filter.matches(d).unwrap())
                .map(|d| d.id)
                .collect();
            let expected: Vec<Document> = ids
                .iter()
                .map(|id| decrypted.iter().find(|d| d.id == *id).unwrap().clone())
                .collect();
            assert_eq!(
                encoded(&col.find(filter.clone()).unwrap()),
                encoded(&expected),
                "{filter:?}"
            );
            assert_eq!(col.count(filter.clone()).unwrap(), expected.len() as u64);
            assert_eq!(
                encoded(
                    &col.find_one(filter.clone())
                        .unwrap()
                        .into_iter()
                        .collect::<Vec<_>>()
                ),
                encoded(&expected[..expected.len().min(1)]),
                "{filter:?}"
            );
        }
    }

    #[test]
    fn encrypted_database_matches_the_full_scan() {
        let inner = Arc::new(RedbBackend::open_in_memory().unwrap());
        let db = Database::open_with_backend(Box::new(EncryptedBackend::new(
            inner,
            Zeroizing::new([7u8; 32]),
        )))
        .unwrap();
        let col = db.collection("varied").unwrap();
        col.insert_many((0..200).map(varied).collect()).unwrap();
        let oracle = Oracle::of(&col);
        // The backend decrypts records below the decoder, so a sample of the
        // shapes `every_filter_shape_matches_the_full_scan` covers is enough
        // (AES-GCM is slow in debug builds).
        for filter in varied_filters().into_iter().step_by(6) {
            check(&col, &oracle, &filter);
        }
    }
}
