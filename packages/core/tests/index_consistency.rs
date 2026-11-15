//! v0.13.1 M2 — transaction and index consistency.
//!
//! Each test states one invariant and checks it against an independent answer:
//! an unindexed twin collection, an index rebuilt from the final documents, a
//! brute-force ranking of the committed vectors, or the exported snapshot taken
//! before a failed write. Narrower checks already live elsewhere
//! (`varlen_index_order`, `fts_stats_integrity`, `vector_cache`,
//! `vector_graph_cache`, `stabilization`); these cover the combinations they
//! leave out — mixed value types with compound indexes, raw incremental index
//! state against a rebuild, and failures that happen after every index kind
//! has already done its part of the write.

use std::collections::{BTreeSet, HashMap};

use taladb::bm25::Bm25Params;
use taladb::fts::{
    FTS_STATS_KEY, HybridQuery, fts_len_table_name, fts_stats_table_name, fts_table_name,
};
use taladb::index::{compound_table_name, index_table_name, meta_key};

/// `taladb::index::META_INDEX_ARRAYS_TABLE` is crate-private; its name is part
/// of the on-disk format, so spelling it here cannot drift silently.
const META_INDEX_ARRAYS_TABLE: &str = "meta::index_arrays";
use taladb::{
    Collection, Database, Document, Filter, HnswOptions, TalaDbError, Ulid, Update, Value,
    VectorQueryOptions, VectorSearchMode,
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

type Rows = Vec<(Vec<u8>, Vec<u8>)>;

fn dump(db: &Database, table: &str) -> Rows {
    db.backend()
        .begin_read()
        .unwrap()
        .scan_all(table)
        .unwrap_or_default()
}

fn get_raw(db: &Database, table: &str, key: &[u8]) -> Option<Vec<u8>> {
    db.backend().begin_read().unwrap().get(table, key).unwrap()
}

/// Ids spread over the key space, so index order and id order differ.
fn id_of(i: u128) -> Ulid {
    Ulid::from(i.wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835) | 1)
}

fn with_id(i: u128, mut fields: Vec<(String, Value)>) -> Vec<(String, Value)> {
    fields.push(("_id".into(), Value::Str(id_of(i).to_string())));
    fields
}

fn ids(docs: &[Document]) -> BTreeSet<Ulid> {
    docs.iter().map(|d| d.id).collect()
}

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

fn vector(v: &[f32]) -> Value {
    Value::Array(v.iter().map(|x| Value::Float(f64::from(*x))).collect())
}

