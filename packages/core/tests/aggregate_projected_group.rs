//! `$match` → `$group` pipelines must produce exactly what the full-decoding
//! pipeline produces.
//!
//! The oracle runs in the same binary: `[$match, $skip: 0, $group, …]` is not
//! the `$match` → `$group` shape, so it takes the full-decoding path, over the
//! same plan and therefore the same document order. `$skip: 0` is the
//! identity. Results are compared through their `Debug` rendering, which tells
//! `-0.0` from `0.0` and keeps NaN comparable, so accumulators whose answer
//! depends on order (`$first`, `$last`, `$push`, float `$sum`) are covered.
//!
//! M2-F2 (pre-existing in v0.13.1): plans that union or intersect ID sets
//! (`IndexOr`, `IndexAnd`, e.g. `n < 150` with its Float twin range) hand
//! documents over in hash-set order, which changes from call to call. For those
//! filters even two runs of the *same* pipeline disagree on `$push` order and
//! on `$first`/`$last`, so they are compared in a canonical, order-free form;
//! filters whose plans have a fixed order are compared exactly, and the test
//! first checks that the oracle itself is repeatable for them.

use taladb::aggregate::{Accumulator, GroupKey, Stage};
use taladb::index::docs_table_name;
use taladb::{Collection, Database, Filter, Ulid, Value};

fn id(i: usize) -> Value {
    Value::Str(Ulid::from(i as u128 + 1).to_string())
}

/// A value of every kind, chosen by `i`.
fn assorted(i: usize) -> Option<Value> {
    Some(match i % 17 {
        0 => return None,
        1 => Value::Null,
        2 => Value::Int(i as i64 % 5),
        3 => Value::Int(-(i as i64)),
        4 => Value::Float(i as f64 / 3.0),
        5 => Value::Float(-0.0),
        6 => Value::Float(0.0),
        7 => Value::Float(f64::NAN),
        8 => Value::Str(format!("g{}", i % 4)),
        9 => Value::Str("a\0b".into()),
        10 => Value::Bool(i.is_multiple_of(2)),
        11 => Value::Bytes(vec![0, (i % 3) as u8]),
        12 => Value::Array(vec![Value::Int(1), Value::Str("x".into())]),
        13 => Value::Object(vec![("g".into(), Value::Int(i as i64 % 3))]),
        14 => Value::Int(i64::MAX),
        15 => Value::Float(f64::INFINITY),
        _ => Value::Int(7),
    })
}

fn document(i: usize) -> Vec<(String, Value)> {
    let mut doc = vec![
        ("n".to_string(), Value::Int(i as i64)),
        ("bucket".to_string(), Value::Int((i % 6) as i64)),
        ("payload".to_string(), Value::Str("x".repeat(64 + i % 7))),
        (
            "meta".to_string(),
            Value::Object(vec![
                ("g".into(), Value::Str(format!("m{}", i % 3))),
                ("v".into(), assorted(i + 3).unwrap_or(Value::Null)),
                (
                    "deep".into(),
                    Value::Object(vec![("w".into(), Value::Int((i % 4) as i64))]),
                ),
            ]),
        ),
        (
            "vec".to_string(),
            Value::Array((0..4).map(|j| Value::Float((i + j) as f64)).collect()),
        ),
    ];
    // Every third document carries a stored `_id` field, as `insert_with_id`
    // and the bindings write it.
    if i.is_multiple_of(3) {
        doc.push(("_id".to_string(), id(i)));
    }
    if let Some(v) = assorted(i) {
        doc.push(("x".to_string(), v));
    }
    if let Some(v) = assorted(i * 7 + 1) {
        doc.push(("y".to_string(), v));
    }
    doc
}

fn seeded(db: &Database, name: &str, count: usize, indexed: bool) -> Collection {
    let c = db.collection(name).unwrap();
    c.insert_many((0..count).map(document).collect()).unwrap();
    if indexed {
        c.create_index("n").unwrap();
        c.create_index("bucket").unwrap();
        c.create_index("x").unwrap();
        c.create_compound_index(&["bucket", "n"]).unwrap();
    }
    c
}

