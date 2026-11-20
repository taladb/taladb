//! `count` on filters an index covers must agree with `find` and with a full
//! scan of the same documents.
//!
//! Every check compares an indexed collection with an unindexed twin holding
//! the same documents, so the oracle is the filter's document semantics, not
//! another index path. The fixtures cover the cases where index-key matching
//! and document matching could drift apart: arrays (one key per element),
//! missing fields, NaN, signed zero, Int/Float twins, NUL-escaped strings and
//! bytes, a field that once held an array, legacy index metadata (no array
//! counter), compound indexes and field-encrypted values.

use std::collections::BTreeSet;
use taladb::index::{docs_table_name, meta_key};
use taladb::{Collection, Database, Filter, Ulid, Update, Value};

fn id(i: usize) -> Value {
    Value::Str(Ulid::from(i as u128 + 1).to_string())
}

fn scalars() -> Vec<Option<Value>> {
    let mut values: Vec<Option<Value>> = (-5..=25).map(|i| Some(Value::Int(i))).collect();
    values.extend(
        [
            Value::Int(i64::MIN),
            Value::Int(i64::MAX),
            Value::Int(0),
            Value::Int(5),
            Value::Float(-3.5),
            Value::Float(-0.0),
            Value::Float(0.0),
            Value::Float(0.5),
            Value::Float(5.0),
            Value::Float(9.999),
            Value::Float(10.0),
            Value::Float(1e300),
            Value::Float(f64::INFINITY),
            Value::Float(f64::NEG_INFINITY),
            Value::Float(f64::NAN),
            Value::Float(-f64::NAN),
            Value::Str("a".into()),
            Value::Str("a".into()),
            Value::Str("10".into()),
            Value::Str(String::new()),
            Value::Str("\0".into()),
            Value::Str("a\0".into()),
            Value::Str("a\0b".into()),
            Value::Str("a\0\0".into()),
            Value::Str("ab".into()),
            Value::Bool(true),
            Value::Bool(false),
            Value::Null,
            Value::Bytes(vec![]),
            Value::Bytes(vec![0]),
            Value::Bytes(vec![0, 10]),
            Value::Bytes(vec![10]),
            Value::Object(vec![("n".into(), Value::Int(5))]),
        ]
        .into_iter()
        .map(Some),
    );
    values.push(None);
    values.push(None);
    values
}

fn with_arrays() -> Vec<Option<Value>> {
    let mut values = scalars();
    values.extend(
        [
            vec![Value::Int(1), Value::Int(20)],
            vec![Value::Int(5), Value::Int(5)],
            vec![Value::Float(10.5), Value::Int(3)],
            vec![Value::Int(5)],
            vec![],
            // Elements sharing an escaped-NUL prefix inside ONE document are
            // the M2-F1 trigger, covered by its own tests below.
            vec![Value::Str("a\0b".into()), Value::Str("c".into())],
            vec![Value::Str("a".into()), Value::Bytes(vec![0, 10])],
            vec![
                Value::Float(-0.0),
                Value::Float(0.0),
                Value::Float(f64::NAN),
            ],
            vec![Value::Array(vec![Value::Int(5)])],
        ]
        .into_iter()
        .map(|v| Some(Value::Array(v))),
    );
    values
}

fn documents(values: &[Option<Value>]) -> Vec<Vec<(String, Value)>> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let mut doc = vec![
                ("_id".to_string(), id(i)),
                ("k".to_string(), Value::Int((i % 3) as i64)),
            ];
            if let Some(v) = v {
                doc.push(("n".to_string(), v.clone()));
            }
            doc
        })
        .collect()
}

struct Pair {
    _db: Database,
    indexed: Collection,
    plain: Collection,
}

fn pair(values: &[Option<Value>]) -> Pair {
    let db = Database::open_in_memory().unwrap();
    let indexed = db.collection("indexed").unwrap();
    let plain = db.collection("plain").unwrap();
    indexed.insert_many(documents(values)).unwrap();
    plain.insert_many(documents(values)).unwrap();
    indexed.create_index("n").unwrap();
    indexed.create_index("k").unwrap();
    indexed.create_compound_index(&["k", "n"]).unwrap();
    Pair {
        _db: db,
        indexed,
        plain,
    }
}

fn bound_values() -> Vec<Value> {
    vec![
        Value::Int(i64::MIN),
        Value::Int(-1),
        Value::Int(0),
        Value::Int(5),
        Value::Int(10),
        Value::Int(i64::MAX),
        Value::Float(-0.0),
        Value::Float(0.0),
        Value::Float(4.5),
        Value::Float(5.0),
        Value::Float(10.0),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
        Value::Str(String::new()),
        Value::Str("a".into()),
        Value::Str("a\0".into()),
        Value::Str("a\0b".into()),
        Value::Str("b".into()),
        Value::Bytes(vec![0]),
        Value::Bytes(vec![0, 10]),
        Value::Bool(false),
        Value::Bool(true),
        Value::Null,
    ]
}

