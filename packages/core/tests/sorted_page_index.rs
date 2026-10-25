//! Sorted pages served from a secondary index must equal a full scan-and-sort.
//!
//! Every query runs twice: on a collection with an index on the first sort key
//! (the fast path when the index can prove its order) and on an unindexed twin
//! holding the same documents under the same ids (always scan and sort). Pages
//! must agree exactly — ids, order and returned values. A counting storage
//! backend shows which path ran: how many index keys were walked and how many
//! documents were decoded.

use std::collections::HashSet;
use std::ops::Bound;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use taladb::aggregate::Stage;
use taladb::engine::{KvPairs, ReadTxn, WriteTxn};
use taladb::query::{FindOptions, SortSpec};
use taladb::{
    Database, Document, Filter, RedbBackend, StorageBackend, TalaDbError, Ulid, Update, Value,
};

// ---------------------------------------------------------------------------
// Counting backend
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Counters {
    /// Keys handed out from `idx::` tables.
    index_keys: AtomicU64,
    /// Documents read from `docs::` tables, by point read or scan.
    docs: AtomicU64,
}
impl Counters {
    fn take(&self) -> (u64, u64) {
        (
            self.index_keys.swap(0, Ordering::SeqCst),
            self.docs.swap(0, Ordering::SeqCst),
        )
    }
    fn note(&self, table: &str, n: u64) {
        if table.starts_with("idx::") {
            self.index_keys.fetch_add(n, Ordering::SeqCst);
        } else if table.starts_with("docs::") {
            self.docs.fetch_add(n, Ordering::SeqCst);
        }
    }
}

struct Counting {
    inner: RedbBackend,
    counters: Arc<Counters>,
}
struct CountingRead<'a> {
    inner: Box<dyn ReadTxn + 'a>,
    counters: Arc<Counters>,
}
impl StorageBackend for Counting {
    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        Ok(Box::new(CountingRead {
            inner: self.inner.begin_read()?,
            counters: self.counters.clone(),
        }))
    }
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        self.inner.begin_write()
    }
}
impl ReadTxn for CountingRead<'_> {
    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        let v = self.inner.get(table, key)?;
        self.counters.note(table, u64::from(v.is_some()));
        Ok(v)
    }
    fn get_many(&self, table: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        let v = self.inner.get_many(table, keys)?;
        self.counters
            .note(table, v.iter().filter(|x| x.is_some()).count() as u64);
        Ok(v)
    }
    fn range(&self, table: &str, s: Bound<&[u8]>, e: Bound<&[u8]>) -> Result<KvPairs, TalaDbError> {
        let v = self.inner.range(table, s, e)?;
        self.counters.note(table, v.len() as u64);
        Ok(v)
    }
    fn scan_all(&self, table: &str) -> Result<KvPairs, TalaDbError> {
        let v = self.inner.scan_all(table)?;
        self.counters.note(table, v.len() as u64);
        Ok(v)
    }
    fn list_tables(&self) -> Result<Vec<String>, TalaDbError> {
        self.inner.list_tables()
    }
    fn count_entries(&self, table: &str) -> Result<u64, TalaDbError> {
        self.inner.count_entries(table)
    }
    fn scan(
        &self,
        table: &str,
        s: Bound<&[u8]>,
        e: Bound<&[u8]>,
        f: taladb::engine::ScanFn<'_>,
    ) -> Result<(), TalaDbError> {
        let counters = &self.counters;
        self.inner.scan(table, s, e, &mut |k, v| {
            counters.note(table, 1);
            f(k, v)
        })
    }
}

fn counted_db() -> (Database, Arc<Counters>) {
    let counters = Arc::new(Counters::default());
    let db = Database::open_with_backend(Box::new(Counting {
        inner: RedbBackend::open_in_memory().unwrap(),
        counters: counters.clone(),
    }))
    .unwrap();
    (db, counters)
}

// ---------------------------------------------------------------------------
// Fixtures and oracle
// ---------------------------------------------------------------------------

/// Ids are spread so their order matches neither insertion nor value order:
/// a tie broken on the wrong id shows up.
fn id(i: usize) -> Ulid {
    Ulid::from(((i as u128 * 7_919) % 100_003 + 1) * 1_000_003)
}

