//! Behavioural coverage for the query/write fast paths.
//!
//! Every optimisation here replaced a straightforward implementation with a
//! sharper one — index intersection instead of scan-and-post-filter, an early
//! exit instead of materialise-then-truncate, batched index maintenance instead
//! of per-document. These tests pin the *observable* behaviour so the fast path
//! can never quietly disagree with the slow one it replaced.

use taladb_core::query::options::{FindOptions, SortSpec};
use taladb_core::{Database, Filter, Update, Value};

fn db() -> Database {
    Database::open_in_memory().unwrap()
}

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

// ---------------------------------------------------------------------------
// IndexAnd — multiple indexed equality conjuncts are intersected
// ---------------------------------------------------------------------------

/// The shape the intersection plan exists for: two indexed equality conjuncts,
/// one of which is barely selective. Whatever plan is chosen, the answer must be
/// exactly the documents satisfying both.
#[test]
fn index_and_matches_both_conjuncts() {
    let db = db();
    let col = db.collection("users").unwrap();
    col.create_index("status").unwrap();
    col.create_index("team").unwrap();

    let mut items = Vec::new();
    for i in 0..300 {
        items.push(vec![
            (
                "status".into(),
                s(if i % 10 == 0 { "archived" } else { "active" }),
            ),
            ("team".into(), s(if i % 50 == 0 { "core" } else { "other" })),
            ("n".into(), Value::Int(i)),
        ]);
    }
    col.insert_many(items).unwrap();

    let hits = col
        .find(Filter::And(vec![
            Filter::Eq("status".into(), s("active")),
            Filter::Eq("team".into(), s("core")),
        ]))
        .unwrap();

    // i % 50 == 0 gives 0,50,100,150,200,250; of those i % 10 == 0 for all,
    // so every "core" row is also "archived" — the intersection is empty.
    assert!(
        hits.is_empty(),
        "expected no active+core rows, got {}",
        hits.len()
    );

    // The complementary query must find all six.
    let archived_core = col
        .find(Filter::And(vec![
            Filter::Eq("status".into(), s("archived")),
            Filter::Eq("team".into(), s("core")),
        ]))
        .unwrap();
    assert_eq!(archived_core.len(), 6);
}

/// An intersection must not lose documents that only one branch's index knows
/// about, and must agree with the unindexed answer.
#[test]
fn index_and_agrees_with_unindexed_result() {
    let indexed = db();
    let plain = db();
    let a = indexed.collection("t").unwrap();
    let b = plain.collection("t").unwrap();
    a.create_index("x").unwrap();
    a.create_index("y").unwrap();

    let items: Vec<Vec<(String, Value)>> = (0..200)
        .map(|i| {
            vec![
                ("x".into(), Value::Int(i % 7)),
                ("y".into(), Value::Int(i % 5)),
            ]
        })
        .collect();
    a.insert_many(items.clone()).unwrap();
    b.insert_many(items).unwrap();

    let filter = Filter::And(vec![
        Filter::Eq("x".into(), Value::Int(3)),
        Filter::Eq("y".into(), Value::Int(2)),
    ]);
    let mut got: Vec<i64> = a
        .find(filter.clone())
        .unwrap()
        .iter()
        .filter_map(|d| d.get("x").and_then(taladb_core::Value::as_int))
        .collect();
    let mut want: Vec<i64> = b
        .find(filter)
        .unwrap()
        .iter()
        .filter_map(|d| d.get("x").and_then(taladb_core::Value::as_int))
        .collect();
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want);
    assert!(!got.is_empty(), "test would be vacuous with no matches");
}

/// A conjunct with no matches short-circuits the whole `$and`.
#[test]
fn index_and_with_empty_branch_returns_nothing() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.create_index("a").unwrap();
    col.create_index("b").unwrap();
    col.insert_many(vec![
        vec![("a".into(), Value::Int(1)), ("b".into(), Value::Int(1))],
        vec![("a".into(), Value::Int(1)), ("b".into(), Value::Int(2))],
    ])
    .unwrap();

    let hits = col
        .find(Filter::And(vec![
            Filter::Eq("a".into(), Value::Int(1)),
            Filter::Eq("b".into(), Value::Int(999)),
        ]))
        .unwrap();
    assert!(hits.is_empty());
}

