//! v0.13.2 M3 / M2-F1: indexed equality must return each matching document
//! exactly once.
//!
//! Index keys are `value-prefix ++ id`, and a string or bytes value containing
//! an escaped NUL *extends* a shorter value's prefix: `"a\0b"`'s keys sort
//! inside the key range of `"a"`. An array document holding both `"a"` and
//! `"a\0b"` therefore has two keys in `"a"`'s equality range, and the
//! `IndexEq` / `CompoundIndexEq` fetch paths — which had no dedup set — emitted
//! it twice. That leaked into `find`, `aggregate`, and the candidate lists of
//! `update_many` / `delete_many`, whose returned counts double-counted it.
//!
//! Every check compares an indexed collection with an unindexed twin holding
//! the same documents, so the oracle is document semantics, not another index
//! path.
use std::collections::BTreeMap;

use taladb::{Collection, Database, Filter, Update, Value};

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

/// Values on both sides of every escaped-NUL boundary, as scalars and inside
/// arrays that mix a value with its own prefix-extensions.
fn fixtures() -> Vec<Value> {
    let scalars = vec![
        s("a"),
        s("a\0"),
        s("a\0\0"),
        s("a\0b"),
        s("a\u{1}"),
        s("ab"),
        s("\0"),
        s(""),
        Value::Bytes(vec![b'a']),
        Value::Bytes(vec![b'a', 0]),
        Value::Bytes(vec![b'a', 0, b'b']),
        Value::Bytes(vec![b'a', 0, 0]),
        Value::Bytes(vec![]),
        Value::Bytes(vec![0]),
    ];
    let mut out = scalars;
    out.extend([
        Value::Array(vec![s("a"), s("a\0b")]),
        Value::Array(vec![s("a\0b"), s("a")]),
        Value::Array(vec![s("a"), s("a\0"), s("a\0\0"), s("a\0b")]),
        Value::Array(vec![s("a\0"), s("a\0b")]),
        Value::Array(vec![s("a"), s("a")]),
        Value::Array(vec![
            Value::Bytes(vec![b'a']),
            Value::Bytes(vec![b'a', 0, b'b']),
        ]),
        Value::Array(vec![Value::Bytes(vec![0]), Value::Bytes(vec![])]),
        Value::Array(vec![s(""), s("\0")]),
    ]);
    out
}

struct Twins {
    _db: Database,
    plain: Collection,
    indexed: Collection,
    compound: Collection,
}

fn twins() -> Twins {
    let db = Database::open_in_memory().unwrap();
    let plain = db.collection("plain").unwrap();
    let indexed = db.collection("indexed").unwrap();
    let compound = db.collection("compound").unwrap();
    indexed.create_index("n").unwrap();
    compound.create_compound_index(&["k", "n"]).unwrap();
    for (i, v) in fixtures().into_iter().enumerate() {
        let doc = vec![
            ("i".into(), Value::Int(i as i64)),
            ("k".into(), Value::Int(0)),
            ("n".into(), v),
        ];
        plain.insert(doc.clone()).unwrap();
        indexed.insert(doc.clone()).unwrap();
        compound.insert(doc).unwrap();
    }
    Twins {
        _db: db,
        plain,
        indexed,
        compound,
    }
}

/// Multiset of `i` values a filter returns: duplicates show up as counts > 1.
fn hits(col: &Collection, f: &Filter) -> BTreeMap<i64, usize> {
    let mut out = BTreeMap::new();
    for doc in col.find(f.clone()).unwrap() {
        let Some(Value::Int(i)) = doc.get("i") else {
            panic!("fixture doc without i");
        };
        *out.entry(*i).or_default() += 1;
    }
    out
}

fn queries() -> Vec<Value> {
    let mut q: Vec<Value> = fixtures()
        .into_iter()
        .filter(|v| !matches!(v, Value::Array(_)))
        .collect();
    q.push(s("zz-absent"));
    q
}