fn as_f32(v: &Value) -> Option<Vec<f32>> {
    match v {
        Value::Array(items) => items
            .iter()
            .map(|x| match x {
                Value::Float(f) => Some(*f as f32),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f32 {
        (self.next() % 20_001) as f32 / 10_000.0 - 1.0
    }
    fn vec4(&mut self) -> [f32; 4] {
        [self.unit(), self.unit(), self.unit(), self.unit()]
    }
}

fn mode(mode: VectorSearchMode) -> VectorQueryOptions {
    VectorQueryOptions {
        mode,
        ..Default::default()
    }
}

/// Text ranking as `(id, score bits)`: equality means the same documents in
/// the same order with bit-identical scores.
fn text_ranking(col: &Collection, query: &str, filter: Option<Filter>) -> Vec<(Ulid, u32)> {
    col.search_text_with("body", query, 1000, &Bm25Params::default(), filter)
        .unwrap()
        .into_iter()
        .map(|r| (r.document.id, r.score.to_bits()))
        .collect()
}

fn vector_ranking(
    col: &Collection,
    q: &[f32],
    k: usize,
    filter: Option<Filter>,
    m: VectorSearchMode,
) -> Vec<(Ulid, u32)> {
    let result = col.search_vectors("v", q, k, filter, &mode(m)).unwrap();
    if m == VectorSearchMode::Ann {
        assert_eq!(result.execution.path, "hnsw");
    }
    result
        .hits
        .into_iter()
        .map(|h| (h.document.id, h.score.to_bits()))
        .collect()
}

/// One mutation expected to fail, run on demand.
type Attempt<'a> = Box<dyn Fn() -> Result<(), TalaDbError> + 'a>;

// ---------------------------------------------------------------------------
// Test A — a write that fails after every index kind has done its part
// ---------------------------------------------------------------------------

/// `write_docs_and_indexes` maintains bodies, scalar indexes, FTS postings and
/// statistics, the vector table and its HNSW graph, and only then compound
/// indexes. A document with arrays in two members of one compound index is
/// rejected at that last step, so every earlier step has already written into
/// the transaction. None of it may survive, nothing may announce it, and no
/// cache warmed before the failure may answer differently after it.
#[test]
fn a_late_failure_rolls_back_every_index_and_announces_nothing() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.create_index("n").unwrap();
    col.create_fts_index("body").unwrap();
    col.create_vector_index(
        "v",
        4,
        None,
        Some(HnswOptions {
            m: 8,
            ef_construction: 64,
        }),
    )
    .unwrap();
    col.create_compound_index(&["tags", "cat"]).unwrap();

    let mut rng = Rng(0x5EED);
    // 300 documents: more than two write batches (128 each). Only the last
    // one, n = 299, has an array in `tags`, so an update giving every
    // document an array `cat` fails in the final batch, after the first two
    // have been fully applied.
    let rows: Vec<_> = (0..300u128)
        .map(|i| {
            with_id(
                i,
                vec![
                    ("n".into(), Value::Int(i as i64)),
                    ("body".into(), s(&format!("rust storage doc{i}"))),
                    ("v".into(), vector(&rng.vec4())),
                    (
                        "tags".into(),
                        if i == 299 {
                            Value::Array(vec![s("t")])
                        } else {
                            s("t")
                        },
                    ),
                    ("cat".into(), s(&format!("c{}", i % 3))),
                ],
            )
        })
        .collect();
    col.insert_many(rows).unwrap();

    let q = [0.3, -0.2, 0.9, 0.1];
    let observe = |col: &Collection| {
        (
            text_ranking(col, "rust doc7 words", None),
            vector_ranking(col, &q, 10, None, VectorSearchMode::Exact),
            vector_ranking(col, &q, 10, None, VectorSearchMode::Ann),
            vector_ranking(
                col,
                &q,
                10,
                Some(Filter::Eq("cat".into(), s("c1"))),
                VectorSearchMode::Exact,
            ),
            ids(&col.find(Filter::Gte("n".into(), Value::Int(290))).unwrap()),
            col.count(Filter::Eq("cat".into(), s("c2"))).unwrap(),
            col.vector_index_status("v").unwrap().revision,
        )
    };
    // Twice: the second read is served from the warmed caches.
    let expected = observe(&col);
    assert_eq!(observe(&col), expected);
    let snapshot = db.export_snapshot().unwrap();
    let generation = col.write_generation();
    let watch = col.watch(Filter::All);

    let invalid_doc = |i: u128| {
        with_id(
            i,
            vec![
                ("n".into(), Value::Int(-1)),
                ("body".into(), s("brand new words")),
                ("v".into(), vector(&[1.0, 0.0, 0.0, 0.0])),
                ("tags".into(), Value::Array(vec![s("x")])),
                ("cat".into(), Value::Array(vec![s("y")])),
            ],
        )
    };
    let array_cat = Update::Set(vec![
        ("n".into(), Value::Int(-5)),
        ("body".into(), s("brand new words")),
        ("v".into(), vector(&[0.0, 1.0, 0.0, 0.0])),
        ("cat".into(), Value::Array(vec![s("z")])),
    ]);
    let mut replacement = Document::with_id(id_of(299), invalid_doc(299));
    replacement.fields.retain(|(k, _)| k != "_id");

    let attempts: Vec<(&str, Attempt<'_>)> = vec![
        (
            "insert",
            Box::new(|| col.insert(invalid_doc(1000)).map(drop)),
        ),
        (
            "insert_many, last document invalid",
            Box::new(|| {
                col.insert_many(vec![
                    with_id(1001, vec![("body".into(), s("brand new words"))]),
                    invalid_doc(1002),
                ])
                .map(drop)
            }),
        ),
        (
            "update_one",
            Box::new(|| {
                col.update_one(Filter::Eq("n".into(), Value::Int(299)), array_cat.clone())
                    .map(drop)
            }),
        ),
        (
            "update_many, failing in its last batch",
            Box::new(|| {
                col.update_many(Filter::Gte("n".into(), Value::Int(0)), array_cat.clone())
                    .map(drop)
            }),
        ),
        (
            "replace_with_id",
            Box::new(|| col.replace_with_id(replacement.clone()).map(drop)),
        ),
    ];
    for (label, attempt) in &attempts {
        let result = attempt();
        assert!(
            matches!(result, Err(TalaDbError::CompoundIndexMultipleArrays)),
            "{label}: expected the compound-index rejection, got {result:?}"
        );
        assert!(
            db.export_snapshot().unwrap() == snapshot,
            "{label}: a failed write changed stored state"
        );
        assert_eq!(col.write_generation(), generation, "{label}");
        assert!(watch.try_next().unwrap().is_none(), "{label}: watch fired");
        assert_eq!(observe(&col), expected, "{label}: reads changed");
    }

    // The same collection still accepts a valid write, and announces it.
    col.update_one(
        Filter::Eq("n".into(), Value::Int(7)),
        Update::Set(vec![("body".into(), s("brand new words"))]),
    )
    .unwrap();
    assert_eq!(col.write_generation(), generation + 1);
    assert!(watch.try_next().unwrap().is_some());
    assert_ne!(observe(&col).0, expected.0);
}

// ---------------------------------------------------------------------------
// Test B1 — scalar and compound indexes change the plan, never the answer
// ---------------------------------------------------------------------------

/// Values chosen for the encodings that differ: null against missing, every
/// numeric shape including both zeros and an integer equal to a float, arrays
/// (empty, mixed, containing null), strings that look numeric, and booleans.
fn mixed_values() -> Vec<Option<Value>> {
    vec![
        None,
        Some(Value::Null),
        Some(Value::Bool(false)),
        Some(Value::Bool(true)),
        Some(Value::Int(-1)),
        Some(Value::Int(0)),
        Some(Value::Int(1)),
        Some(Value::Int(2)),
        Some(Value::Int(i64::MAX)),
        Some(Value::Float(-0.0)),
        Some(Value::Float(0.0)),
        Some(Value::Float(1.0)),
        Some(Value::Float(1.5)),
        Some(s("")),
        Some(s("1")),
        Some(s("a")),
        Some(Value::Array(vec![])),
        Some(Value::Array(vec![Value::Int(1), s("a")])),
        Some(Value::Array(vec![Value::Float(2.0), Value::Null])),
        Some(Value::Array(vec![Value::Bool(true), Value::Int(1)])),
    ]
}

struct Twin {
    db: Database,
    indexed: Collection,
    plain: Collection,
}

impl Twin {
    fn new() -> Self {
        let db = Database::open_in_memory().unwrap();
        let indexed = db.collection("indexed").unwrap();
        let plain = db.collection("plain").unwrap();
        indexed.create_index("x").unwrap();
        indexed.create_index("y").unwrap();
        indexed.create_compound_index(&["x", "y"]).unwrap();
        indexed.create_compound_index(&["y", "x"]).unwrap();
        Self { db, indexed, plain }
    }

    fn both<T: PartialEq + std::fmt::Debug>(&self, label: &str, f: impl Fn(&Collection) -> T) -> T {
        let a = f(&self.indexed);
        let b = f(&self.plain);
        assert_eq!(a, b, "{label}: indexed and unindexed answers differ");
        a
    }

    /// Documents compared by id and user fields; `_changed_at` is a clock.
    fn state(col: &Collection) -> Vec<(Ulid, Vec<(String, Value)>)> {
        let mut out: Vec<_> = col
            .find(Filter::All)
            .unwrap()
            .into_iter()
            .map(|d| {
                let mut f: Vec<_> = d
                    .fields
                    .into_iter()
                    .filter(|(k, _)| k != "_changed_at")
                    .collect();
                f.sort_by(|a, b| a.0.cmp(&b.0));
                (d.id, f)
            })
            .collect();
        out.sort_by_key(|(id, _)| *id);
        out
    }

    fn check(&self, label: &str) {
        assert_eq!(
            Self::state(&self.indexed),
            Self::state(&self.plain),
            "{label}: twins diverged"
        );
        let probes: Vec<Value> = mixed_values().into_iter().flatten().collect();
        let mut filters = vec![
            Filter::Exists("x".into(), true),
            Filter::Exists("x".into(), false),
        ];
        for p in &probes {
            for f in [
                Filter::Eq("x".into(), p.clone()),
                Filter::Ne("x".into(), p.clone()),
                Filter::Gt("x".into(), p.clone()),
                Filter::Gte("x".into(), p.clone()),
                Filter::Lt("x".into(), p.clone()),
                Filter::Lte("x".into(), p.clone()),
                Filter::Not(Box::new(Filter::Eq("x".into(), p.clone()))),
            ] {
                filters.push(f);
            }
        }
        for pair in probes.windows(2) {
            filters.push(Filter::In("x".into(), pair.to_vec()));
            filters.push(Filter::Nin("x".into(), pair.to_vec()));
            // Equality on both members: the compound plan, in both orders.
            filters.push(Filter::And(vec![
                Filter::Eq("x".into(), pair[0].clone()),
                Filter::Eq("y".into(), pair[1].clone()),
            ]));
            filters.push(Filter::And(vec![
                Filter::Eq("y".into(), pair[0].clone()),
                Filter::Eq("x".into(), pair[1].clone()),
            ]));
            filters.push(Filter::Or(vec![
                Filter::Eq("x".into(), pair[0].clone()),
                Filter::Gt("y".into(), pair[1].clone()),
            ]));
            filters.push(Filter::And(vec![
                Filter::Gte("x".into(), pair[0].clone()),
                Filter::Lt("x".into(), pair[1].clone()),
            ]));
        }
        for f in filters {
            let l = format!("{label} {f:?}");
            let found = self.both(&l, |c| ids(&c.find(f.clone()).unwrap()));
            self.both(&l, |c| c.count(f.clone()).unwrap());
            // find_one's choice among several matches is not specified; only
            // whether it finds one, and that the one it finds matches.
            for c in [&self.indexed, &self.plain] {
                let one = c.find_one(f.clone()).unwrap();
                assert_eq!(one.is_some(), !found.is_empty(), "{l} find_one");
                assert!(one.is_none_or(|d| found.contains(&d.id)), "{l} find_one");
            }
        }
    }
}

#[test]
fn scalar_and_compound_indexes_agree_with_a_scan_through_mutations_and_rebuilds() {
    let twin = Twin::new();
    let values = mixed_values();
    let mut docs = Vec::new();
    let mut i = 0u128;
    for x in &values {
        for y in [
            None,
            Some(Value::Int(1)),
            Some(Value::Float(1.0)),
            Some(s("a")),
        ] {
            let mut f = vec![("k".into(), Value::Int(i as i64))];
            if let Some(x) = x {
                f.push(("x".into(), x.clone()));
            }
            if let Some(y) = y {
                f.push(("y".into(), y));
            }
            docs.push(with_id(i, f));
            i += 1;
        }
    }
    twin.indexed.insert_many(docs.clone()).unwrap();
    twin.plain.insert_many(docs).unwrap();
    twin.check("initial");

    // Mutation selection goes through the planner too: both twins must pick
    // the same documents and end in the same state.
    let steps: Vec<(Filter, Update)> = vec![
        (
            Filter::Eq("x".into(), Value::Int(1)),
            Update::Set(vec![("x".into(), Value::Float(1.0))]),
        ),
        (
            Filter::Gte("x".into(), Value::Float(1.5)),
            Update::Unset(vec!["x".into()]),
        ),
        (
            Filter::Exists("x".into(), false),
            Update::Set(vec![("x".into(), Value::Null)]),
        ),
        // Scalars become arrays, then those arrays grow: `$push` onto a
        // scalar is a type error by design, so it is selected via an array.
        (
            Filter::In("x".into(), vec![Value::Int(0), s("a")]),
            Update::Set(vec![("x".into(), Value::Array(vec![s("a"), Value::Null]))]),
        ),
        (
            Filter::Eq("x".into(), s("a")),
            Update::Push("x".into(), Value::Int(2)),
        ),
        (
            Filter::Eq("x".into(), Value::Int(2)),
            Update::Pull("x".into(), Value::Int(2)),
        ),
        (
            Filter::And(vec![
                Filter::Eq("x".into(), Value::Null),
                Filter::Eq("y".into(), Value::Int(1)),
            ]),
            Update::Set(vec![
                ("y".into(), s("moved")),
                ("x".into(), Value::Bool(true)),
            ]),
        ),
        (
            Filter::Lt("x".into(), Value::Int(1)),
            Update::Inc(vec![("x".into(), Value::Int(10))]),
        ),
    ];
    for (n, (filter, update)) in steps.into_iter().enumerate() {
        let label = format!("step {n} {filter:?}");
        twin.both(&label, |c| {
            c.update_many(filter.clone(), update.clone()).unwrap()
        });
        twin.check(&label);
    }
    // Single-document mutations on a filter that matches one document.
    for k in [3i64, 17, 40] {
        let f = Filter::And(vec![
            Filter::Eq("k".into(), Value::Int(k)),
            Filter::Exists("x".into(), true),
        ]);
        twin.both("update_one", |c| {
            c.update_one(f.clone(), Update::Set(vec![("x".into(), s("one"))]))
                .unwrap()
        });
        twin.both("delete_one", |c| c.delete_one(f.clone()).unwrap());
    }
    for f in [
        Filter::Eq("x".into(), Value::Float(0.0)),
        Filter::In("x".into(), vec![Value::Bool(false), s("1")]),
    ] {
        twin.both("delete_many", |c| c.delete_many(f.clone()).unwrap());
    }
    twin.check("after mutations");

    // Incremental maintenance must leave exactly what a rebuild from the
    // final documents writes: same keys, same array counters.
    let tables = [
        index_table_name("indexed", "x"),
        index_table_name("indexed", "y"),
        compound_table_name("indexed", &["x", "y"]),
        compound_table_name("indexed", &["y", "x"]),
    ];
    let arrays = |f: &str| {
        get_raw(
            &twin.db,
            META_INDEX_ARRAYS_TABLE,
            meta_key("indexed", f).as_bytes(),
        )
    };
    let maintained: Vec<Rows> = tables.iter().map(|t| dump(&twin.db, t)).collect();
    let maintained_arrays = (arrays("x"), arrays("y"));
    assert!(maintained.iter().all(|t| !t.is_empty()));
    twin.indexed.drop_index("x").unwrap();
    twin.indexed.drop_index("y").unwrap();
    twin.indexed.drop_compound_index(&["x", "y"]).unwrap();
    twin.indexed.drop_compound_index(&["y", "x"]).unwrap();
    twin.check("indexes dropped");
    twin.indexed.create_index("x").unwrap();
    twin.indexed.create_index("y").unwrap();
    twin.indexed.create_compound_index(&["x", "y"]).unwrap();
    twin.indexed.create_compound_index(&["y", "x"]).unwrap();
    for (t, before) in tables.iter().zip(&maintained) {
        assert!(
            dump(&twin.db, t) == *before,
            "{t}: maintained index differs from a rebuild"
        );
    }
    assert_eq!((arrays("x"), arrays("y")), maintained_arrays);
    twin.check("after rebuild");
}

// ---------------------------------------------------------------------------
// Test B2 — incrementally maintained BM25 equals a rebuild
// ---------------------------------------------------------------------------

#[test]
fn incremental_fts_state_and_rankings_equal_a_rebuild_from_the_final_documents() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    let plain = db.collection("plain").unwrap();
    col.create_fts_index("body").unwrap();

    let bodies: Vec<Option<Value>> = vec![
        Some(s("Rust storage engine with rust bindings")),
        Some(s("")),
        Some(s("!! ?? --")),
        Some(s("a b c")), // tokenless: every token is one byte
        Some(s("Straße ÉCOLE naïve café")),
        Some(s("rust rust rust rust")),
        Some(s("storage")),
        Some(Value::Int(5)),
        Some(Value::Null),
        None,
        Some(s("vector search and full text search")),
        Some(s("the quick brown fox")),
    ];
    let rows: Vec<_> = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let mut f = vec![("k".into(), Value::Int(i as i64))];
            if let Some(b) = b {
                f.push(("body".into(), b.clone()));
            }
            with_id(i as u128, f)
        })
        .collect();
    col.insert_many(rows[..6].to_vec()).unwrap();
    for row in &rows[6..] {
        col.insert(row.clone()).unwrap();
    }
    plain.insert_many(rows).unwrap();

    let k = |n: i64| Filter::Eq("k".into(), Value::Int(n));
    let steps: Vec<(Filter, Update)> = vec![
        // text → other text, sharing some tokens
        (
            k(0),
            Update::Set(vec![("body".into(), s("storage engine for search"))]),
        ),
        // text → empty, → tokenless, → non-string, → removed
        (k(6), Update::Set(vec![("body".into(), s(""))])),
        (k(10), Update::Set(vec![("body".into(), s("? !"))])),
        (k(11), Update::Set(vec![("body".into(), Value::Int(1))])),
        (k(5), Update::Unset(vec!["body".into()])),
        // non-string / missing / empty / tokenless → text
        (k(7), Update::Set(vec![("body".into(), s("fox search"))])),
        (k(9), Update::Set(vec![("body".into(), s("rust"))])),
        (k(1), Update::Set(vec![("body".into(), s("quick rust"))])),
        (k(3), Update::Set(vec![("body".into(), s("brown fox"))])),
        // unrelated field only
        (Filter::All, Update::Set(vec![("tag".into(), s("t"))])),
    ];
    for (filter, update) in &steps {
        assert_eq!(
            col.update_many(filter.clone(), update.clone()).unwrap(),
            plain.update_many(filter.clone(), update.clone()).unwrap()
        );
    }
    let mut replacement = col.find_by_id(id_of(4)).unwrap().unwrap();
    replacement.set("body", s("café search rust"));
    col.replace_with_id(replacement.clone()).unwrap();
    plain.replace_with_id(replacement).unwrap();
    for f in [k(2), k(8)] {
        assert!(col.delete_one(f.clone()).unwrap());
        assert!(plain.delete_one(f).unwrap());
    }
    col.insert(with_id(100, vec![("body".into(), s("late rust arrival"))]))
        .unwrap();
    plain
        .insert(with_id(100, vec![("body".into(), s("late rust arrival"))]))
        .unwrap();

    let queries = [
        "rust",
        "storage search",
        "fox quick brown",
        "café straße",
        "engine",
        "late arrival rust",
        "nothing matches",
    ];
    let rank_all = || -> Vec<Vec<(Ulid, u32)>> {
        queries
            .iter()
            .flat_map(|q| {
                [
                    text_ranking(&col, q, None),
                    text_ranking(&col, q, Some(Filter::Gt("k".into(), Value::Int(3)))),
                ]
            })
            .collect()
    };
    let contains = |c: &Collection| -> Vec<BTreeSet<Ulid>> {
        queries
            .iter()
            .map(|q| {
                ids(&c
                    .find(Filter::Contains("body".into(), (*q).into()))
                    .unwrap())
            })
            .collect()
    };
    let tables = [
        fts_table_name("docs", "body"),
        fts_len_table_name("docs", "body"),
    ];
    let maintained: Vec<Rows> = tables.iter().map(|t| dump(&db, t)).collect();
    let maintained_stats = get_raw(&db, &fts_stats_table_name("docs", "body"), FTS_STATS_KEY);
    let maintained_ranking = rank_all();
    // `$contains` through the index equals the unindexed matcher.
    assert_eq!(contains(&col), contains(&plain));

    col.drop_fts_index("body").unwrap();
    col.create_fts_index("body").unwrap();
    for (t, before) in tables.iter().zip(&maintained) {
        assert!(
            dump(&db, t) == *before,
            "{t}: maintained postings differ from a rebuild"
        );
    }
    assert_eq!(
        get_raw(&db, &fts_stats_table_name("docs", "body"), FTS_STATS_KEY),
        maintained_stats,
        "corpus statistics differ from a rebuild"
    );
    assert_eq!(rank_all(), maintained_ranking, "rankings or scores differ");
    assert_eq!(contains(&col), contains(&plain));
}