/// Non-equality conjuncts still work alongside indexed ones — they are applied
/// as post-filters rather than being intersected.
#[test]
fn index_and_still_applies_non_indexed_conjuncts() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.create_index("a").unwrap();
    col.create_index("b").unwrap();
    col.insert_many(vec![
        vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(1)),
            ("c".into(), s("keep")),
        ],
        vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(1)),
            ("c".into(), s("drop")),
        ],
    ])
    .unwrap();

    let hits = col
        .find(Filter::And(vec![
            Filter::Eq("a".into(), Value::Int(1)),
            Filter::Eq("b".into(), Value::Int(1)),
            Filter::Eq("c".into(), s("keep")),
        ]))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].get("c"), Some(&s("keep")));
}

// ---------------------------------------------------------------------------
// find_one — early exit must not change which document comes back
// ---------------------------------------------------------------------------

/// `find_one` stops at the first match now; it must still return the same
/// document the full `find` puts first.
#[test]
fn find_one_matches_head_of_find() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.insert_many(
        (0..50)
            .map(|i| vec![("n".into(), Value::Int(i)), ("tag".into(), s("x"))])
            .collect(),
    )
    .unwrap();

    let head = col.find(Filter::Eq("tag".into(), s("x"))).unwrap();
    let one = col.find_one(Filter::Eq("tag".into(), s("x"))).unwrap();
    assert_eq!(one.map(|d| d.id), head.first().map(|d| d.id));
}

#[test]
fn find_one_indexed_matches_head_of_find() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.create_index("tag").unwrap();
    col.insert_many(
        (0..50)
            .map(|i| vec![("n".into(), Value::Int(i)), ("tag".into(), s("x"))])
            .collect(),
    )
    .unwrap();

    let head = col.find(Filter::Eq("tag".into(), s("x"))).unwrap();
    let one = col.find_one(Filter::Eq("tag".into(), s("x"))).unwrap();
    assert_eq!(one.map(|d| d.id), head.first().map(|d| d.id));
}

#[test]
fn find_one_no_match_is_none() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.insert(vec![("n".into(), Value::Int(1))]).unwrap();
    assert!(
        col.find_one(Filter::Eq("n".into(), Value::Int(99)))
            .unwrap()
            .is_none()
    );
}

/// Field encryption is applied on the way out of `find_one` exactly as it is
/// for `find` — the rewritten path must not have skipped the decrypt pass.
#[cfg(feature = "encryption")]
#[test]
fn find_one_decrypts_like_find() {
    use taladb_core::derive_key;
    let db = db();
    let key = derive_key("pw", &[7u8; 16], taladb_core::MIN_PBKDF2_ITERATIONS).unwrap();
    let col = db
        .collection("t")
        .unwrap()
        .with_field_encryption(vec!["secret".into()], key);
    col.insert(vec![
        ("tag".into(), s("x")),
        ("secret".into(), s("classified")),
    ])
    .unwrap();

    let one = col
        .find_one(Filter::Eq("tag".into(), s("x")))
        .unwrap()
        .unwrap();
    assert_eq!(one.get("secret"), Some(&s("classified")));
}

// ---------------------------------------------------------------------------
// count — the index-only path must agree with the counting path
// ---------------------------------------------------------------------------

#[test]
fn count_indexed_eq_agrees_with_find() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.create_index("k").unwrap();
    col.insert_many(
        (0..120)
            .map(|i| vec![("k".into(), Value::Int(i % 4))])
            .collect(),
    )
    .unwrap();

    for k in 0..5 {
        let filter = Filter::Eq("k".into(), Value::Int(k));
        assert_eq!(
            col.count(filter.clone()).unwrap(),
            col.find(filter).unwrap().len() as u64,
            "count disagreed with find for k={k}"
        );
    }
}

/// A document missing the indexed field has no index entry, so an index-only
/// count must not claim it — and an `Eq` against a value nothing holds must be
/// zero, not the table length.
#[test]
fn count_indexed_eq_ignores_documents_without_the_field() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.create_index("k").unwrap();
    col.insert_many(vec![
        vec![("k".into(), s("a"))],
        vec![("other".into(), s("a"))],
        vec![("k".into(), s("b"))],
    ])
    .unwrap();

    assert_eq!(col.count(Filter::Eq("k".into(), s("a"))).unwrap(), 1);
    assert_eq!(col.count(Filter::Eq("k".into(), s("zzz"))).unwrap(), 0);
    assert_eq!(col.count(Filter::All).unwrap(), 3);
}

/// `Eq` compares with `==`, and NaN equals nothing — so the filter matches zero
/// documents even though NaN encodes to a real index key whose range *would*
/// find the stored row. The index-only count path must decline this case rather
/// than count the range.
#[test]
fn count_nan_equality_agrees_with_find() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.create_index("f").unwrap();
    col.insert_many(vec![
        vec![("f".into(), Value::Float(f64::NAN))],
        vec![("f".into(), Value::Float(1.0))],
    ])
    .unwrap();

    let filter = Filter::Eq("f".into(), Value::Float(f64::NAN));
    assert_eq!(col.find(filter.clone()).unwrap().len(), 0);
    assert_eq!(
        col.count(filter).unwrap(),
        0,
        "count counted a NaN Eq that find rejects"
    );
}