/// Where per-field array counters live (crate-private in the library).
const META_INDEX_ARRAYS: &str = "meta::index_arrays";

fn n(f: fn(String, Value) -> Filter, v: &Value) -> Filter {
    f("n".into(), v.clone())
}

fn single_filters() -> Vec<Filter> {
    let mut out = Vec::new();
    for v in bound_values() {
        out.push(n(Filter::Eq, &v));
        out.push(n(Filter::Gt, &v));
        out.push(n(Filter::Gte, &v));
        out.push(n(Filter::Lt, &v));
        out.push(n(Filter::Lte, &v));
    }
    out.push(Filter::In("n".into(), vec![]));
    out.push(Filter::In(
        "n".into(),
        vec![Value::Int(5), Value::Int(5), Value::Float(5.0)],
    ));
    out.push(Filter::In(
        "n".into(),
        vec![Value::Float(0.0), Value::Float(f64::NAN)],
    ));
    out.push(Filter::In(
        "n".into(),
        vec![
            Value::Str("a".into()),
            Value::Str("a\0b".into()),
            Value::Bytes(vec![0]),
            Value::Null,
        ],
    ));
    out
}

fn compound_filters() -> Vec<Filter> {
    let mut out = Vec::new();
    let all = bound_values();
    for (i, lo) in all.iter().enumerate() {
        // Two- and three-sided windows, with redundant and contradictory bounds.
        let hi = &all[(i * 7 + 3) % all.len()];
        out.push(Filter::And(vec![n(Filter::Gte, lo), n(Filter::Lt, hi)]));
        out.push(Filter::And(vec![n(Filter::Gt, lo), n(Filter::Lte, hi)]));
        out.push(Filter::And(vec![
            n(Filter::Gt, lo),
            n(Filter::Lte, hi),
            n(Filter::Lt, &Value::Int(20)),
        ]));
        out.push(Filter::And(vec![
            Filter::All,
            n(Filter::Gte, lo),
            Filter::All,
        ]));
        // Mixed fields and shapes the key-only path must decline.
        out.push(Filter::And(vec![
            n(Filter::Gte, lo),
            Filter::Eq("k".into(), Value::Int(1)),
        ]));
        out.push(Filter::And(vec![n(Filter::Eq, lo), n(Filter::Gte, hi)]));
        out.push(Filter::Or(vec![n(Filter::Lt, lo), n(Filter::Gt, hi)]));
        out.push(Filter::Not(Box::new(n(Filter::Gte, lo))));
        // Compound equality on the (k, n) index.
        for k in 0..3 {
            out.push(Filter::And(vec![
                Filter::Eq("k".into(), Value::Int(k)),
                n(Filter::Eq, lo),
            ]));
        }
    }
    out
}

/// Deterministic pseudo-random filters over the bound values (xorshift).
fn random_filters(seed: u64, count: usize) -> Vec<Filter> {
    let mut state = seed;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let values = bound_values();
    let leaf = |r: u64, v: &Value| -> Filter {
        match r % 5 {
            0 => n(Filter::Eq, v),
            1 => n(Filter::Gt, v),
            2 => n(Filter::Gte, v),
            3 => n(Filter::Lt, v),
            _ => n(Filter::Lte, v),
        }
    };
    (0..count)
        .map(|_| {
            let parts = 1 + (next() % 3) as usize;
            let mut children: Vec<Filter> = (0..parts)
                .map(|_| {
                    let v = &values[(next() % values.len() as u64) as usize];
                    leaf(next(), v)
                })
                .collect();
            if children.len() == 1 {
                children.pop().unwrap()
            } else {
                Filter::And(children)
            }
        })
        .collect()
}

fn distinct(docs: Vec<taladb::Document>) -> u64 {
    docs.iter().map(|d| d.id).collect::<BTreeSet<_>>().len() as u64
}

fn check(pair: &Pair, filter: &Filter, label: &str) {
    let want = pair.plain.find(filter.clone()).unwrap().len() as u64;
    // Distinct IDs: v0.13.1's indexed equality `find` can return an array
    // document twice when two of its elements share an escaped-NUL prefix
    // (`["a", "a\0b"]` for `n == "a"`). That is a `find` defect tracked
    // separately (M2-F1); this file is about `count`.
    let found = distinct(pair.indexed.find(filter.clone()).unwrap());
    let counted = pair.indexed.count(filter.clone()).unwrap();
    assert_eq!(found, want, "{label}: indexed find differs for {filter:?}");
    assert_eq!(
        counted, want,
        "{label}: indexed count differs for {filter:?}"
    );
    assert_eq!(
        pair.plain.count(filter.clone()).unwrap(),
        want,
        "{label}: unindexed count differs for {filter:?}"
    );
}