// ---------------------------------------------------------------------------
// Tests C + D — cross-index mutations against brute force, with warm caches
// ---------------------------------------------------------------------------

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (na * nb)
}

/// Exact top-k ids computed from the committed documents themselves.
fn brute_force(col: &Collection, q: &[f32], k: usize, cat: Option<&str>) -> Vec<Ulid> {
    let mut scored: Vec<(Ulid, f32)> = col
        .find(Filter::All)
        .unwrap()
        .into_iter()
        .filter(|d| cat.is_none_or(|c| d.get("cat") == Some(&s(c))))
        .filter_map(|d| Some((d.id, cosine(q, &as_f32(d.get("v")?)?))))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().take(k).map(|(id, _)| id).collect()
}

struct Observed {
    text: Vec<Vec<(Ulid, u32)>>,
    vectors: Vec<Vec<(Ulid, u32)>>,
}

fn verify(col: &Collection, queries: &[[f32; 4]], label: &str) -> Observed {
    let live: HashMap<Ulid, Document> = col
        .find(Filter::All)
        .unwrap()
        .into_iter()
        .map(|d| (d.id, d))
        .collect();
    let mut vectors = Vec::new();
    for q in queries {
        for cat in [None, Some("c1")] {
            let filter = cat.map(|c| Filter::Eq("cat".into(), s(c)));
            // Twice, so the second answer comes from the warmed block cache.
            let first = vector_ranking(col, q, 8, filter.clone(), VectorSearchMode::Exact);
            let exact = vector_ranking(col, q, 8, filter.clone(), VectorSearchMode::Exact);
            assert_eq!(first, exact, "{label}: cached exact answer differs");
            let exact_ids: Vec<Ulid> = exact.iter().map(|h| h.0).collect();
            assert_eq!(
                exact_ids,
                brute_force(col, q, 8, cat),
                "{label}: exact != brute force"
            );
            vectors.push(exact);
        }
        // ANN is approximate: its hits must be live, carry the exact score of
        // the committed vector, and recall the exact top-k well.
        let truth: BTreeSet<Ulid> = vectors[vectors.len() - 2].iter().map(|h| h.0).collect();
        for _ in 0..2 {
            let hits = vector_ranking(col, q, 8, None, VectorSearchMode::Ann);
            for (id, bits) in &hits {
                let doc = live
                    .get(id)
                    .unwrap_or_else(|| panic!("{label}: ANN returned a deleted document"));
                let expected = cosine(q, &as_f32(doc.get("v").unwrap()).unwrap());
                assert!(
                    (f32::from_bits(*bits) - expected).abs() < 1e-4,
                    "{label}: stale ANN score"
                );
            }
            let found = hits.iter().filter(|h| truth.contains(&h.0)).count();
            assert!(
                found * 10 >= truth.len() * 9,
                "{label}: ANN recall {found}/{}",
                truth.len()
            );
        }
    }
    let mut text = Vec::new();
    for query in ["rust storage", "vector", "alpha beta"] {
        for filter in [None, Some(Filter::Eq("cat".into(), s("c1")))] {
            let ranking = text_ranking(col, query, filter.clone());
            let tokens: Vec<String> = query.split(' ').map(str::to_string).collect();
            for (id, _) in &ranking {
                let doc = live
                    .get(id)
                    .unwrap_or_else(|| panic!("{label}: text hit was deleted"));
                let Some(Value::Str(body)) = doc.get("body") else {
                    panic!("{label}: text hit without text");
                };
                let lower = body.to_lowercase();
                assert!(
                    tokens.iter().any(|t| lower.contains(t.as_str())),
                    "{label}: stale posting"
                );
                if filter.is_some() {
                    assert_eq!(doc.get("cat"), Some(&s("c1")), "{label}: filter ignored");
                }
            }
            text.push(ranking);
        }
        // Hybrid must be the fusion of the two current rankings.
        let q = queries[0];
        for filter in [None, Some(Filter::Eq("cat".into(), s("c1")))] {
            let mut hq = HybridQuery::new("body", query, "v", &q, 5);
            hq.filter = filter.clone();
            let pool = 20;
            let text_side = text_ranking(col, query, filter.clone());
            let vector_side: Vec<Ulid> = col
                .search_vectors(
                    "v",
                    &q,
                    pool,
                    filter.clone(),
                    &VectorQueryOptions::default(),
                )
                .unwrap()
                .hits
                .into_iter()
                .map(|h| h.document.id)
                .collect();
            for hit in col.hybrid_search(hq).unwrap() {
                let id = hit.document.id;
                assert!(
                    live.contains_key(&id),
                    "{label}: hybrid returned a deleted document"
                );
                assert_eq!(
                    hit.text_rank,
                    text_side.iter().take(pool).position(|h| h.0 == id),
                    "{label}: hybrid text rank"
                );
                assert_eq!(
                    hit.vector_rank,
                    vector_side.iter().position(|h| *h == id),
                    "{label}: hybrid vector rank"
                );
            }
        }
    }
    Observed { text, vectors }
}