/// `0.0 == -0.0` in IEEE 754, so an `Eq` on either must find both — the planner
/// covers this by emitting both encodings, and count must agree.
#[test]
fn count_signed_zero_equality_agrees_with_find() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.create_index("f").unwrap();
    col.insert_many(vec![
        vec![("f".into(), Value::Float(0.0))],
        vec![("f".into(), Value::Float(-0.0))],
        vec![("f".into(), Value::Float(1.0))],
    ])
    .unwrap();

    for probe in [0.0f64, -0.0f64] {
        let filter = Filter::Eq("f".into(), Value::Float(probe));
        let found = col.find(filter.clone()).unwrap().len() as u64;
        assert_eq!(
            col.count(filter).unwrap(),
            found,
            "disagreement for {probe}"
        );
        assert_eq!(found, 2, "both signed zeros should match {probe}");
    }
}

#[test]
fn count_unindexed_and_compound_filters_agree_with_find() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.create_index("a").unwrap();
    col.insert_many(
        (0..80)
            .map(|i| {
                vec![
                    ("a".into(), Value::Int(i % 3)),
                    ("b".into(), Value::Int(i % 7)),
                ]
            })
            .collect(),
    )
    .unwrap();

    for filter in [
        Filter::Gt("a".into(), Value::Int(0)),
        Filter::And(vec![
            Filter::Eq("a".into(), Value::Int(1)),
            Filter::Eq("b".into(), Value::Int(1)),
        ]),
        Filter::Or(vec![
            Filter::Eq("a".into(), Value::Int(0)),
            Filter::Eq("a".into(), Value::Int(2)),
        ]),
        Filter::Ne("b".into(), Value::Int(0)),
    ] {
        assert_eq!(
            col.count(filter.clone()).unwrap(),
            col.find(filter.clone()).unwrap().len() as u64,
            "count disagreed with find for {filter:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Limit pushdown in find_with_options
// ---------------------------------------------------------------------------

/// Without a sort, `skip + limit` is pushed into the storage walk. The page must
/// be identical to slicing the full result.
#[test]
fn limit_pushdown_matches_unbounded_slice() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.insert_many(
        (0..100)
            .map(|i| vec![("n".into(), Value::Int(i)), ("tag".into(), s("x"))])
            .collect(),
    )
    .unwrap();

    let all = col.find(Filter::Eq("tag".into(), s("x"))).unwrap();
    for (skip, limit) in [(0u64, 5u64), (10, 5), (95, 20), (0, 100), (100, 5)] {
        let page = col
            .find_with_options(
                Filter::Eq("tag".into(), s("x")),
                FindOptions {
                    skip,
                    limit: Some(limit),
                    ..Default::default()
                },
            )
            .unwrap();
        let want: Vec<_> = all
            .iter()
            .skip(skip as usize)
            .take(limit as usize)
            .map(|d| d.id)
            .collect();
        assert_eq!(
            page.iter().map(|d| d.id).collect::<Vec<_>>(),
            want,
            "page mismatch for skip={skip} limit={limit}"
        );
    }
}

/// With a sort, no limit may be pushed down — every candidate can still reach
/// the page, so truncating the walk would drop the actual top-N.
#[test]
fn sorted_limit_still_sees_every_candidate() {
    let db = db();
    let col = db.collection("t").unwrap();
    // Insert ascending, so the highest `n` are the LAST documents written and a
    // truncated walk would miss them entirely.
    col.insert_many(
        (0..200)
            .map(|i| vec![("n".into(), Value::Int(i)), ("tag".into(), s("x"))])
            .collect(),
    )
    .unwrap();

    let top = col
        .find_with_options(
            Filter::Eq("tag".into(), s("x")),
            FindOptions {
                sort: vec![SortSpec::desc("n")],
                limit: Some(3),
                ..Default::default()
            },
        )
        .unwrap();
    let got: Vec<i64> = top
        .iter()
        .filter_map(|d| d.get("n").and_then(taladb_core::Value::as_int))
        .collect();
    assert_eq!(got, vec![199, 198, 197]);
}

#[test]
fn limit_zero_returns_nothing() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.insert_many((0..10).map(|i| vec![("n".into(), Value::Int(i))]).collect())
        .unwrap();
    let page = col
        .find_with_options(
            Filter::All,
            FindOptions {
                limit: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(page.is_empty());
}

// ---------------------------------------------------------------------------
// Batched index maintenance must leave indexes exactly as per-document did
// ---------------------------------------------------------------------------

/// After a bulk insert, every index-backed query must return exactly what the
/// same query returns with no indexes at all. A batching bug shows up as the
/// indexed collection missing rows the control still finds.
#[test]
fn batched_insert_keeps_indexes_consistent() {
    let rows: Vec<Vec<(String, Value)>> = (0..150)
        .map(|i| {
            vec![
                ("a".into(), Value::Int(i % 5)),
                ("b".into(), s(if i % 2 == 0 { "even" } else { "odd" })),
                ("body".into(), s("alpha beta gamma")),
            ]
        })
        .collect();

    let indexed_db = db();
    let col = indexed_db.collection("t").unwrap();
    col.create_index("a").unwrap();
    col.create_index("b").unwrap();
    col.create_compound_index(&["a", "b"]).unwrap();
    col.create_fts_index("body").unwrap();
    col.insert_many(rows.clone()).unwrap();

    // Control: identical data, no indexes, so every query is a scan+filter.
    let control_db = db();
    let control = control_db.collection("t").unwrap();
    control.insert_many(rows).unwrap();

    for filter in [
        Filter::Eq("a".into(), Value::Int(2)),
        Filter::And(vec![
            Filter::Eq("a".into(), Value::Int(2)),
            Filter::Eq("b".into(), s("even")),
        ]),
        Filter::And(vec![
            Filter::Eq("a".into(), Value::Int(0)),
            Filter::Eq("b".into(), s("odd")),
        ]),
    ] {
        let got = col.find(filter.clone()).unwrap().len();
        let want = control.find(filter.clone()).unwrap().len();
        assert_eq!(
            got, want,
            "indexed result disagreed with scan for {filter:?}"
        );
        assert!(got > 0, "test would be vacuous for {filter:?}");
    }

    assert_eq!(
        col.find(Filter::Contains("body".into(), "beta".into()))
            .unwrap()
            .len(),
        150
    );
    // Corpus stats must reflect the whole batch, not just the last document.
    assert_eq!(col.search_text("body", "gamma", 200).unwrap().len(), 150);
}

/// A bulk delete must remove every index entry it created — a stale entry
/// surfaces as a query returning a document that no longer exists.
#[test]
fn batched_delete_clears_every_index() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.create_index("a").unwrap();
    col.create_compound_index(&["a", "b"]).unwrap();
    col.create_fts_index("body").unwrap();

    col.insert_many(
        (0..60)
            .map(|i| {
                vec![
                    ("a".into(), Value::Int(i % 3)),
                    ("b".into(), s("x")),
                    ("body".into(), s("delete me now")),
                ]
            })
            .collect(),
    )
    .unwrap();

    let removed = col
        .delete_many(Filter::Eq("a".into(), Value::Int(1)))
        .unwrap();
    assert_eq!(removed, 20);

    assert!(
        col.find(Filter::Eq("a".into(), Value::Int(1)))
            .unwrap()
            .is_empty()
    );
    assert!(
        col.find(Filter::And(vec![
            Filter::Eq("a".into(), Value::Int(1)),
            Filter::Eq("b".into(), s("x")),
        ]))
        .unwrap()
        .is_empty()
    );
    assert_eq!(col.count(Filter::All).unwrap(), 40);
    assert_eq!(col.search_text("body", "delete", 100).unwrap().len(), 40);
}

/// An update whose indexed value is unchanged emits the same index key as both
/// a delete and a put in one batch. The put must win — otherwise the document
/// silently drops out of its own index.
#[test]
fn update_with_unchanged_index_key_stays_indexed() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.create_index("k").unwrap();
    col.insert_many(vec![
        vec![("k".into(), s("stable")), ("v".into(), Value::Int(1))],
        vec![("k".into(), s("stable")), ("v".into(), Value::Int(2))],
    ])
    .unwrap();

    let n = col
        .update_many(
            Filter::Eq("k".into(), s("stable")),
            Update::Set(vec![("v".into(), Value::Int(9))]),
        )
        .unwrap();
    assert_eq!(n, 2);

    let hits = col.find(Filter::Eq("k".into(), s("stable"))).unwrap();
    assert_eq!(hits.len(), 2, "documents fell out of their own index");
    for d in &hits {
        assert_eq!(d.get("v"), Some(&Value::Int(9)));
    }
}