fn check_all(pair: &Pair, label: &str) {
    for f in single_filters()
        .iter()
        .chain(&compound_filters())
        .chain(&random_filters(0x9E37_79B9_7F4A_7C15, 400))
    {
        check(pair, f, label);
    }
}

#[test]
fn covered_counts_match_a_full_scan_on_scalar_fields() {
    check_all(&pair(&scalars()), "scalars");
}

#[test]
fn covered_counts_match_a_full_scan_when_the_field_holds_arrays() {
    check_all(&pair(&with_arrays()), "arrays");
}

/// A field that held an array and no longer does is scalar again, and must
/// still count right; one that still holds an array keeps array semantics.
#[test]
fn covered_counts_follow_array_history() {
    let pair = pair(&scalars());
    let target = Filter::Eq("_id".into(), id(3));
    let array = Update::Set(vec![(
        "n".into(),
        Value::Array(vec![Value::Int(1), Value::Int(20)]),
    )]);
    for c in [&pair.indexed, &pair.plain] {
        c.update_one(target.clone(), array.clone()).unwrap();
    }
    check_all(&pair, "array added");
    let scalar = Update::Set(vec![("n".into(), Value::Int(7))]);
    for c in [&pair.indexed, &pair.plain] {
        c.update_one(target.clone(), scalar.clone()).unwrap();
    }
    check_all(&pair, "array removed");
    for c in [&pair.indexed, &pair.plain] {
        c.delete_many(n(Filter::Gte, &Value::Int(10))).unwrap();
    }
    check_all(&pair, "after delete_many");
}

/// An index whose array counter is missing (written before counters existed,
/// or with malformed metadata) is treated as possibly holding arrays.
#[test]
fn covered_counts_match_with_legacy_index_metadata() {
    for values in [scalars(), with_arrays()] {
        let pair = pair(&values);
        let backend = pair._db.backend();
        let mut w = backend.begin_write().unwrap();
        for field in ["n", "k"] {
            w.delete(META_INDEX_ARRAYS, meta_key("indexed", field).as_bytes())
                .unwrap();
        }
        w.commit().unwrap();
        check_all(&pair, "legacy metadata");

        let mut w = backend.begin_write().unwrap();
        w.put(
            META_INDEX_ARRAYS,
            meta_key("indexed", "n").as_bytes(),
            b"bad",
        )
        .unwrap();
        w.commit().unwrap();
        check_all(&pair, "malformed metadata");
    }
}

/// Counts of the same filters on a larger collection, where the index walk
/// and the document fetch both run in many chunks.
#[test]
fn covered_counts_match_on_a_larger_collection() {
    let mut values = Vec::new();
    for i in 0..3_000i64 {
        values.push(Some(match i % 6 {
            0 => Value::Float(i as f64 / 4.0),
            1 => Value::Str(format!("s{:04}", i % 997)),
            5 if i % 30 == 5 => continue,
            _ => Value::Int(i % 1_013),
        }));
    }
    check_all(&pair(&values), "3k");
}

#[cfg(feature = "encryption")]
#[test]
fn covered_counts_on_field_encrypted_values_are_unchanged() {
    use zeroize::Zeroizing;
    let db = Database::open_in_memory().unwrap();
    let open = |name: &str| {
        db.collection(name)
            .unwrap()
            .with_field_encryption(vec!["n".into()], Zeroizing::new([7u8; 32]))
    };
    let (indexed, twin) = (open("enc"), open("enc_twin"));
    indexed.insert_many(documents(&scalars())).unwrap();
    twin.insert_many(documents(&scalars())).unwrap();
    indexed.create_index("n").unwrap();
    // Ciphertext is stored and indexed. Whatever the document path decides
    // about plaintext predicates on it, the index path must decide the same.
    for f in single_filters().iter().chain(&compound_filters()) {
        assert_eq!(
            indexed.count(f.clone()).unwrap(),
            twin.count(f.clone()).unwrap(),
            "encrypted count differs for {f:?}"
        );
    }
}

// --- Damaged storage --------------------------------------------------------
//
// A covered count answers from index keys alone, as the equality count already
// did in v0.13.1: it does not read document bodies. These tests pin what that
// means when storage is damaged.

fn damaged() -> (Database, Collection, Ulid) {
    let db = Database::open_in_memory().unwrap();
    let c = db.collection("c").unwrap();
    c.insert_many(documents(&scalars())).unwrap();
    c.create_index("n").unwrap();
    let victim = Ulid::from(10u128); // n = 4
    (db, c, victim)
}

fn overwrite_body(db: &Database, id: Ulid, bytes: &[u8]) {
    let mut w = db.backend().begin_write().unwrap();
    w.put(&docs_table_name("c"), &id.to_bytes(), bytes).unwrap();
    w.commit().unwrap();
}