/// Documents `(id, n, m, s)`. `n` is the indexed first sort key; `m` and `s`
/// are secondary keys. `n = None` leaves the field out.
type Row = (usize, Option<Value>, i64, String);

fn doc_fields(row: &Row) -> Vec<(String, Value)> {
    let (i, n, m, s) = row;
    let mut fields = vec![
        ("_id".to_string(), Value::Str(id(*i).to_string())),
        ("m".to_string(), Value::Int(*m)),
        ("s".to_string(), Value::Str(s.clone())),
    ];
    if let Some(n) = n {
        fields.push(("n".to_string(), n.clone()));
    }
    fields
}

struct Pair {
    db: Database,
    counters: Arc<Counters>,
}
impl Pair {
    fn new(rows: &[Row]) -> Self {
        let (db, counters) = counted_db();
        let indexed = db.collection("indexed").unwrap();
        let plain = db.collection("plain").unwrap();
        indexed.create_index("n").unwrap();
        for chunk in rows.chunks(500) {
            indexed
                .insert_many(chunk.iter().map(doc_fields).collect())
                .unwrap();
            plain
                .insert_many(chunk.iter().map(doc_fields).collect())
                .unwrap();
        }
        counters.take();
        Pair { db, counters }
    }
    fn indexed(&self) -> taladb::Collection {
        self.db.collection("indexed").unwrap()
    }
    fn plain(&self) -> taladb::Collection {
        self.db.collection("plain").unwrap()
    }
    /// Apply the same write to both collections.
    fn both(&self, f: impl Fn(&taladb::Collection)) {
        f(&self.indexed());
        f(&self.plain());
        self.counters.take();
    }
}

/// Ids in order, plus a rendering of every returned value that keeps NaN and
/// the sign of zero apart (`Value`'s `PartialEq` cannot).
fn render(docs: &[Document]) -> Vec<(Ulid, String)> {
    docs.iter()
        .map(|d| {
            let fields: Vec<String> = d
                .fields
                .iter()
                // Written per collection at insert time, so it differs by twin.
                .filter(|(k, _)| k != "_changed_at")
                .map(|(k, v)| match v {
                    Value::Float(f) => format!("{k}=f{:#x}", f.to_bits()),
                    other => format!("{k}={other:?}"),
                })
                .collect();
            (d.id, fields.join(","))
        })
        .collect()
}

fn opts(sort: &[SortSpec], skip: u64, limit: Option<u64>) -> FindOptions {
    FindOptions {
        sort: sort.to_vec(),
        skip,
        limit,
        ..Default::default()
    }
}

/// A page's outcome. A panic is kept as an outcome: on NaN, the scan-and-sort
/// path itself can panic (its comparator calls NaN equal to everything, which
/// is not a total order), and both collections must then behave alike.
fn outcome(col: &taladb::Collection, o: FindOptions) -> Result<Vec<(Ulid, String)>, &'static str> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        render(&col.find_with_options(Filter::All, o).unwrap())
    }))
    .map_err(|_| "panicked")
}

/// Runs a page on both collections; returns (index keys walked, documents
/// decoded) by the indexed side.
fn check_page(
    pair: &Pair,
    sort: &[SortSpec],
    skip: u64,
    limit: Option<u64>,
    label: &str,
) -> (u64, u64) {
    pair.counters.take();
    let got = outcome(&pair.indexed(), opts(sort, skip, limit));
    let cost = pair.counters.take();
    let want = outcome(&pair.plain(), opts(sort, skip, limit));
    pair.counters.take();
    assert_eq!(
        got, want,
        "{label}: sort {sort:?} skip {skip} limit {limit:?}"
    );
    cost
}

fn sorts() -> Vec<Vec<SortSpec>> {
    vec![
        vec![SortSpec::asc("n")],
        vec![SortSpec::desc("n")],
        vec![SortSpec::asc("n"), SortSpec::desc("m")],
        vec![SortSpec::desc("n"), SortSpec::asc("m")],
        vec![SortSpec::desc("n"), SortSpec::desc("m"), SortSpec::asc("s")],
        vec![SortSpec::asc("n"), SortSpec::asc("s")],
    ]
}