fn group_keys() -> Vec<GroupKey> {
    [
        "bucket",
        "x",
        "y",
        "missing",
        "_id",
        "meta",
        "meta.g",
        "meta.v",
        "meta.deep.w",
        "meta.missing",
        "vec",
        "payload",
    ]
    .into_iter()
    .map(|f| GroupKey::Field(f.into()))
    .chain([GroupKey::Null])
    .collect()
}

fn accumulator_sets() -> Vec<Vec<(String, Accumulator)>> {
    let fields = [
        "n",
        "x",
        "y",
        "meta.v",
        "meta.deep.w",
        "missing",
        "_id",
        "vec",
    ];
    let mut sets: Vec<Vec<(String, Accumulator)>> =
        vec![vec![], vec![("c".into(), Accumulator::Count)]];
    for f in fields {
        sets.push(vec![
            ("sum".into(), Accumulator::Sum(f.into())),
            ("avg".into(), Accumulator::Avg(f.into())),
            ("min".into(), Accumulator::Min(f.into())),
            ("max".into(), Accumulator::Max(f.into())),
            ("count".into(), Accumulator::Count),
            ("push".into(), Accumulator::Push(f.into())),
            ("set".into(), Accumulator::AddToSet(f.into())),
            ("first".into(), Accumulator::First(f.into())),
            ("last".into(), Accumulator::Last(f.into())),
        ]);
    }
    // Several fields in one stage, and an output name that shadows an input.
    sets.push(vec![
        ("n".into(), Accumulator::Sum("x".into())),
        ("y".into(), Accumulator::Last("meta.g".into())),
        ("z".into(), Accumulator::First("payload".into())),
    ]);
    sets
}

/// Filters, each with whether its plan hands documents over in a fixed order.
fn filters() -> Vec<(Filter, bool)> {
    let gte = |f: &str, v: Value| Filter::Gte(f.into(), v);
    vec![
        (Filter::All, true),
        // `Lt(Int)` adds a Float twin range: an `IndexOr`, hash-set order.
        (Filter::Lt("n".into(), Value::Int(150)), false),
        (gte("n", Value::Int(150)), true),
        (Filter::Eq("bucket".into(), Value::Int(2)), true),
        (
            Filter::And(vec![
                Filter::Eq("bucket".into(), Value::Int(3)),
                gte("n", Value::Int(40)),
            ]),
            true,
        ),
        (
            Filter::And(vec![
                gte("n", Value::Int(40)),
                Filter::Lt("n".into(), Value::Int(200)),
            ]),
            false,
        ),
        (
            Filter::In(
                "x".into(),
                vec![Value::Null, Value::Float(0.0), Value::Int(7)],
            ),
            false,
        ),
        (
            Filter::Or(vec![
                Filter::Eq("bucket".into(), Value::Int(1)),
                Filter::Exists("y".into(), false),
            ]),
            true,
        ),
        (
            Filter::Or(vec![
                Filter::Eq("bucket".into(), Value::Int(1)),
                Filter::Eq("bucket".into(), Value::Int(4)),
            ]),
            false,
        ),
        (
            Filter::Not(Box::new(Filter::Eq(
                "meta.g".into(),
                Value::Str("m1".into()),
            ))),
            true,
        ),
        (gte("meta.deep.w", Value::Int(2)), true),
        (Filter::Eq("_id".into(), id(9)), true),
        (Filter::Regex("payload".into(), "^x{70}".into()), true),
        (Filter::Eq("n".into(), Value::Int(-1)), true),
    ]
}

fn render(docs: &[taladb::Document]) -> Vec<String> {
    docs.iter()
        .map(|d| format!("{:?} {:?}", d.id, d.fields))
        .collect()
}

fn value_key(v: &Value) -> String {
    match v {
        // Summation order moves the last bits of a float sum.
        Value::Float(f) if f.is_finite() => format!("f{f:.9e}"),
        Value::Array(items) => {
            let mut keys: Vec<String> = items.iter().map(value_key).collect();
            keys.sort();
            format!("[{}]", keys.join(","))
        }
        other => format!("{other:?}"),
    }
}