#[test]
fn reading_a_damaged_body_is_still_an_error() {
    let (db, c, victim) = damaged();
    overwrite_body(&db, victim, &[0xFF, 0xFF, 0xFF]);
    let range = n(Filter::Gte, &Value::Int(0));
    assert!(
        c.find(range.clone()).is_err(),
        "find must report the damage"
    );
    // Anything that has to decode bodies still reports it.
    assert!(
        c.count(Filter::And(vec![range, Filter::Exists("k".into(), true)]))
            .is_err()
    );
}

#[test]
fn equality_count_does_not_read_bodies() {
    // Unchanged from v0.13.1: the key-only equality count.
    let (db, c, victim) = damaged();
    overwrite_body(&db, victim, &[0xFF, 0xFF, 0xFF]);
    assert_eq!(c.count(n(Filter::Eq, &Value::Int(4))).unwrap(), 1);
}

/// M2-F1 (fixed in v0.13.2 M3): the indexed equality `find` used to emit an
/// array document once per element whose key falls in the value's range.
#[test]
fn indexed_equality_find_returns_each_document_once() {
    let pair = pair(&[Some(Value::Array(vec![
        Value::Str("a".into()),
        Value::Str("a\0b".into()),
    ]))]);
    let f = n(Filter::Eq, &Value::Str("a".into()));
    assert_eq!(pair.plain.find(f.clone()).unwrap().len(), 1);
    assert_eq!(pair.indexed.count(f.clone()).unwrap(), 1);
    assert_eq!(pair.indexed.find(f).unwrap().len(), 1);
}

fn m2_f1_pair() -> Pair {
    pair(&[
        Some(Value::Array(vec![
            Value::Str("a".into()),
            Value::Str("a\0b".into()),
        ])),
        Some(Value::Str("a".into())),
        Some(Value::Str("a\0b".into())),
    ])
}

/// M2-F1, compound shape. v0.13.1 counts the array document twice (3); the
/// key-only count decodes each compound key and counts it once.
#[test]
fn compound_equality_count_counts_each_document_once() {
    let pair = m2_f1_pair();
    // k = i % 3: documents 0, 1, 2 have k = 0, 1, 2.
    for k in 0..3 {
        let f = Filter::And(vec![
            Filter::Eq("k".into(), Value::Int(k)),
            n(Filter::Eq, &Value::Str("a".into())),
        ]);
        let want = pair.plain.find(f.clone()).unwrap().len() as u64;
        assert_eq!(pair.indexed.count(f.clone()).unwrap(), want, "{f:?}");
    }
}

fn put_index_key(db: &Database, key: &[u8]) {
    let mut w = db.backend().begin_write().unwrap();
    w.put("idx::c::n", key, &[]).unwrap();
    w.commit().unwrap();
}

/// A covered range count answers from index keys alone, like the equality
/// count: a damaged body under a matching key is counted, not reported.
/// (v0.13.1 decoded every body on this path and returned the decode error.)
#[test]
fn covered_range_count_does_not_read_bodies() {
    let (db, c, victim) = damaged();
    let before = c.count(n(Filter::Gte, &Value::Int(0))).unwrap();
    overwrite_body(&db, victim, &[0xFF, 0xFF, 0xFF]);
    assert_eq!(c.count(n(Filter::Gte, &Value::Int(0))).unwrap(), before);
}

/// An index key whose value prefix does not decode is reported as damage.
/// (v0.13.1 fetched the body by the key's trailing id instead.)
#[test]
fn covered_range_count_reports_a_malformed_index_key() {
    let (db, c, _) = damaged();
    let mut key = vec![0x20, 0x80, 0, 0];
    key.extend_from_slice(&Ulid::from(10u128).to_bytes());
    put_index_key(&db, &key);
    assert!(c.count(n(Filter::Gte, &Value::Int(0))).is_err());
}

/// An index key without a document (only possible through damage: bodies and
/// keys are written in one transaction) is counted, as the equality count
/// already did. (v0.13.1 skipped it on the range path.)
#[test]
fn covered_range_count_counts_an_orphan_key() {
    let (db, c, _) = damaged();
    let before = c.count(n(Filter::Gte, &Value::Int(0))).unwrap();
    let mut key = vec![0x20];
    key.extend_from_slice(&(4u64 ^ (1 << 63)).to_be_bytes());
    key.extend_from_slice(&Ulid::from(999_999u128).to_bytes());
    put_index_key(&db, &key);
    assert_eq!(c.count(n(Filter::Eq, &Value::Int(4))).unwrap(), 2);
    assert_eq!(c.count(n(Filter::Gte, &Value::Int(0))).unwrap(), before + 1);
}
