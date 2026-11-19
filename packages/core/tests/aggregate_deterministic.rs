//! v0.13.2 M3 / M2-F2: `aggregate` must give the same answer every time.
//!
//! When `$match` plans to an id-set plan — `IndexOr` (an `$or`/`$in` over an
//! index, or a range with its Int/Float twin), `IndexAnd` (several indexed
//! equality conjuncts) or a full-text `Contains` — the executor gathered ids in
//! a `HashSet` and fetched them in its iteration order, which is seeded per
//! call. Order-dependent accumulators (`$push`, `$addToSet`, `$first`,
//! `$last`), tie-breaking `$min`/`$max` over mixed types, and the bits of a
//! floating-point `$sum` then changed from one identical call to the next.
//!
//! The oracle is an unindexed twin: a full scan visits documents in id order,
//! and the fixed id-set plans fetch in the same order, so the two must agree
//! exactly — not just call-to-call.
use taladb::aggregate::{Accumulator, GroupKey, Stage};
use taladb::{Collection, Database, Document, Filter, Value};

const DOCS: i64 = 400;

fn doc(i: i64) -> Vec<(String, Value)> {
    // Int/Float twins of equal magnitude make $min/$max ties type-dependent;
    // 1e16-scale terms make float $sum order-dependent.
    let n = if i % 2 == 0 {
        Value::Int(i % 50)
    } else {
        Value::Float((i % 50) as f64)
    };
    let x = match i % 4 {
        0 => 0.1,
        1 => 1e16,
        2 => -1e16,
        _ => 0.3,
    };
    vec![
        ("i".into(), Value::Int(i)),
        ("n".into(), n),
        ("g".into(), Value::Int(i % 3)),
        ("a".into(), Value::Int(i % 5)),
        ("b".into(), Value::Int(i % 7)),
        ("x".into(), Value::Float(x)),
        ("tag".into(), Value::Str(format!("t{}", i % 11))),
        (
            "body".into(),
            Value::Str(if i % 6 == 0 {
                "red green blue".into()
            } else {
                "red only".into()
            }),
        ),
    ]
}

struct Twins {
    _db: Database,
    plain: Collection,
    indexed: Collection,
}

fn twins() -> Twins {
    let db = Database::open_in_memory().unwrap();
    let plain = db.collection("plain").unwrap();
    let indexed = db.collection("indexed").unwrap();
    for f in ["n", "a", "b"] {
        indexed.create_index(f).unwrap();
    }
    indexed.create_fts_index("body").unwrap();
    // Same ids on both sides, so "id order" means the same thing.
    for i in 0..DOCS {
        let mut d = doc(i);
        let id = taladb::Ulid::from(i as u128 + 1).to_string();
        d.push(("_id".into(), Value::Str(id)));
        plain.insert(d.clone()).unwrap();
        indexed.insert(d).unwrap();
    }
    Twins {
        _db: db,
        plain,
        indexed,
    }
}

fn group(by: GroupKey) -> Stage {
    Stage::Group {
        key: by,
        accumulators: vec![
            ("push".into(), Accumulator::Push("i".into())),
            ("set".into(), Accumulator::AddToSet("tag".into())),
            ("first".into(), Accumulator::First("i".into())),
            ("last".into(), Accumulator::Last("i".into())),
            ("min".into(), Accumulator::Min("n".into())),
            ("max".into(), Accumulator::Max("n".into())),
            ("sum".into(), Accumulator::Sum("x".into())),
            ("count".into(), Accumulator::Count),
        ],
    }
}

/// `Debug` of every result, with floats as raw bits so a 1-ulp `$sum`
/// difference or a Float-vs-Int tie cannot hide behind `==`.
fn render(docs: &[Document]) -> String {
    fn v(out: &mut String, value: &Value) {
        match value {
            Value::Float(f) => out.push_str(&format!("F{:016x}", f.to_bits())),
            Value::Array(items) => {
                out.push('[');
                for it in items {
                    v(out, it);
                    out.push(',');
                }
                out.push(']');
            }
            other => out.push_str(&format!("{other:?}")),
        }
    }
    let mut out = String::new();
    for d in docs {
        for (k, val) in &d.fields {
            out.push_str(k);
            out.push('=');
            v(&mut out, val);
            out.push(';');
        }
        out.push('\n');
    }
    out
}