/// Pages that start and end on, before and after run boundaries, deep inside
/// the collection, past its end, and empty.
fn pages(len: usize) -> Vec<(u64, Option<u64>)> {
    let len = len as u64;
    let mut out = Vec::new();
    for skip in [
        0,
        1,
        9,
        10,
        11,
        49,
        50,
        51,
        99,
        100,
        101,
        len / 2,
        len - 1,
        len,
        len + 5,
    ] {
        for limit in [0, 1, 3, 10, 50, 120, len, u64::MAX] {
            out.push((skip, Some(limit)));
        }
    }
    out
}

fn check_all(pair: &Pair, len: usize, label: &str) {
    for sort in sorts() {
        for (skip, limit) in pages(len) {
            check_page(pair, &sort, skip, limit, label);
        }
    }
}

fn int_rows(len: usize, value: impl Fn(usize) -> Option<Value>) -> Vec<Row> {
    (0..len)
        .map(|i| (i, value(i), (i % 7) as i64, format!("s{}", i % 5)))
        .collect()
}

// ---------------------------------------------------------------------------
// Results equal the oracle
// ---------------------------------------------------------------------------

#[test]
fn eligible_fixtures_match_a_full_sort_on_every_page() {
    let len = 300;
    let fixtures: Vec<(&str, Vec<Row>)> = vec![
        (
            "unique ints",
            int_rows(len, |i| Some(Value::Int((i * 37 % len) as i64 - 150))),
        ),
        ("all equal", int_rows(len, |_| Some(Value::Int(7)))),
        (
            "runs of 50",
            int_rows(len, |i| Some(Value::Int((i % 6) as i64))),
        ),
        (
            "runs of 1..=24",
            int_rows(len, |i| Some(Value::Int(((i as f64).sqrt() as i64) % 9))),
        ),
        (
            "floats with infinities",
            int_rows(len, |i| {
                Some(Value::Float(match i % 5 {
                    0 => f64::INFINITY,
                    1 => f64::NEG_INFINITY,
                    2 => f64::MIN_POSITIVE,
                    3 => -1.5,
                    _ => (i % 11) as f64 + 0.25,
                }))
            }),
        ),
        (
            "strings with NUL and prefixes",
            int_rows(len, |i| {
                Some(Value::Str(
                    ["", "a", "a\0", "a\0b", "ab", "b", "\0", "é"][i % 8].to_string(),
                ))
            }),
        ),
        (
            "nulls and bools",
            int_rows(len, |i| {
                Some([Value::Null, Value::Bool(false), Value::Bool(true)][i % 3].clone())
            }),
        ),
    ];
    for (label, rows) in fixtures {
        let pair = Pair::new(&rows);
        check_all(&pair, len, label);
    }
}

#[test]
fn fallback_fixtures_match_a_full_sort_on_every_page() {
    let len = 120;
    let fixtures: Vec<(&str, Vec<Row>)> = vec![
        (
            "mixed int and float",
            int_rows(len, |i| {
                Some(if i % 2 == 0 {
                    Value::Int(i as i64 % 9)
                } else {
                    Value::Float((i % 9) as f64)
                })
            }),
        ),
        (
            "NaN",
            int_rows(len, |i| {
                Some(Value::Float(if i % 10 == 0 {
                    f64::NAN
                } else {
                    (i % 9) as f64 + 0.5
                }))
            }),
        ),
        (
            "negative NaN",
            int_rows(len, |i| {
                Some(Value::Float(if i == 3 {
                    -f64::NAN
                } else {
                    (i % 9) as f64 + 0.5
                }))
            }),
        ),
        (
            "signed zero",
            int_rows(len, |i| Some(Value::Float([0.0, -0.0, 1.5, -2.5][i % 4]))),
        ),
        (
            "positive zero only",
            int_rows(len, |i| {
                Some(Value::Float(if i % 9 == 0 { 0.0 } else { 3.5 }))
            }),
        ),
        (
            "missing field",
            int_rows(len, |i| (i % 13 != 0).then(|| Value::Int(i as i64 % 5))),
        ),
        (
            "array values",
            int_rows(len, |i| {
                Some(if i == 7 {
                    Value::Array(vec![Value::Int(1), Value::Int(40)])
                } else {
                    Value::Int(i as i64 % 5)
                })
            }),
        ),
        (
            "object value",
            int_rows(len, |i| {
                Some(if i == 7 {
                    Value::Object(vec![("x".into(), Value::Int(1))])
                } else {
                    Value::Int(i as i64 % 5)
                })
            }),
        ),
    ];
    for (label, rows) in fixtures {
        let pair = Pair::new(&rows);
        check_all(&pair, len, label);
    }
}