#[test]
fn indexed_equality_find_returns_each_document_once() {
    let t = twins();
    for v in queries() {
        let f = Filter::Eq("n".into(), v.clone());
        let want = hits(&t.plain, &f);
        assert!(
            want.values().all(|&c| c == 1),
            "oracle itself unique: {v:?}"
        );
        assert_eq!(hits(&t.indexed, &f), want, "simple index, n == {v:?}");
        assert_eq!(
            t.indexed.count(f.clone()).unwrap(),
            want.len() as u64,
            "{v:?}"
        );
    }
}

#[test]
fn compound_equality_find_returns_each_document_once() {
    let t = twins();
    for v in queries() {
        let f = Filter::And(vec![
            Filter::Eq("k".into(), Value::Int(0)),
            Filter::Eq("n".into(), v.clone()),
        ]);
        let want = hits(&t.plain, &f);
        assert_eq!(hits(&t.compound, &f), want, "compound index, n == {v:?}");
        assert_eq!(
            t.compound.count(f.clone()).unwrap(),
            want.len() as u64,
            "{v:?}"
        );
    }
}

/// Limits and `find_one` walk the same fetch path with a cap; a duplicate
/// used to consume a slot of the page.
#[test]
fn limited_equality_pages_hold_distinct_documents() {
    let t = twins();
    let f = Filter::Eq("n".into(), s("a"));
    let all = hits(&t.plain, &f).len();
    assert!(all >= 4, "fixture: several documents contain \"a\"");
    for col in [&t.indexed, &t.compound] {
        let f = if std::ptr::eq(col, &t.compound) {
            Filter::And(vec![Filter::Eq("k".into(), Value::Int(0)), f.clone()])
        } else {
            f.clone()
        };
        for limit in 1..=all as u64 {
            let page = col
                .find_with_options(
                    f.clone(),
                    taladb::FindOptions {
                        limit: Some(limit),
                        ..Default::default()
                    },
                )
                .unwrap();
            let mut ids: Vec<_> = page.iter().map(|d| d.id).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), page.len(), "limit {limit}: duplicate in page");
            assert_eq!(page.len() as u64, limit, "limit {limit}: page short");
        }
    }
}

/// The same duplicate inflated the counts `update_many` / `delete_many`
/// return (the candidate list held the document twice).
#[test]
fn bulk_mutation_counts_each_document_once() {
    let t = twins();
    let f = Filter::Eq("n".into(), s("a"));
    let want = hits(&t.plain, &f).len() as u64;
    let upd = || Update::Set(vec![("touched".into(), Value::Bool(true))]);
    assert_eq!(t.plain.update_many(f.clone(), upd()).unwrap(), want);
    assert_eq!(t.indexed.update_many(f.clone(), upd()).unwrap(), want);
    let cf = Filter::And(vec![Filter::Eq("k".into(), Value::Int(0)), f.clone()]);
    assert_eq!(t.compound.update_many(cf.clone(), upd()).unwrap(), want);
    assert_eq!(t.indexed.delete_many(f.clone()).unwrap(), want);
    assert_eq!(t.compound.delete_many(cf).unwrap(), want);
    assert_eq!(t.indexed.count(f).unwrap(), 0);
}

/// `$match` -> `$group` over an indexed equality: `$sum: 1` must count each
/// document once.
#[test]
fn aggregate_over_indexed_equality_counts_each_document_once() {
    use taladb::aggregate::{Accumulator, GroupKey, Stage};
    let t = twins();
    let f = Filter::Eq("n".into(), s("a"));
    let want = hits(&t.plain, &f).len() as i64;
    let pipeline = vec![
        Stage::Match(f),
        Stage::Group {
            key: GroupKey::Field("k".into()),
            accumulators: vec![("n".into(), Accumulator::Count)],
        },
    ];
    for col in [&t.plain, &t.indexed] {
        let out = col.aggregate(pipeline.clone()).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].get("n"), Some(&Value::Int(want)));
    }
}