/// Order-free form of group rows: the group key, counts and `$push` as a
/// sorted multiset — every input value of every grouped document, which is
/// what a projected decode could get wrong. `$min`/`$max` over mixed types,
/// `$addToSet` (`-0.0` vs `0.0`), `$first`/`$last` and float sums all depend on
/// arrival order, so they are compared exactly on ordered filters only.
fn canonical(docs: &[taladb::Document]) -> Vec<String> {
    let mut rows: Vec<String> = docs
        .iter()
        .map(|d| {
            d.fields
                .iter()
                .filter(|(k, _)| matches!(k.as_str(), "_id" | "count" | "c" | "push"))
                .map(|(k, v)| format!("{k}={}", value_key(v)))
                .collect::<Vec<_>>()
                .join(";")
        })
        .collect();
    rows.sort();
    rows
}

type Outcome = Result<Vec<taladb::Document>, String>;

fn run(c: &Collection, pipeline: Vec<Stage>) -> Outcome {
    c.aggregate(pipeline).map_err(|e| format!("error: {e}"))
}

fn show(r: &Outcome, ordered: bool) -> String {
    match r {
        Ok(docs) if ordered => format!("{:?}", render(docs)),
        Ok(docs) => format!("{:?}", canonical(docs)),
        Err(e) => e.clone(),
    }
}

/// The pipeline both ways; `tail` stages run after `$group`. With `ordered`,
/// also checks that the full-decoding oracle repeats itself exactly.
fn both(
    c: &Collection,
    f: &Filter,
    ordered: bool,
    group: &Stage,
    tail: &[Stage],
) -> (String, String) {
    let mut fast = vec![Stage::Match(f.clone()), group.clone()];
    fast.extend_from_slice(tail);
    let mut full = vec![Stage::Match(f.clone()), Stage::Skip(0), group.clone()];
    full.extend_from_slice(tail);
    let want = show(&run(c, full.clone()), ordered);
    if ordered {
        assert_eq!(
            want,
            show(&run(c, full), true),
            "oracle not repeatable for {f:?}: mark it unordered"
        );
    }
    (show(&run(c, fast), ordered), want)
}

fn check_collection(c: &Collection, label: &str) {
    let tails: [&[Stage]; 2] = [
        &[],
        &[
            Stage::Sort(vec![taladb::SortSpec::desc("_id")]),
            Stage::Limit(3),
        ],
    ];
    for (f, ordered) in filters() {
        for key in group_keys() {
            for accumulators in accumulator_sets() {
                let group = Stage::Group {
                    key: key.clone(),
                    accumulators,
                };
                for tail in tails {
                    // A `$limit` after an order-free sort of rows whose
                    // `$first` differs cannot be canonicalised: exact only.
                    if !ordered && !tail.is_empty() {
                        continue;
                    }
                    let (fast, full) = both(c, &f, ordered, &group, tail);
                    assert_eq!(fast, full, "{label}: {f:?} / {group:?} / {tail:?}");
                }
            }
        }
    }
}

#[test]
fn projected_group_matches_full_decode_with_indexes() {
    let db = Database::open_in_memory().unwrap();
    check_collection(&seeded(&db, "c", 300, true), "indexed");
}

#[test]
fn projected_group_matches_full_decode_without_indexes() {
    let db = Database::open_in_memory().unwrap();
    check_collection(&seeded(&db, "c", 300, false), "unindexed");
}

#[test]
fn projected_group_matches_full_decode_on_an_empty_collection() {
    let db = Database::open_in_memory().unwrap();
    check_collection(&seeded(&db, "c", 0, true), "empty");
}

