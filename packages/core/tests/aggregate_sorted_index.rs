//! The index-served sort fast path must be *indistinguishable* from the
//! scan-and-sort path it replaces. Every test here pins the fast path's output
//! against the same query run without a bound (which always takes the slow path).
use taladb_core::Database;
use taladb_core::aggregate::Stage;
use taladb_core::document::{Document, Value};
use taladb_core::query::Filter;
use taladb_core::query::options::{SortDirection, SortSpec};

fn spec(field: &str, dir: SortDirection) -> SortSpec {
    SortSpec {
        field: field.into(),
        direction: dir,
    }
}

fn names(docs: &[Document]) -> Vec<String> {
    docs.iter()
        .map(|d| match d.get("name") {
            Some(Value::Str(s)) => s.clone(),
            _ => "<none>".into(),
        })
        .collect()
}

/// `n` docs; `score` has heavy ties (only `distinct` values) so the tie-run
/// logic at the page boundary is exercised hard.
fn seeded(n: usize, distinct: usize, index_it: bool) -> Database {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("items").unwrap();
    for i in 0..n {
        col.insert(vec![
            ("name".into(), Value::Str(format!("item-{i:05}"))),
            ("score".into(), Value::Int((i % distinct) as i64)),
            ("tiebreak".into(), Value::Int((i % 7) as i64)),
        ])
        .unwrap();
    }
    if index_it {
        col.create_index("score").unwrap();
    }
    db
}

/// Ground truth: the same sort with no `$limit`, which never hits the fast path.
fn full_sorted(db: &Database, sort: Vec<SortSpec>) -> Vec<Document> {
    db.collection("items")
        .unwrap()
        .aggregate(vec![Stage::Sort(sort)])
        .unwrap()
}

#[test]
fn indexed_asc_page_matches_full_sort() {
    let db = seeded(1000, 20, true);
    let col = db.collection("items").unwrap();
    let sort = vec![spec("score", SortDirection::Asc)];

    let page = col
        .aggregate(vec![
            Stage::Sort(sort.clone()),
            Stage::Skip(0),
            Stage::Limit(25),
        ])
        .unwrap();

    let full = full_sorted(&db, sort);
    assert_eq!(names(&page), names(&full[0..25]));
}

#[test]
fn indexed_desc_page_matches_full_sort() {
    // Descending reverses the index walk, which flips the `_id` tiebreak order
    // within a tie-run — the fast path must re-sort the candidates to recover it.
    let db = seeded(1000, 20, true);
    let col = db.collection("items").unwrap();
    let sort = vec![spec("score", SortDirection::Desc)];

    let page = col
        .aggregate(vec![
            Stage::Sort(sort.clone()),
            Stage::Skip(0),
            Stage::Limit(25),
        ])
        .unwrap();

    let full = full_sorted(&db, sort);
    assert_eq!(names(&page), names(&full[0..25]));
}

#[test]
fn indexed_paging_walks_the_whole_collection_exactly_once() {
    let db = seeded(600, 12, true); // 50 docs per score value — fat tie-runs
    let col = db.collection("items").unwrap();
    let sort = vec![spec("score", SortDirection::Desc)];
    let full = full_sorted(&db, sort.clone());

    let mut seen: Vec<String> = Vec::new();
    for page in 0..24 {
        let out = col
            .aggregate(vec![
                Stage::Sort(sort.clone()),
                Stage::Skip(page * 25),
                Stage::Limit(25),
            ])
            .unwrap();
        seen.extend(names(&out));
    }

    assert_eq!(seen, names(&full), "paged order diverged from full sort");
    let uniq: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(
        uniq.len(),
        600,
        "a document was repeated or lost across pages"
    );
}

#[test]
fn multi_key_sort_uses_index_on_first_key_only() {
    // `score` is indexed, `tiebreak` is not. The fast path may only use `score`
    // to bound the candidates; `tiebreak` must still order within a tie-run.
    let db = seeded(800, 10, true);
    let col = db.collection("items").unwrap();
    let sort = vec![
        spec("score", SortDirection::Desc),
        spec("tiebreak", SortDirection::Asc),
    ];

    let page = col
        .aggregate(vec![
            Stage::Sort(sort.clone()),
            Stage::Skip(30),
            Stage::Limit(20),
        ])
        .unwrap();

    let full = full_sorted(&db, sort);
    assert_eq!(names(&page), names(&full[30..50]));
}

#[test]
fn falls_back_when_sort_field_is_missing_from_some_docs() {
    // Documents without the sort field have no index entry. If the fast path
    // trusted the index here it would silently drop them from the result.
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("items").unwrap();
    for i in 0..50 {
        let mut fields = vec![("name".into(), Value::Str(format!("item-{i:03}")))];
        if i % 2 == 0 {
            fields.push(("score".into(), Value::Int(i64::from(i))));
        }
        col.insert(fields).unwrap();
    }
    col.create_index("score").unwrap();

    let sort = vec![spec("score", SortDirection::Asc)];
    let page = col
        .aggregate(vec![Stage::Sort(sort.clone()), Stage::Limit(50)])
        .unwrap();

    assert_eq!(
        page.len(),
        50,
        "documents missing the sort field were dropped"
    );
    let full = full_sorted(&db, sort);
    assert_eq!(names(&page), names(&full));
}