/// Moving an indexed value must clear the old index entry, not just add a new
/// one — the old key and the new key are both in one batch.
#[test]
fn update_moving_index_key_clears_the_old_entry() {
    let db = db();
    let col = db.collection("t").unwrap();
    col.create_index("k").unwrap();
    col.insert_many(vec![
        vec![("k".into(), s("before")), ("n".into(), Value::Int(1))],
        vec![("k".into(), s("before")), ("n".into(), Value::Int(2))],
    ])
    .unwrap();

    col.update_many(
        Filter::Eq("k".into(), s("before")),
        Update::Set(vec![("k".into(), s("after"))]),
    )
    .unwrap();

    assert!(
        col.find(Filter::Eq("k".into(), s("before")))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        col.find(Filter::Eq("k".into(), s("after"))).unwrap().len(),
        2
    );
}

// ---------------------------------------------------------------------------
// Vector scoring — the hoisted-norm form must score identically
// ---------------------------------------------------------------------------

/// The flat scan now computes the query norm once and uses a specialised
/// scorer. It must rank identically to the general `compute_similarity`.
#[test]
fn hoisted_query_norm_scores_match_compute_similarity() {
    use taladb_core::VectorMetric;
    use taladb_core::vector::{compute_similarity, l2_norm, score_with_query_norm};

    let query = vec![0.3f32, -1.5, 2.0, 0.0];
    let norm = l2_norm(&query);
    let candidates = [
        vec![1.0f32, 0.0, 0.0, 0.0],
        vec![0.3, -1.5, 2.0, 0.0],
        vec![-0.3, 1.5, -2.0, 0.0],
        vec![0.0, 0.0, 0.0, 0.0],
    ];
    for metric in [
        VectorMetric::Cosine,
        VectorMetric::Dot,
        VectorMetric::Euclidean,
    ] {
        for c in &candidates {
            let a = compute_similarity(&metric, &query, c);
            let b = score_with_query_norm(&metric, &query, norm, c);
            assert!(
                (a - b).abs() < 1e-6,
                "{metric:?}: {a} != {b} for candidate {c:?}"
            );
        }
    }
}