/// Duplicate field names in one stored document (`insert` removes them, so
/// the bodies are edited directly): `get` sees the first occurrence in both
/// decodings.
#[test]
fn projected_group_matches_full_decode_with_duplicate_fields() {
    let db = Database::open_in_memory().unwrap();
    let c = seeded(&db, "c", 40, true);
    let table = docs_table_name("c");
    let ids: Vec<Ulid> = c.find(Filter::All).unwrap().iter().map(|d| d.id).collect();
    for id in ids {
        let body = db
            .backend()
            .begin_read()
            .unwrap()
            .get(&table, &id.to_bytes())
            .unwrap()
            .unwrap();
        // Postcard: id string (length 26 + 26 bytes), field count, fields.
        assert_eq!(body[0], 26);
        let count = body[27];
        assert!(count < 0x7F, "single-byte varint");
        let mut edited = body[..27].to_vec();
        edited.push(count + 2);
        // ("bucket", Str("dup")) first, so it shadows the real `bucket`.
        edited.extend_from_slice(&[6]);
        edited.extend_from_slice(b"bucket");
        edited.extend_from_slice(&[4, 3]);
        edited.extend_from_slice(b"dup");
        edited.extend_from_slice(&body[28..]);
        // ("x", Null) last, shadowed by any real `x`.
        edited.extend_from_slice(&[1, b'x', 0]);
        let mut w = db.backend().begin_write().unwrap();
        w.put(&table, &id.to_bytes(), &edited).unwrap();
        w.commit().unwrap();
    }
    let stored = c.find(Filter::All).unwrap();
    assert_eq!(
        stored[0]
            .fields
            .iter()
            .filter(|(k, _)| k == "bucket")
            .count(),
        2,
        "the duplicates are stored"
    );
    check_collection(&c, "duplicates");
}

/// Damaged bodies: the projected decode must fail on exactly the inputs the
/// full decode fails on, with the same outcome for the whole pipeline.
#[test]
fn projected_group_reports_damage_exactly_like_full_decode() {
    let db = Database::open_in_memory().unwrap();
    let c = seeded(&db, "c", 12, false);
    // Document 6 carries `_id` = id(6), so it is stored under that id.
    let victim = Ulid::from(7u128);
    let table = docs_table_name("c");
    let original = db
        .backend()
        .begin_read()
        .unwrap()
        .get(&table, &victim.to_bytes())
        .unwrap()
        .unwrap();
    let mut variants: Vec<Vec<u8>> = Vec::new();
    for cut in 0..original.len() {
        variants.push(original[..cut].to_vec());
    }
    for at in 0..original.len() {
        for byte in [0x00, 0x07, 0x7F, 0x80, 0xC3, 0xFF] {
            let mut v = original.clone();
            v[at] = byte;
            variants.push(v);
        }
    }
    let group = Stage::Group {
        key: GroupKey::Field("bucket".into()),
        accumulators: vec![("s".into(), Accumulator::Sum("n".into()))],
    };
    for (k, bytes) in variants.iter().enumerate() {
        let mut w = db.backend().begin_write().unwrap();
        w.put(&table, &victim.to_bytes(), bytes).unwrap();
        w.commit().unwrap();
        for f in [Filter::All, Filter::Gte("n".into(), Value::Int(0))] {
            let (fast, full) = both(&c, &f, true, &group, &[]);
            assert_eq!(fast, full, "variant {k}: {bytes:?} under {f:?}");
        }
    }
}

/// Field-level encryption: `aggregate` does not decrypt (risk S-A1, tracked
/// for M3/M4). M2 must keep that behaviour exactly, ciphertext included.
#[cfg(feature = "encryption")]
#[test]
fn projected_group_keeps_field_encryption_behaviour() {
    use zeroize::Zeroizing;
    let db = Database::open_in_memory().unwrap();
    let c = db
        .collection("enc")
        .unwrap()
        .with_field_encryption(vec!["x".into(), "meta".into()], Zeroizing::new([9u8; 32]));
    c.insert_many((0..120).map(document).collect()).unwrap();
    c.create_index("n").unwrap();
    // Ciphertext differs per insert, so compare the two paths, not values.
    check_collection(&c, "field-encrypted");
}

/// M2-F2 (pre-existing, not fixed in M2): the same pipeline twice can return
/// `$push` in a different order when `$match` plans to an ID-set union.
#[test]
#[ignore = "M2-F2: aggregate order over IndexOr/IndexAnd plans is not repeatable; M3"]
fn order_dependent_accumulators_are_repeatable() {
    let db = Database::open_in_memory().unwrap();
    let c = seeded(&db, "c", 300, true);
    let pipeline = vec![
        Stage::Match(Filter::Lt("n".into(), Value::Int(150))),
        Stage::Group {
            key: GroupKey::Null,
            accumulators: vec![("p".into(), Accumulator::Push("n".into()))],
        },
    ];
    let first = render(&c.aggregate(pipeline.clone()).unwrap());
    for _ in 0..5 {
        assert_eq!(render(&c.aggregate(pipeline.clone()).unwrap()), first);
    }
}