#[test]
fn mutations_to_one_field_keep_every_index_current_and_leave_the_others_alone() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.create_index("cat").unwrap();
    col.create_fts_index("body").unwrap();
    col.create_vector_index(
        "v",
        4,
        None,
        Some(HnswOptions {
            m: 8,
            ef_construction: 64,
        }),
    )
    .unwrap();
    let mut rng = Rng(0xC0FFEE);
    let words = ["rust", "storage", "vector", "alpha", "beta", "gamma"];
    let rows: Vec<_> = (0..60u128)
        .map(|i| {
            let body = format!(
                "{} {} {}",
                words[i as usize % 6],
                words[(i as usize / 6) % 6],
                words[(i as usize * 7) % 6]
            );
            with_id(
                i,
                vec![
                    ("n".into(), Value::Int(i as i64)),
                    ("cat".into(), s(&format!("c{}", i % 3))),
                    ("body".into(), s(&body)),
                    ("v".into(), vector(&rng.vec4())),
                ],
            )
        })
        .collect();
    col.insert_many(rows).unwrap();
    let queries = [rng.vec4(), rng.vec4(), [1.0, 0.0, 0.0, 0.0]];
    let n = |lo: i64, hi: i64| {
        Filter::And(vec![
            Filter::Gte("n".into(), Value::Int(lo)),
            Filter::Lt("n".into(), Value::Int(hi)),
        ])
    };
    let status = |col: &Collection| {
        let s = col.vector_index_status("v").unwrap();
        assert_eq!(s.state, "ready");
        s.revision
    };

    let base = verify(&col, &queries, "initial");
    let revision = status(&col);

    // Metadata only: no text or vector ranking may move, and the graph keeps
    // its revision (metadata writes do not touch the vector table).
    col.update_many(n(0, 30), Update::Set(vec![("cat".into(), s("c1"))]))
        .unwrap();
    let meta = verify(&col, &queries, "metadata update");
    assert_eq!(status(&col), revision);
    // Unfiltered rankings (every even entry) are unaffected by `cat`.
    for (a, b) in base.text.iter().zip(&meta.text).step_by(2) {
        assert_eq!(a, b, "metadata update moved a text ranking");
    }
    for (a, b) in base.vectors.iter().zip(&meta.vectors).step_by(2) {
        assert_eq!(a, b, "metadata update moved a vector ranking");
    }

    // Text only: vector rankings are untouched.
    col.update_many(
        n(10, 40),
        Update::Set(vec![("body".into(), s("vector rust rust"))]),
    )
    .unwrap();
    let text = verify(&col, &queries, "text update");
    assert_eq!(
        text.vectors, meta.vectors,
        "a text update moved a vector ranking"
    );
    assert_eq!(status(&col), revision);

    // Vectors only: text rankings are untouched, and the graph follows.
    let moved: Vec<[f32; 4]> = (0..15).map(|_| rng.vec4()).collect();
    for (i, v) in moved.iter().enumerate() {
        col.update_one(
            Filter::Eq("n".into(), Value::Int(i as i64 * 3)),
            Update::Set(vec![("v".into(), vector(v))]),
        )
        .unwrap();
    }
    let vec_update = verify(&col, &queries, "vector update");
    assert_eq!(
        vec_update.text, text.text,
        "a vector update moved a text ranking"
    );
    assert!(status(&col) > revision);

    // Delete the current leaders of every ranking, then add new documents.
    let mut leaders: BTreeSet<Ulid> = vec_update
        .vectors
        .iter()
        .chain(&vec_update.text)
        .filter_map(|r| r.first().map(|h| h.0))
        .collect();
    leaders.insert(id_of(59));
    for id in &leaders {
        assert!(col.delete_by_id(*id).unwrap());
    }
    col.insert_many(
        (200..210u128)
            .map(|i| {
                with_id(
                    i,
                    vec![
                        ("n".into(), Value::Int(i as i64)),
                        ("cat".into(), s("c1")),
                        ("body".into(), s("alpha vector storage")),
                        ("v".into(), vector(&rng.vec4())),
                    ],
                )
            })
            .collect(),
    )
    .unwrap();
    let after_churn = verify(&col, &queries, "deletes and inserts");
    let live = ids(&col.find(Filter::All).unwrap());
    assert!(leaders.iter().all(|id| !live.contains(id)));

    // A failed mixed write (valid text, wrong-dimension vector) leaves all of
    // it exactly as it was.
    assert!(
        col.update_many(
            n(0, 300),
            Update::Set(vec![
                ("body".into(), s("gamma gamma")),
                ("cat".into(), s("c2")),
                ("v".into(), vector(&[1.0, 2.0])),
            ]),
        )
        .is_err()
    );
    let failed = verify(&col, &queries, "failed write");
    assert_eq!(failed.text, after_churn.text);
    assert_eq!(failed.vectors, after_churn.vectors);

    // Replacing one index leaves the others' answers alone.
    col.drop_fts_index("body").unwrap();
    col.create_fts_index("body").unwrap();
    let fts_rebuilt = verify(&col, &queries, "fts rebuilt");
    assert_eq!(fts_rebuilt.text, after_churn.text);
    assert_eq!(fts_rebuilt.vectors, after_churn.vectors);
    col.rebuild_vector_index("v", None).unwrap();
    let graph_rebuilt = verify(&col, &queries, "graph rebuilt");
    assert_eq!(graph_rebuilt.text, after_churn.text);
    assert_eq!(graph_rebuilt.vectors, after_churn.vectors);
}