/// End to end: nearest-neighbour order is unchanged by the scoring rewrite.
#[test]
fn find_nearest_ranks_by_similarity() {
    let db = db();
    let col = db.collection("v").unwrap();
    col.create_vector_index("emb", 3, None, None).unwrap();
    col.insert_many(vec![
        vec![
            ("name".into(), s("exact")),
            (
                "emb".into(),
                Value::Array(vec![
                    Value::Float(1.0),
                    Value::Float(0.0),
                    Value::Float(0.0),
                ]),
            ),
        ],
        vec![
            ("name".into(), s("orthogonal")),
            (
                "emb".into(),
                Value::Array(vec![
                    Value::Float(0.0),
                    Value::Float(1.0),
                    Value::Float(0.0),
                ]),
            ),
        ],
        vec![
            ("name".into(), s("opposite")),
            (
                "emb".into(),
                Value::Array(vec![
                    Value::Float(-1.0),
                    Value::Float(0.0),
                    Value::Float(0.0),
                ]),
            ),
        ],
    ])
    .unwrap();

    let hits = col.find_nearest("emb", &[1.0, 0.0, 0.0], 3, None).unwrap();
    let order: Vec<_> = hits
        .iter()
        .filter_map(|h| h.document.get("name").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(order, vec!["exact", "orthogonal", "opposite"]);
    assert!((hits[0].score - 1.0).abs() < 1e-6);
}

// ---------------------------------------------------------------------------
// Write generation — the counter subscribe() relies on
// ---------------------------------------------------------------------------

#[test]
fn write_generation_advances_only_on_commit() {
    let db = db();
    let col = db.collection("t").unwrap();
    let start = col.write_generation();

    // A read must not move it.
    col.find(Filter::All).unwrap();
    assert_eq!(col.write_generation(), start);

    col.insert(vec![("n".into(), Value::Int(1))]).unwrap();
    let after_insert = col.write_generation();
    assert!(after_insert > start, "insert did not bump the generation");

    // A batch fires one notification, not one per document.
    col.insert_many((0..5).map(|i| vec![("n".into(), Value::Int(i))]).collect())
        .unwrap();
    assert_eq!(col.write_generation(), after_insert + 1);
}

/// Handles from the same `Database` share a registry, so a write through one is
/// visible to a watcher created through another.
#[test]
fn write_generation_is_shared_across_handles() {
    let db = db();
    let a = db.collection("t").unwrap();
    let b = db.collection("t").unwrap();
    let before = b.write_generation();
    a.insert(vec![("n".into(), Value::Int(1))]).unwrap();
    assert!(b.write_generation() > before);
}