/// Filters that plan to an id-set: IndexOr, IndexAnd, FtsSearch.
fn id_set_filters() -> Vec<(&'static str, Filter)> {
    vec![
        (
            "IndexOr ($or of equalities)",
            Filter::Or(vec![
                Filter::Eq("a".into(), Value::Int(1)),
                Filter::Eq("b".into(), Value::Int(2)),
                Filter::Eq("a".into(), Value::Int(3)),
            ]),
        ),
        (
            "IndexOr (range with its Float twin)",
            Filter::Lt("n".into(), Value::Int(30)),
        ),
        (
            "IndexAnd (two indexed equalities)",
            Filter::And(vec![
                Filter::Eq("a".into(), Value::Int(1)),
                Filter::Eq("b".into(), Value::Int(3)),
            ]),
        ),
        (
            "FtsSearch (one token)",
            Filter::Contains("body".into(), "red".into()),
        ),
        (
            "FtsSearch (token intersection)",
            Filter::Contains("body".into(), "green blue".into()),
        ),
    ]
}

#[test]
fn id_set_aggregates_are_repeatable_and_match_a_full_scan() {
    let t = twins();
    for (what, f) in id_set_filters() {
        for by in [GroupKey::Null, GroupKey::Field("g".into())] {
            let pipeline = vec![Stage::Match(f.clone()), group(by.clone())];
            let oracle = render(&t.plain.aggregate(pipeline.clone()).unwrap());
            assert!(!oracle.is_empty(), "{what}: fixture must match something");
            for call in 0..8 {
                let got = render(&t.indexed.aggregate(pipeline.clone()).unwrap());
                assert_eq!(got, oracle, "{what}, group {by:?}, call {call}");
            }
        }
    }
}

/// `$in` plans to `IndexIn`, which walks its ranges in key order: not id
/// order, but already the same on every call. It is deliberately left as it
/// was, so it is held to repeatability rather than to the full-scan order.
#[test]
fn index_in_aggregates_are_repeatable() {
    let t = twins();
    let f = Filter::In(
        "a".into(),
        vec![Value::Int(0), Value::Int(2), Value::Int(4)],
    );
    let pipeline = vec![Stage::Match(f), group(GroupKey::Field("g".into()))];
    let first = render(&t.indexed.aggregate(pipeline.clone()).unwrap());
    for call in 0..8 {
        assert_eq!(
            render(&t.indexed.aggregate(pipeline.clone()).unwrap()),
            first,
            "call {call}"
        );
    }
}

/// High-cardinality grouping (one group per document) over an id-set plan:
/// every group is emitted, exactly once, identically on every call.
#[test]
fn high_cardinality_grouping_is_repeatable() {
    let t = twins();
    let pipeline = vec![
        Stage::Match(Filter::Lt("n".into(), Value::Int(45))),
        group(GroupKey::Field("i".into())),
    ];
    let oracle = t.plain.aggregate(pipeline.clone()).unwrap();
    assert!(oracle.len() > 300);
    let first = render(&t.indexed.aggregate(pipeline.clone()).unwrap());
    assert_eq!(first, render(&oracle));
    for _ in 0..4 {
        assert_eq!(
            render(&t.indexed.aggregate(pipeline.clone()).unwrap()),
            first
        );
    }
}

/// `find` stays documented as unordered; this only pins that the id-set plans
/// still return the same *set* as a full scan (the fix must not lose or
/// duplicate anything while ordering the fetch).
#[test]
fn id_set_finds_return_the_full_scan_set() {
    let t = twins();
    let mut filters = id_set_filters();
    filters.push((
        "IndexIn ($in)",
        Filter::In("a".into(), vec![Value::Int(0), Value::Int(2)]),
    ));
    for (what, f) in filters {
        let mut want: Vec<_> = t
            .plain
            .find(f.clone())
            .unwrap()
            .iter()
            .map(|d| d.id)
            .collect();
        let mut got: Vec<_> = t
            .indexed
            .find(f.clone())
            .unwrap()
            .iter()
            .map(|d| d.id)
            .collect();
        want.sort();
        got.sort();
        assert_eq!(got, want, "{what}");
    }
}
