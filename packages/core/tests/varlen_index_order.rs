//! Indexed queries on strings and byte arrays must agree with a full scan when
//! values contain NUL and ids start with 0xFF.
//!
//! An index key is `value prefix ++ id`. Str and Bytes prefixes escape NUL as
//! `00 FF` and end with `00`, so `"a\0b"` encodes as `"a"`'s prefix ++ `FF` ++ …
//! — inside `"a"`'s own block of keys, which runs from `prefix ++ 00…00` to
//! `prefix ++ FF…FF`. On v0.12.1 and `dev` that made:
//!
//! * `$gt: "a"` miss every `"a\0…"` (any id);
//! * `count({ s: "a" })` count the `"a\0…"` documents too (any id);
//! * `$lt`/`$lte: "a\0b"` miss an `"a"` whose id starts with 0xFF, which sorts
//!   after `"a\0b"`;
//! * a sorted page served from the index put such an `"a"` after `"a\0b"`.
//!
//! Generated ids start with 0xFF only after the year 10889, but caller-supplied
//! ids can, and `derive_doc_id` hashes into all 16 bytes (about 1 in 256 start
//! with 0xFF). Every query here runs on an indexed collection and on an
//! unindexed twin holding the same documents under the same ids.

use std::collections::BTreeSet;
use std::ops::Bound;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use taladb::aggregate::{Accumulator, GroupKey, Stage};
use taladb::engine::{KvPairs, ReadTxn, WriteTxn};
use taladb::query::{FindOptions, SortSpec};
use taladb::{
    Collection, Database, Document, Filter, RedbBackend, StorageBackend, TalaDbError, Ulid, Update,
    Value, derive_doc_id,
};