#[test]
fn aggregation_pipelines_match_a_full_sort() {
    let len = 300;
    for (label, rows) in [
        (
            "runs of 50",
            int_rows(len, |i| Some(Value::Int((i % 6) as i64))),
        ),
        ("all equal", int_rows(len, |_| Some(Value::Int(7)))),
        (
            "mixed",
            int_rows(len, |i| {
                Some(if i % 2 == 0 {
                    Value::Int(1)
                } else {
                    Value::Float(1.5)
                })
            }),
        ),
    ] {
        let pair = Pair::new(&rows);
        for sort in sorts() {
            for (skip, limit) in [
                (0, 1),
                (0, 10),
                (20, 10),
                (49, 3),
                (51, 120),
                (290, 50),
                (400, 5),
                (0, 0),
            ] {
                let pipelines: Vec<Vec<Stage>> = vec![
                    vec![Stage::Sort(sort.clone()), Stage::Limit(limit)],
                    vec![
                        Stage::Sort(sort.clone()),
                        Stage::Skip(skip),
                        Stage::Limit(limit),
                    ],
                    vec![
                        Stage::Sort(sort.clone()),
                        Stage::Skip(skip),
                        Stage::Limit(limit),
                        Stage::Project {
                            fields: vec!["n".into(), "m".into()],
                            include: true,
                            keep_id: true,
                        },
                    ],
                    vec![
                        Stage::Sort(sort.clone()),
                        Stage::Limit(skip + limit),
                        Stage::Skip(skip),
                    ],
                ];
                for pipeline in pipelines {
                    let got = pair.indexed().aggregate(pipeline.clone()).unwrap();
                    let want = pair.plain().aggregate(pipeline.clone()).unwrap();
                    assert_eq!(render(&got), render(&want), "{label}: {pipeline:?}");
                }
            }
        }
    }
}

#[test]
fn projection_and_exclusion_apply_to_index_served_pages() {
    let rows = int_rows(200, |i| Some(Value::Int((i % 6) as i64)));
    let pair = Pair::new(&rows);
    for sort in sorts() {
        for (skip, limit) in [(0, 10), (33, 17), (195, 10)] {
            for (fields, exclude) in [
                (Some(vec!["m".to_string()]), None),
                (None, Some(vec!["s".to_string()])),
            ] {
                let o = FindOptions {
                    fields: fields.clone(),
                    exclude: exclude.clone(),
                    ..opts(&sort, skip, Some(limit))
                };
                let got = pair
                    .indexed()
                    .find_with_options(Filter::All, o.clone())
                    .unwrap();
                let want = pair.plain().find_with_options(Filter::All, o).unwrap();
                assert_eq!(render(&got), render(&want));
            }
        }
    }
}

#[test]
fn paging_through_the_whole_collection_returns_every_document_once_in_order() {
    let len = 503;
    let rows = int_rows(len, |i| Some(Value::Int((i % 17) as i64)));
    let pair = Pair::new(&rows);
    for sort in sorts() {
        let full = pair
            .plain()
            .find_with_options(Filter::All, opts(&sort, 0, None))
            .unwrap();
        let mut walked = Vec::new();
        for skip in (0..len as u64 + 7).step_by(7) {
            walked.extend(
                pair.indexed()
                    .find_with_options(Filter::All, opts(&sort, skip, Some(7)))
                    .unwrap(),
            );
        }
        assert_eq!(render(&walked), render(&full), "{sort:?}");
    }
}