#[test]
fn unindexed_sort_field_still_correct() {
    // No index ⇒ fast path declines ⇒ ordinary scan-and-sort. Same answer.
    let db = seeded(300, 9, false);
    let col = db.collection("items").unwrap();
    let sort = vec![spec("score", SortDirection::Desc)];
    let page = col
        .aggregate(vec![Stage::Sort(sort.clone()), Stage::Limit(15)])
        .unwrap();
    let full = full_sorted(&db, sort);
    assert_eq!(names(&page), names(&full[0..15]));
}

#[test]
fn match_before_sort_is_not_hijacked_by_the_index_path() {
    // A leading $match must still filter. The fast path only applies when the
    // sort is the *first* stage.
    let db = seeded(400, 8, true);
    let col = db.collection("items").unwrap();
    let out = col
        .aggregate(vec![
            Stage::Match(Filter::Eq("score".into(), Value::Int(3))),
            Stage::Sort(vec![spec("score", SortDirection::Desc)]),
            Stage::Limit(10),
        ])
        .unwrap();
    assert_eq!(out.len(), 10);
    for d in &out {
        assert_eq!(d.get("score"), Some(&Value::Int(3)));
    }
}

#[test]
fn limit_beyond_collection_returns_everything_in_order() {
    let db = seeded(40, 5, true);
    let col = db.collection("items").unwrap();
    let sort = vec![spec("score", SortDirection::Asc)];
    let page = col
        .aggregate(vec![Stage::Sort(sort.clone()), Stage::Limit(10_000)])
        .unwrap();
    assert_eq!(names(&page), names(&full_sorted(&db, sort)));
}

#[test]
fn projection_after_indexed_sort_still_applies() {
    let db = seeded(100, 5, true);
    let col = db.collection("items").unwrap();
    let out = col
        .aggregate(vec![
            Stage::Sort(vec![spec("score", SortDirection::Desc)]),
            Stage::Limit(5),
            Stage::Project {
                fields: vec!["score".into()],
                include: false, // exclusion
                keep_id: true,
            },
        ])
        .unwrap();
    assert_eq!(out.len(), 5);
    for d in &out {
        assert!(d.get("score").is_none(), "excluded field survived");
        assert!(d.get("name").is_some(), "unlisted field was dropped");
    }
}

// ---------------------------------------------------------------------------
// Mixed Int/Float. The index tags Int (0x20) below Float (0x30), so byte order
// puts *every* integer before *every* float — while the sort comparator ranks
// them numerically. On such a field the index is not sorted the way the query
// asks, and the fast path must decline it: it truncates to a page before the
// comparator ever runs, so a document it wrongly excludes can never be
// recovered by the final sort. These pin it against the scan-and-sort path.
// ---------------------------------------------------------------------------

/// Interleaved so that neither type is already in the right place: correct
/// ascending order is 1.5, 2, 3.5, 100 — but index order is 2, 100, 1.5, 3.5.
fn seeded_mixed_numeric(index_it: bool) -> Database {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("items").unwrap();
    for (name, score) in [
        ("item-a", Value::Int(100)),
        ("item-b", Value::Float(1.5)),
        ("item-c", Value::Int(2)),
        ("item-d", Value::Float(3.5)),
    ] {
        col.insert(vec![
            ("name".into(), Value::Str(name.into())),
            ("score".into(), score),
        ])
        .unwrap();
    }
    if index_it {
        col.create_index("score").unwrap();
    }
    db
}

#[test]
fn mixed_int_float_sort_matches_unindexed_across_pages() {
    let run = |db: &Database, pipeline: Vec<Stage>| {
        names(&db.collection("items").unwrap().aggregate(pipeline).unwrap())
    };
    for dir in [SortDirection::Asc, SortDirection::Desc] {
        for skip in [0u64, 1, 2] {
            for limit in [1u64, 2, 3, 4] {
                let pipeline = vec![
                    Stage::Sort(vec![spec("score", dir.clone())]),
                    Stage::Skip(skip),
                    Stage::Limit(limit),
                ];
                let got = run(&seeded_mixed_numeric(true), pipeline.clone());
                let want = run(&seeded_mixed_numeric(false), pipeline);
                assert_eq!(got, want, "dir={dir:?} skip={skip} limit={limit}");
            }
        }
    }
}

/// The all-Int and all-Float cases stay on the fast path — the bail-out must be
/// narrow, triggering only on a field that actually mixes the two.
#[test]
fn homogeneous_numeric_sorts_are_unaffected() {
    for float_valued in [false, true] {
        let db = Database::open_in_memory().unwrap();
        let col = db.collection("items").unwrap();
        for i in 0..20u32 {
            let score = if float_valued {
                Value::Float(f64::from(i) * 1.5)
            } else {
                Value::Int(i64::from(i))
            };
            col.insert(vec![
                ("name".into(), Value::Str(format!("item-{i:05}"))),
                ("score".into(), score),
            ])
            .unwrap();
        }
        col.create_index("score").unwrap();
        let sort = vec![spec("score", SortDirection::Asc)];
        let page = col
            .aggregate(vec![Stage::Sort(sort.clone()), Stage::Limit(5)])
            .unwrap();
        let want: Vec<String> = names(&full_sorted(&db, sort)).into_iter().take(5).collect();
        assert_eq!(names(&page), want);
    }
}