// ---------------------------------------------------------------------------
// Counting backend: which path served a sorted page
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Counters {
    index_keys: AtomicU64,
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
// Fixtures
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Row {
    id: Ulid,
    s: Option<String>,
    b: Option<Vec<u8>>,
    t: i64,
}

fn fields(row: &Row) -> Vec<(String, Value)> {
    let mut f = vec![
        ("_id".to_string(), Value::Str(row.id.to_string())),
        ("t".to_string(), Value::Int(row.t)),
    ];
    if let Some(s) = &row.s {
        f.push(("s".to_string(), Value::Str(s.clone())));
    }
    if let Some(b) = &row.b {
        f.push(("b".to_string(), Value::Bytes(b.clone())));
    }
    f
}

/// Every word over `alphabet` up to `max_len` symbols, shortest first.
fn words(alphabet: &[u8], max_len: usize) -> Vec<Vec<u8>> {
    let mut out = vec![vec![]];
    let mut level = vec![vec![]];
    for _ in 0..max_len {
        level = level
            .iter()
            .flat_map(|w: &Vec<u8>| {
                alphabet.iter().map(move |&c| {
                    let mut w = w.clone();
                    w.push(c);
                    w
                })
            })
            .collect();
        out.extend(level.iter().cloned());
    }
    out
}

fn strings() -> Vec<String> {
    let mut out: Vec<String> = words(b"\0ab", 3)
        .into_iter()
        .map(|w| String::from_utf8(w).unwrap())
        .collect();
    out.extend(["é", "a\0é", "\0\0\0\0", "b\0a\0b"].map(String::from));
    out
}

fn byte_values() -> Vec<Vec<u8>> {
    let mut out = words(&[0x00, 0x01, 0xFF], 3);
    let mut long = vec![0x01, 0x00];
    long.extend([0xFF; 16]);
    out.push(long.clone());
    long.push(0x01);
    out.push(long);
    out
}

/// Ids of every shape that matters, `i` kept in the last bytes for uniqueness.
fn shaped_id(i: usize) -> Ulid {
    let mut id = [0u8; 16];
    match i % 8 {
        0 => id[..2].copy_from_slice(&[0x01, 0x8F]), // a generated ULID today
        1 => {}                                      // near the minimum
        2 => id[0] = 0xFF,
        3 => id[..2].copy_from_slice(&[0xFF, 0x70]),
        4 => id[..2].copy_from_slice(&[0xFF, 0xFF]),
        5 => id = [0xFF; 16], // near the maximum
        6 => return derive_doc_id("c", &format!("key-{i}")),
        _ => id[..3].copy_from_slice(&[0xFF, 0x61, 0x00]), // reads like "a\0"
    }
    id[14..].copy_from_slice(&(i as u16).to_be_bytes());
    Ulid::from_bytes(id)
}

/// Three documents per string, with ids of different shapes, and `t` ties.
fn shaped_rows() -> Vec<Row> {
    let strings = strings();
    let bytes = byte_values();
    let rows: Vec<Row> = (0..strings.len() * 3)
        .map(|i| Row {
            id: shaped_id(i),
            s: Some(strings[i % strings.len()].clone()),
            b: Some(bytes[(i * 7) % bytes.len()].clone()),
            t: (i % 3) as i64,
        })
        .collect();
    let unique: BTreeSet<Ulid> = rows.iter().map(|r| r.id).collect();
    assert_eq!(unique.len(), rows.len());
    rows
}

/// An indexed collection and its unindexed twin, holding the same documents.
struct Pair {
    _db: Database,
    indexed: Collection,
    plain: Collection,
}

impl Pair {
    fn in_db(db: Database, rows: &[Row]) -> Self {
        let indexed = db.collection("indexed").unwrap();
        let plain = db.collection("plain").unwrap();
        for field in ["s", "b", "t"] {
            indexed.create_index(field).unwrap();
        }
        let docs: Vec<_> = rows.iter().map(fields).collect();
        indexed.insert_many(docs.clone()).unwrap();
        plain.insert_many(docs).unwrap();
        Self {
            _db: db,
            indexed,
            plain,
        }
    }
    fn new(rows: &[Row]) -> Self {
        Self::in_db(Database::open_in_memory().unwrap(), rows)
    }
}

fn id_set(docs: Vec<Document>) -> BTreeSet<Ulid> {
    docs.into_iter().map(|d| d.id).collect()
}

/// One returned document: id, then `s`, `b` and `t`.
type Rendered = (Ulid, Option<Value>, Option<Value>, Option<Value>);

/// What a page returns, compared exactly: ids in order, and the values.
fn render(docs: &[Document]) -> Vec<Rendered> {
    docs.iter()
        .map(|d| {
            (
                d.id,
                d.get("s").cloned(),
                d.get("b").cloned(),
                d.get("t").cloned(),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

/// `find`, `count`, `find_one` and an unsorted limited `find` against the scan.
fn check_filter(pair: &Pair, filter: &Filter, label: &str) {
    let want = id_set(pair.plain.find(filter.clone()).unwrap());
    let got = id_set(pair.indexed.find(filter.clone()).unwrap());
    assert_eq!(got, want, "{label}: find");
    assert_eq!(
        pair.indexed.count(filter.clone()).unwrap(),
        want.len() as u64,
        "{label}: count"
    );
    match pair.indexed.find_one(filter.clone()).unwrap() {
        Some(doc) => assert!(want.contains(&doc.id), "{label}: find_one"),
        None => assert!(want.is_empty(), "{label}: find_one found nothing"),
    }
    let limited = pair
        .indexed
        .find_with_options(
            filter.clone(),
            FindOptions {
                limit: Some(3),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(limited.len(), want.len().min(3), "{label}: limit 3");
    assert!(
        limited.iter().all(|d| want.contains(&d.id)),
        "{label}: limit 3 subset"
    );
}

fn str_value(s: &str) -> Value {
    Value::Str(s.into())
}

/// Bounds whose encodings sit next to escaped NULs, plus an ordinary few.
fn tricky_strings() -> Vec<&'static str> {
    vec![
        "", "\0", "\0\0", "a", "a\0", "a\0\0", "a\0a", "a\0b", "a\0b\0", "aa", "ab", "b", "b\0",
        "b\0a", "é", "zz",
    ]
}

fn check_every_filter(pair: &Pair, label: &str) {
    let all = strings();
    for v in &all {
        let v = str_value(v);
        for (op, f) in [
            ("eq", Filter::Eq("s".into(), v.clone())),
            ("gt", Filter::Gt("s".into(), v.clone())),
            ("gte", Filter::Gte("s".into(), v.clone())),
            ("lt", Filter::Lt("s".into(), v.clone())),
            ("lte", Filter::Lte("s".into(), v.clone())),
        ] {
            check_filter(pair, &f, &format!("{label} s {op} {v:?}"));
        }
    }
    let tricky = tricky_strings();
    for lo in &tricky {
        for hi in &tricky {
            let (lo, hi) = (str_value(lo), str_value(hi));
            for (shape, f) in [
                (
                    "[lo, hi)",
                    Filter::And(vec![
                        Filter::Gte("s".into(), lo.clone()),
                        Filter::Lt("s".into(), hi.clone()),
                    ]),
                ),
                (
                    "(lo, hi]",
                    Filter::And(vec![
                        Filter::Gt("s".into(), lo.clone()),
                        Filter::Lte("s".into(), hi.clone()),
                    ]),
                ),
            ] {
                check_filter(pair, &f, &format!("{label} s {shape} {lo:?} {hi:?}"));
            }
        }
    }
    for window in tricky.windows(3) {
        let values: Vec<Value> = window.iter().map(|s| str_value(s)).collect();
        check_filter(
            pair,
            &Filter::In("s".into(), values.clone()),
            &format!("{label} s in {window:?}"),
        );
        check_filter(
            pair,
            &Filter::Or(vec![
                Filter::Eq("s".into(), values[0].clone()),
                Filter::Gt("s".into(), values[1].clone()),
            ]),
            &format!("{label} s or {window:?}"),
        );
        check_filter(
            pair,
            &Filter::And(vec![
                Filter::Gt("s".into(), values[0].clone()),
                Filter::Eq("t".into(), Value::Int(1)),
            ]),
            &format!("{label} s and t {window:?}"),
        );
    }
    for v in byte_values() {
        let v = Value::Bytes(v);
        check_filter(
            pair,
            &Filter::Eq("b".into(), v.clone()),
            &format!("{label} b eq {v:?}"),
        );
        check_filter(
            pair,
            &Filter::In("b".into(), vec![v.clone(), Value::Bytes(vec![0x01])]),
            &format!("{label} b in {v:?}"),
        );
        // Range filters never match Bytes, on either path; the index must not
        // invent matches.
        check_filter(
            pair,
            &Filter::Gt("b".into(), v.clone()),
            &format!("{label} b gt {v:?}"),
        );
    }
}

fn sorts() -> Vec<Vec<SortSpec>> {
    vec![
        vec![SortSpec::asc("s")],
        vec![SortSpec::desc("s")],
        vec![SortSpec::asc("s"), SortSpec::desc("t")],
        vec![SortSpec::desc("s"), SortSpec::asc("t")],
        vec![SortSpec::asc("b")],
        vec![SortSpec::desc("b"), SortSpec::asc("t")],
    ]
}

fn sorted(
    col: &Collection,
    filter: Filter,
    sort: &[SortSpec],
    skip: u64,
    limit: Option<u64>,
) -> Vec<Document> {
    col.find_with_options(
        filter,
        FindOptions {
            sort: sort.to_vec(),
            skip,
            limit,
            ..Default::default()
        },
    )
    .unwrap()
}

/// Sorted pages (index-served when eligible) and sorted aggregation, against
/// the scan.
fn check_sorted(pair: &Pair, label: &str) {
    let n = pair.plain.count(Filter::All).unwrap();
    let skips = [0, 1, 2, 5, 13, 50, n / 2, n.saturating_sub(3), n + 1];
    for sort in sorts() {
        for &skip in &skips {
            for limit in [Some(1), Some(2), Some(7), Some(40), None] {
                let got = sorted(&pair.indexed, Filter::All, &sort, skip, limit);
                let want = sorted(&pair.plain, Filter::All, &sort, skip, limit);
                assert_eq!(
                    render(&got),
                    render(&want),
                    "{label} {sort:?} skip {skip} limit {limit:?}"
                );
            }
            let pipeline = vec![
                Stage::Sort(sort.clone()),
                Stage::Skip(skip),
                Stage::Limit(9),
            ];
            assert_eq!(
                render(&pair.indexed.aggregate(pipeline.clone()).unwrap()),
                render(&pair.plain.aggregate(pipeline).unwrap()),
                "{label} aggregate {sort:?} skip {skip}"
            );
        }
        // A filtered sort runs on the planner's candidates instead.
        let filter = Filter::Gt("s".into(), str_value("a"));
        assert_eq!(
            render(&sorted(&pair.indexed, filter.clone(), &sort, 3, Some(20))),
            render(&sorted(&pair.plain, filter, &sort, 3, Some(20))),
            "{label} filtered {sort:?}"
        );
    }
    for t in 0..3 {
        let pipeline = vec![
            Stage::Match(Filter::And(vec![
                Filter::Gte("s".into(), str_value("a")),
                Filter::Lt("s".into(), str_value("a\0b")),
                Filter::Eq("t".into(), Value::Int(t)),
            ])),
            Stage::Group {
                key: GroupKey::Field("s".into()),
                accumulators: vec![("n".into(), Accumulator::Count)],
            },
        ];
        assert_eq!(
            render(&pair.indexed.aggregate(pipeline.clone()).unwrap()),
            render(&pair.plain.aggregate(pipeline).unwrap()),
            "{label} group t={t}"
        );
    }
}

fn walk_pages(col: &Collection, sort: &[SortSpec], page: u64) -> Vec<Ulid> {
    let mut out = Vec::new();
    loop {
        let docs = sorted(col, Filter::All, sort, out.len() as u64, Some(page));
        if docs.is_empty() {
            return out;
        }
        out.extend(docs.iter().map(|d| d.id));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The reported case, alone.
#[test]
fn a_shorter_string_with_an_ff_id_is_not_lost_behind_its_nul_extension() {
    let id = |bytes: [u8; 2]| {
        let mut b = [0u8; 16];
        b[..2].copy_from_slice(&bytes);
        b[15] = bytes[1];
        Ulid::from_bytes(b)
    };
    let rows = vec![
        Row {
            id: id([0x01, 0x00]),
            s: Some("a".into()),
            b: None,
            t: 0,
        },
        Row {
            id: id([0x01, 0x01]),
            s: Some("a\0b".into()),
            b: None,
            t: 0,
        },
        Row {
            id: id([0xFF, 0x70]),
            s: Some("a".into()),
            b: None,
            t: 0,
        },
    ];
    let pair = Pair::new(&rows);
    let ids = |f: Filter| id_set(pair.indexed.find(f).unwrap());
    let a = BTreeSet::from([rows[0].id, rows[2].id]);
    assert_eq!(ids(Filter::Eq("s".into(), str_value("a"))), a);
    assert_eq!(
        pair.indexed
            .count(Filter::Eq("s".into(), str_value("a")))
            .unwrap(),
        2
    );
    assert_eq!(
        ids(Filter::Gt("s".into(), str_value("a"))),
        BTreeSet::from([rows[1].id])
    );
    assert_eq!(ids(Filter::Lt("s".into(), str_value("a\0b"))), a);
    assert_eq!(ids(Filter::Lte("s".into(), str_value("a\0b"))).len(), 3);
    let page = sorted(
        &pair.indexed,
        Filter::All,
        &[SortSpec::asc("s")],
        0,
        Some(2),
    );
    assert_eq!(
        page.iter().map(|d| d.id).collect::<Vec<_>>(),
        vec![rows[0].id, rows[2].id]
    );
    check_every_filter(&pair, "reported");
    check_sorted(&pair, "reported");
}

#[test]
fn filters_match_a_scan_for_every_shape_of_value_and_id() {
    let pair = Pair::new(&shaped_rows());
    check_every_filter(&pair, "shaped");
}

#[test]
fn sorted_pages_match_a_scan_for_every_shape_of_value_and_id() {
    let pair = Pair::new(&shaped_rows());
    check_sorted(&pair, "shaped");
    for sort in sorts() {
        assert_eq!(
            walk_pages(&pair.indexed, &sort, 11),
            walk_pages(&pair.plain, &sort, 11),
            "deep pagination {sort:?}"
        );
    }
}

#[test]
fn updates_deletes_and_index_rebuilds_stay_consistent() {
    let pair = Pair::new(&shaped_rows());
    let tagged = Filter::Eq("u".into(), Value::Int(1));
    for (filter, label) in [
        (Filter::Gt("s".into(), str_value("a")), "gt a"),
        (Filter::Lt("s".into(), str_value("a\0b")), "lt a\\0b"),
        (Filter::Eq("s".into(), str_value("a")), "eq a"),
    ] {
        let update = Update::Set(vec![("u".into(), Value::Int(1))]);
        let n = pair
            .indexed
            .update_many(filter.clone(), update.clone())
            .unwrap();
        assert_eq!(
            n,
            pair.plain.update_many(filter, update).unwrap(),
            "update {label}"
        );
        check_filter(&pair, &tagged, &format!("tagged after {label}"));
        let clear = Update::Unset(vec!["u".into()]);
        pair.indexed
            .update_many(Filter::All, clear.clone())
            .unwrap();
        pair.plain.update_many(Filter::All, clear).unwrap();
    }

    // Move values onto and off NUL extensions, through the index.
    for (from, to) in [("a", "a\0a"), ("a\0b", "a"), ("b", "a\0")] {
        let filter = Filter::And(vec![
            Filter::Eq("s".into(), str_value(from)),
            Filter::Eq("t".into(), Value::Int(1)),
        ]);
        let update = Update::Set(vec![("s".into(), str_value(to))]);
        let n = pair
            .indexed
            .update_many(filter.clone(), update.clone())
            .unwrap();
        assert_eq!(
            n,
            pair.plain.update_many(filter, update).unwrap(),
            "{from:?} -> {to:?}"
        );
    }
    check_every_filter(&pair, "after updates");
    check_sorted(&pair, "after updates");

    for filter in [
        Filter::Lte("s".into(), str_value("a\0")),
        Filter::Gt("s".into(), str_value("b\0")),
    ] {
        let n = pair.indexed.delete_many(filter.clone()).unwrap();
        assert_eq!(n, pair.plain.delete_many(filter).unwrap(), "delete");
    }
    check_every_filter(&pair, "after deletes");
    check_sorted(&pair, "after deletes");

    pair.indexed.drop_index("s").unwrap();
    pair.indexed.create_index("s").unwrap();
    check_every_filter(&pair, "after rebuild");
    check_sorted(&pair, "after rebuild");
}

#[test]
fn a_reopened_database_answers_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v.db");
    let rows = shaped_rows();
    {
        let pair = Pair::in_db(Database::open(&path).unwrap(), &rows);
        check_sorted(&pair, "before reopen");
    }
    let db = Database::open(&path).unwrap();
    let pair = Pair {
        indexed: db.collection("indexed").unwrap(),
        plain: db.collection("plain").unwrap(),
        _db: db,
    };
    check_every_filter(&pair, "reopened");
    check_sorted(&pair, "reopened");
}

#[cfg(feature = "encryption")]
#[test]
fn an_encrypted_database_answers_the_same() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::open_encrypted(&dir.path().join("e.db"), "passphrase").unwrap();
    let pair = Pair::in_db(db, &shaped_rows());
    check_sorted(&pair, "encrypted");
    check_filter(
        &pair,
        &Filter::Gt("s".into(), str_value("a")),
        "encrypted gt a",
    );
    check_filter(
        &pair,
        &Filter::Lt("s".into(), str_value("a\0b")),
        "encrypted lt a\\0b",
    );
}

/// Seeded random collections over a tiny alphabet, so values are mostly
/// prefixes and NUL extensions of one another, with half the ids starting 0xFF.
#[test]
fn random_collections_agree_with_a_scan() {
    let mut state = 0xA076_1D64_78BD_642Fu64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..40 {
        let n = 10 + (next() % 90) as usize;
        let mut rows = Vec::with_capacity(n);
        let mut seen = BTreeSet::new();
        while rows.len() < n {
            let mut id = [0u8; 16];
            for chunk in id.chunks_mut(8) {
                chunk.copy_from_slice(&next().to_be_bytes());
            }
            match next() % 4 {
                0 | 1 => id[0] = 0xFF,
                2 => id[..2].copy_from_slice(&[0xFF, 0xFF]),
                _ => {}
            }
            let id = Ulid::from_bytes(id);
            if !seen.insert(id) {
                continue;
            }
            let word = |next: &mut dyn FnMut() -> u64, alphabet: &[u8]| -> Vec<u8> {
                let len = (next() % 5) as usize;
                (0..len)
                    .map(|_| alphabet[(next() % alphabet.len() as u64) as usize])
                    .collect()
            };
            let s = String::from_utf8(word(&mut next, b"\0ab")).unwrap();
            let b = word(&mut next, &[0x00, 0x01, 0xFF]);
            rows.push(Row {
                id,
                s: (next() % 16 != 0).then_some(s),
                b: Some(b),
                t: (next() % 3) as i64,
            });
        }
        let pair = Pair::new(&rows);
        let label = format!("round {round}");
        for _ in 0..12 {
            let v = rows[(next() % n as u64) as usize]
                .s
                .clone()
                .unwrap_or_default();
            let v = str_value(&v);
            for f in [
                Filter::Eq("s".into(), v.clone()),
                Filter::Gt("s".into(), v.clone()),
                Filter::Gte("s".into(), v.clone()),
                Filter::Lt("s".into(), v.clone()),
                Filter::Lte("s".into(), v.clone()),
            ] {
                check_filter(&pair, &f, &format!("{label} {f:?}"));
            }
            let b = Value::Bytes(rows[(next() % n as u64) as usize].b.clone().unwrap());
            check_filter(&pair, &Filter::Eq("b".into(), b), &label);
        }
        for sort in sorts() {
            for _ in 0..6 {
                let skip = next() % (n as u64 + 2);
                let limit = Some(1 + next() % 12);
                assert_eq!(
                    render(&sorted(&pair.indexed, Filter::All, &sort, skip, limit)),
                    render(&sorted(&pair.plain, Filter::All, &sort, skip, limit)),
                    "{label} {sort:?} skip {skip} limit {limit:?}"
                );
            }
        }
    }
}

/// The guard costs nothing where the reordering cannot happen, and only falls
/// back where the walk meets a value containing NUL.
#[test]
fn the_index_still_serves_sorted_pages_where_order_is_provable() {
    let n = 3_000;
    let page = |pair: &Pair, counters: &Counters, sort: &[SortSpec]| {
        counters.take();
        let got = sorted(&pair.indexed, Filter::All, sort, 20, Some(10));
        let work = counters.take();
        let want = sorted(&pair.plain, Filter::All, sort, 20, Some(10));
        counters.take();
        assert_eq!(render(&got), render(&want), "{sort:?}");
        work
    };
    let build = |rows: Vec<Row>| {
        let (db, counters) = counted_db();
        (Pair::in_db(db, &rows), counters)
    };
    let row = |id: Ulid, s: String| Row {
        id,
        s: Some(s),
        b: None,
        t: 0,
    };

    // Derived ids (about 1 in 256 start with 0xFF), strings without NUL.
    let (pair, counters) = build(
        (0..n)
            .map(|i| row(derive_doc_id("c", &i.to_string()), format!("v{i:05}")))
            .collect(),
    );
    let (keys, docs) = page(&pair, &counters, &[SortSpec::asc("s")]);
    // No key holds NUL, so the walk never probes for 0xFF ids: the page costs
    // what it did before.
    assert!(keys <= 30 + 5, "derived ids, asc: walked {keys} keys");
    assert_eq!(docs, 10, "derived ids, asc");
    let (keys, docs) = page(&pair, &counters, &[SortSpec::desc("s")]);
    assert!(
        keys >= n as u64 && keys <= n as u64 + 5,
        "derived ids, desc: walked {keys}"
    );
    assert_eq!(docs, 10, "derived ids, desc");

    // Strings with NUL, no id starting 0xFF.
    let (pair, counters) = build(
        (0..n)
            .map(|i| row(shaped_id(i * 8), format!("v{:04}\0{}", i / 3, i % 3)))
            .collect(),
    );
    for sort in [vec![SortSpec::asc("s")], vec![SortSpec::desc("s")]] {
        let (_, docs) = page(&pair, &counters, &sort);
        assert_eq!(docs, 10, "NUL strings, generated ids, {sort:?}");
    }

    // Both, but every NUL string sorts after the ascending page: the walk
    // never meets one, so the index serves it; descending meets one and scans.
    let (pair, counters) = build(
        (0..n)
            .map(|i| {
                let s = if i % 10 == 0 {
                    format!("z{i}\0")
                } else {
                    format!("v{i:05}")
                };
                let mut id = derive_doc_id("c", &i.to_string()).to_bytes();
                if i % 2 == 0 {
                    id[0] = 0xFF;
                }
                row(Ulid::from_bytes(id), s)
            })
            .collect(),
    );
    let (_, docs) = page(&pair, &counters, &[SortSpec::asc("s")]);
    assert_eq!(docs, 10, "NUL strings past the page, asc");
    let (_, docs) = page(&pair, &counters, &[SortSpec::desc("s")]);
    assert!(
        docs >= n as u64,
        "NUL strings, desc: read {docs} documents, expected a scan"
    );
}