#[test]
fn writes_between_snapshots_and_index_rebuilds_are_seen() {
    let len = 300;
    let rows = int_rows(len, |i| Some(Value::Int((i % 6) as i64)));
    let pair = Pair::new(&rows);
    check_all(&pair, len, "initial");

    let by_id = |i: usize| Filter::Eq("_id".into(), Value::Str(id(i).to_string()));
    // Move documents into, across and out of the first runs.
    pair.both(|c| {
        for i in [0, 6, 12, 299, 150] {
            c.update_one(by_id(i), Update::Set(vec![("n".into(), Value::Int(-1))]))
                .unwrap();
        }
    });
    check_all(&pair, len, "after updates");
    pair.both(|c| {
        c.insert_many(
            (1000..1040)
                .map(|i| doc_fields(&(i, Some(Value::Int(2)), 3, "z".into())))
                .collect(),
        )
        .unwrap();
    });
    check_all(&pair, len + 40, "after inserts");
    pair.both(|c| {
        c.delete_many(Filter::Eq("n".into(), Value::Int(0)))
            .unwrap();
        c.delete_one(by_id(1)).unwrap();
    });
    check_all(&pair, len - 10, "after deletes");

    // A field that loses its last missing value becomes eligible again.
    pair.both(|c| {
        c.update_one(by_id(5), Update::Unset(vec!["n".into()]))
            .unwrap();
    });
    check_all(&pair, len - 10, "one document without the field");
    pair.both(|c| {
        c.update_one(by_id(5), Update::Set(vec![("n".into(), Value::Int(4))]))
            .unwrap();
    });
    check_all(&pair, len - 10, "field restored");

    pair.indexed().drop_index("n").unwrap();
    check_all(&pair, len - 10, "index dropped");
    pair.indexed().create_index("n").unwrap();
    check_all(&pair, len - 10, "index rebuilt");
}

#[cfg(feature = "encryption")]
#[test]
fn encrypted_fields_and_encrypted_databases_match_a_full_sort() {
    let rows = int_rows(200, |i| Some(Value::Int((i % 6) as i64)));
    // Field encryption on the sort key: the index is not used for order.
    let (db, _) = counted_db();
    let key = zeroize::Zeroizing::new([7u8; 32]);
    let indexed = db
        .collection("indexed")
        .unwrap()
        .with_field_encryption(vec!["n".into()], key.clone());
    let plain = db
        .collection("plain")
        .unwrap()
        .with_field_encryption(vec!["n".into()], key);
    indexed.create_index("n").unwrap();
    indexed
        .insert_many(rows.iter().map(doc_fields).collect())
        .unwrap();
    plain
        .insert_many(rows.iter().map(doc_fields).collect())
        .unwrap();
    for sort in sorts() {
        for (skip, limit) in [(0, 10), (49, 3), (190, 50)] {
            let got = indexed
                .find_with_options(Filter::All, opts(&sort, skip, Some(limit)))
                .unwrap();
            let want = plain
                .find_with_options(Filter::All, opts(&sort, skip, Some(limit)))
                .unwrap();
            assert_eq!(render(&got), render(&want), "field encryption {sort:?}");
        }
    }

    // Whole-database encryption leaves keys in the clear: the fast path runs.
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_encrypted(&dir.path().join("e.db"), "passphrase").unwrap();
    let indexed = db.collection("indexed").unwrap();
    let plain = db.collection("plain").unwrap();
    indexed.create_index("n").unwrap();
    indexed
        .insert_many(rows.iter().map(doc_fields).collect())
        .unwrap();
    plain
        .insert_many(rows.iter().map(doc_fields).collect())
        .unwrap();
    for sort in sorts() {
        for (skip, limit) in [(0, 10), (49, 3), (190, 50)] {
            let got = indexed
                .find_with_options(Filter::All, opts(&sort, skip, Some(limit)))
                .unwrap();
            let want = plain
                .find_with_options(Filter::All, opts(&sort, skip, Some(limit)))
                .unwrap();
            assert_eq!(render(&got), render(&want), "database encryption {sort:?}");
        }
    }
}

#[test]
fn concurrent_writes_never_produce_an_inconsistent_page() {
    let len = 2000;
    let rows = int_rows(len, |i| Some(Value::Int((i % 40) as i64)));
    let pair = Pair::new(&rows);
    std::thread::scope(|s| {
        for reader in 0..3 {
            let col = pair.indexed();
            s.spawn(move || {
                let sort = &sorts()[reader % 6];
                for round in 0..60 {
                    let skip = (round * 31 % 1900) as u64;
                    let page = col
                        .find_with_options(Filter::All, opts(sort, skip, Some(25)))
                        .unwrap();
                    // The collection's size never changes, so every page is full.
                    assert_eq!(page.len(), 25);
                    let ids: HashSet<Ulid> = page.iter().map(|d| d.id).collect();
                    assert_eq!(ids.len(), 25, "duplicate on a page");
                    let mut sorted = page.clone();
                    taladb::query::options::sort_documents(&mut sorted, sort);
                    assert_eq!(render(&sorted), render(&page), "page out of order");
                }
            });
        }
        let writer = pair.indexed();
        for i in 0..300 {
            writer
                .update_one(
                    Filter::Eq("_id".into(), Value::Str(id(i * 7 % len).to_string())),
                    Update::Set(vec![("n".into(), Value::Int((i % 50) as i64 - 5))]),
                )
                .unwrap();
        }
    });
}

// ---------------------------------------------------------------------------
// The fast path runs, and does only the work it should
// ---------------------------------------------------------------------------

#[test]
fn ascending_walks_stop_at_the_page_and_decode_only_the_page() {
    let len = 10_000;
    let pair = Pair::new(&int_rows(len, |i| Some(Value::Int(i as i64))));
    // Each eligibility probe reads at most one key.
    let (keys, docs) = check_page(&pair, &[SortSpec::asc("n")], 20, Some(10), "asc");
    assert!(keys <= 30 + 5, "walked {keys} index keys");
    assert_eq!(docs, 10, "decoded {docs} documents");
    let (keys, docs) = check_page(&pair, &[SortSpec::asc("n")], 5_000, Some(10), "deep asc");
    assert!(keys <= 5_010 + 5, "walked {keys} index keys");
    assert_eq!(docs, 10);
}

#[test]
fn descending_walks_every_key_but_decodes_only_the_page() {
    let len = 10_000;
    let pair = Pair::new(&int_rows(len, |i| Some(Value::Int(i as i64))));
    let (keys, docs) = check_page(&pair, &[SortSpec::desc("n")], 20, Some(10), "desc");
    assert!(
        (len as u64..=len as u64 + 5).contains(&keys),
        "walked {keys}"
    );
    assert_eq!(docs, 10);
}

#[test]
fn single_key_ties_decode_only_the_page_however_long_the_run() {
    let len = 5_000;
    let pair = Pair::new(&int_rows(len, |_| Some(Value::Int(1))));
    for sort in [vec![SortSpec::asc("n")], vec![SortSpec::desc("n")]] {
        let (_, docs) = check_page(&pair, &sort, 2_000, Some(10), "one run");
        assert_eq!(docs, 10, "{sort:?}");
    }
}

#[test]
fn multi_key_pages_decode_the_runs_they_touch_and_no_more() {
    // Runs of 100: a page inside run 3 decodes run 3 only.
    let len = 1_000;
    let pair = Pair::new(&int_rows(len, |i| Some(Value::Int((i / 100) as i64))));
    for sort in [
        vec![SortSpec::asc("n"), SortSpec::desc("m")],
        vec![SortSpec::desc("n"), SortSpec::asc("m")],
    ] {
        let (_, docs) = check_page(&pair, &sort, 310, Some(20), "inside a run");
        assert_eq!(docs, 100, "{sort:?}");
        let (_, docs) = check_page(&pair, &sort, 390, Some(20), "across two runs");
        assert_eq!(docs, 200, "{sort:?}");
    }
}

#[test]
fn ineligible_fields_take_the_scan_path() {
    let len = 400;
    for (label, rows) in [
        (
            "missing",
            int_rows(len, |i| (i != 9).then(|| Value::Int(i as i64))),
        ),
        (
            "mixed",
            int_rows(len, |i| {
                Some(if i == 9 {
                    Value::Float(9.5)
                } else {
                    Value::Int(i as i64)
                })
            }),
        ),
        (
            "nan",
            int_rows(len, |i| {
                Some(Value::Float(if i == 399 {
                    f64::NAN
                } else {
                    i as f64 + 0.5
                }))
            }),
        ),
        (
            "zero",
            int_rows(len, |i| {
                Some(Value::Float(if i == 399 { 0.0 } else { i as f64 + 0.5 }))
            }),
        ),
    ] {
        let pair = Pair::new(&rows);
        let (_, docs) = check_page(&pair, &[SortSpec::asc("n")], 0, Some(10), label);
        assert_eq!(docs, len as u64, "{label}: expected the scan path");
    }
}
