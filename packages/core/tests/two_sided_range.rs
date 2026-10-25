//! Two-sided ranges on an indexed field (`n >= x AND n < y`).
//!
//! On a field that has never held an array, the executor walks only the key
//! ranges both bounds share instead of intersecting the two halves' ID sets.
//! Every test here compares an indexed collection with an unindexed twin — a
//! full scan of the same documents — so the oracle is the filter itself, not
//! another index path.

use std::collections::BTreeSet;
use std::sync::Arc;
use taladb::query::options::{FindOptions, SortSpec};
use taladb::{Collection, Database, Document, Filter, Ulid, Update, Value};

fn id(i: usize) -> Value {
    Value::Str(Ulid::from(i as u128 + 1).to_string())
}

/// `n` values, one document each (`None` = field missing).
fn scalars() -> Vec<Option<Value>> {
    let mut values: Vec<Option<Value>> = (-5..=25).map(|i| Some(Value::Int(i))).collect();
    values.extend(
        [
            Value::Int(i64::MIN),
            Value::Int(i64::MAX),
            Value::Float(-3.5),
            Value::Float(-0.0),
            Value::Float(0.0),
            Value::Float(0.5),
            Value::Float(2.5),
            Value::Float(9.999),
            Value::Float(10.0),
            Value::Float(10.000_000_1),
            Value::Float(19.5),
            Value::Float(1e300),
            Value::Float(f64::INFINITY),
            Value::Float(f64::NEG_INFINITY),
            Value::Float(f64::NAN),
            Value::Str("a".into()),
            Value::Str("10".into()),
            Value::Str(String::new()),
            Value::Str("a\0b".into()),
            Value::Bool(true),
            Value::Null,
            Value::Bytes(vec![0, 10]),
            Value::Object(vec![("n".into(), Value::Int(5))]),
        ]
        .into_iter()
        .map(Some),
    );
    values.push(None);
    values.push(None);
    values
}

/// Arrays whose elements satisfy the two bounds separately, plus the scalars.
fn with_arrays() -> Vec<Option<Value>> {
    let mut values = scalars();
    values.extend(
        [
            vec![Value::Int(1), Value::Int(20)],
            vec![Value::Int(-4), Value::Int(30)],
            vec![Value::Float(10.5), Value::Int(3)],
            vec![Value::Int(5)],
            vec![],
            vec![Value::Str("x".into()), Value::Int(7)],
            vec![Value::Float(-0.0), Value::Float(f64::NAN)],
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
                ("k".to_string(), Value::Int((i % 7) as i64)),
                ("counter".to_string(), Value::Int(0)),
            ];
            if let Some(v) = v {
                doc.push(("n".to_string(), v.clone()));
            }
            doc
        })
        .collect()
}

/// An indexed collection and its unindexed twin with the same documents.
fn pair(values: &[Option<Value>]) -> (Database, Collection, Collection) {
    let db = Database::open_in_memory().unwrap();
    let indexed = db.collection("indexed").unwrap();
    let plain = db.collection("plain").unwrap();
    indexed.insert_many(documents(values)).unwrap();
    plain.insert_many(documents(values)).unwrap();
    indexed.create_index("n").unwrap();
    (db, indexed, plain)
}

fn bound_values() -> Vec<Value> {
    vec![
        Value::Int(i64::MIN),
        Value::Int(-1),
        Value::Int(0),
        Value::Int(5),
        Value::Int(10),
        Value::Int(20),
        Value::Int(i64::MAX),
        Value::Float(-0.0),
        Value::Float(0.0),
        Value::Float(4.5),
        Value::Float(10.0),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Float(f64::NEG_INFINITY),
        Value::Str("a".into()),
    ]
}

/// Every lower × upper bound, inclusive and exclusive, including empty and
/// contradictory windows (lower above upper) and equal bounds.
fn two_sided_filters() -> Vec<Filter> {
    let mut out = Vec::new();
    for lo in bound_values() {
        for hi in bound_values() {
            for (lower, upper) in [
                (
                    Filter::Gte("n".into(), lo.clone()),
                    Filter::Lt("n".into(), hi.clone()),
                ),
                (
                    Filter::Gte("n".into(), lo.clone()),
                    Filter::Lte("n".into(), hi.clone()),
                ),
                (
                    Filter::Gt("n".into(), lo.clone()),
                    Filter::Lt("n".into(), hi.clone()),
                ),
                (
                    Filter::Gt("n".into(), lo.clone()),
                    Filter::Lte("n".into(), hi.clone()),
                ),
            ] {
                out.push(Filter::And(vec![lower, upper]));
            }
        }
    }
    out
}

fn ids(docs: &[Document]) -> BTreeSet<Ulid> {
    docs.iter().map(|d| d.id).collect()
}

fn ordered(docs: &[Document]) -> Vec<Ulid> {
    docs.iter().map(|d| d.id).collect()
}

fn assert_same_answers(indexed: &Collection, plain: &Collection, filter: &Filter) {
    let got = indexed.find(filter.clone()).unwrap();
    let want = plain.find(filter.clone()).unwrap();
    assert_eq!(
        got.len(),
        ids(&got).len(),
        "duplicate documents for {filter:?}"
    );
    assert_eq!(ids(&got), ids(&want), "ID set differs for {filter:?}");
    assert_eq!(
        indexed.count(filter.clone()).unwrap(),
        want.len() as u64,
        "count differs for {filter:?}"
    );
}

#[test]
fn every_bound_combination_matches_a_full_scan_on_scalar_fields() {
    let (_db, indexed, plain) = pair(&scalars());
    for filter in two_sided_filters() {
        assert_same_answers(&indexed, &plain, &filter);
    }
}

#[test]
fn arrays_whose_elements_satisfy_separate_bounds_are_still_found() {
    let (_db, indexed, plain) = pair(&with_arrays());
    for filter in two_sided_filters() {
        assert_same_answers(&indexed, &plain, &filter);
    }
    // The case the ID intersection exists for: [1, 20] has no element in
    // [5, 10), but 20 >= 5 and 1 < 10.
    let filter = Filter::And(vec![
        Filter::Gte("n".into(), Value::Int(5)),
        Filter::Lt("n".into(), Value::Int(10)),
    ]);
    let found = ids(&indexed.find(filter).unwrap());
    assert!(found.contains(&Ulid::from(scalars().len() as u128 + 1)));
}

#[test]
fn bound_order_extra_predicates_and_repeated_bounds_match_a_full_scan() {
    for values in [scalars(), with_arrays()] {
        let (_db, indexed, plain) = pair(&values);
        let lo = || Filter::Gte("n".into(), Value::Int(2));
        let hi = || Filter::Lt("n".into(), Value::Float(19.5));
        let filters = [
            Filter::And(vec![hi(), lo()]),
            Filter::And(vec![lo(), Filter::Eq("k".into(), Value::Int(3)), hi()]),
            Filter::And(vec![
                Filter::Gte("n".into(), Value::Int(-100)),
                lo(),
                hi(),
                Filter::Lte("n".into(), Value::Int(15)),
            ]),
            Filter::And(vec![lo(), hi(), Filter::Ne("n".into(), Value::Int(7))]),
            Filter::And(vec![lo(), hi(), Filter::Exists("n".into(), true)]),
            Filter::Or(vec![
                Filter::And(vec![lo(), hi()]),
                Filter::Eq("n".into(), Value::Int(-5)),
            ]),
        ];
        for filter in filters {
            assert_same_answers(&indexed, &plain, &filter);
        }
    }
}

#[test]
fn sorted_pages_are_identical_and_unsorted_limits_are_subsets() {
    for values in [scalars(), with_arrays()] {
        let (_db, indexed, plain) = pair(&values);
        for filter in two_sided_filters().into_iter().step_by(7) {
            let all = ids(&plain.find(filter.clone()).unwrap());
            for sort in [
                vec![SortSpec::asc("n")],
                vec![SortSpec::desc("n")],
                vec![SortSpec::asc("k"), SortSpec::desc("n")],
            ] {
                for (skip, limit) in [
                    (0, None),
                    (0, Some(3)),
                    (2, Some(4)),
                    (5, Some(1)),
                    (100, Some(5)),
                ] {
                    let options = || FindOptions {
                        sort: sort.clone(),
                        skip,
                        limit,
                        ..Default::default()
                    };
                    let got = indexed
                        .find_with_options(filter.clone(), options())
                        .unwrap();
                    let want = plain.find_with_options(filter.clone(), options()).unwrap();
                    assert_eq!(
                        ordered(&got),
                        ordered(&want),
                        "sorted page differs for {filter:?}"
                    );
                }
            }
            // Without a sort the order is the plan's; only the set is defined.
            for (skip, limit) in [(0, Some(1)), (0, Some(3)), (1, Some(2)), (3, None)] {
                let page = indexed
                    .find_with_options(
                        filter.clone(),
                        FindOptions {
                            skip,
                            limit,
                            ..Default::default()
                        },
                    )
                    .unwrap();
                let expected = all
                    .len()
                    .saturating_sub(skip as usize)
                    .min(limit.map_or(usize::MAX, |l| l as usize));
                assert_eq!(page.len(), expected, "page size for {filter:?}");
                assert_eq!(ids(&page).len(), page.len(), "duplicates for {filter:?}");
                assert!(
                    ids(&page).is_subset(&all),
                    "page outside the answer for {filter:?}"
                );
            }
            let one = indexed.find_one(filter.clone()).unwrap();
            assert_eq!(one.is_some(), !all.is_empty());
            assert!(one.is_none_or(|d| all.contains(&d.id)));
        }
    }
}

#[test]
fn unsorted_results_come_back_in_index_key_order_on_scalar_fields() {
    // Key order: the Int block sorts below the Float block, each ascending.
    // This is the documented, now deterministic, order of an unsorted
    // two-sided range on a scalar field; it used to be per-process random.
    let (_db, indexed, _plain) = pair(&scalars());
    let filter = Filter::And(vec![
        Filter::Gte("n".into(), Value::Int(3)),
        Filter::Lt("n".into(), Value::Int(12)),
    ]);
    let values: Vec<Value> = indexed
        .find(filter)
        .unwrap()
        .iter()
        .map(|d| d.get("n").cloned().unwrap())
        .collect();
    let mut expected: Vec<Value> = (3..12).map(Value::Int).collect();
    expected.extend([9.999, 10.0, 10.000_000_1].map(Value::Float));
    assert_eq!(values, expected);
}

#[test]
fn update_and_delete_affect_the_same_documents_as_a_full_scan() {
    for values in [scalars(), with_arrays()] {
        for filter in two_sided_filters().into_iter().step_by(11) {
            let (_db, indexed, plain) = pair(&values);
            let inc = || Update::Inc(vec![("counter".into(), Value::Int(1))]);
            assert_eq!(
                indexed.update_many(filter.clone(), inc()).unwrap(),
                plain.update_many(filter.clone(), inc()).unwrap(),
                "update count for {filter:?}"
            );
            let touched = Filter::Eq("counter".into(), Value::Int(1));
            assert_eq!(
                ids(&indexed.find(touched.clone()).unwrap()),
                ids(&plain.find(touched).unwrap())
            );
            assert_eq!(
                indexed.delete_many(filter.clone()).unwrap(),
                plain.delete_many(filter.clone()).unwrap(),
                "delete count for {filter:?}"
            );
            assert_eq!(
                ids(&indexed.find(Filter::All).unwrap()),
                ids(&plain.find(Filter::All).unwrap())
            );
        }
    }
}

#[test]
fn a_field_that_gains_an_array_falls_back_and_stays_correct() {
    let (_db, indexed, plain) = pair(&scalars());
    let doc = vec![
        ("_id".to_string(), id(10_000)),
        (
            "n".to_string(),
            Value::Array(vec![Value::Int(-50), Value::Int(50)]),
        ),
    ];
    indexed.insert(doc.clone()).unwrap();
    plain.insert(doc).unwrap();
    for filter in two_sided_filters().into_iter().step_by(3) {
        assert_same_answers(&indexed, &plain, &filter);
    }
}

#[cfg(feature = "encryption")]
#[test]
fn encrypted_fields_behave_as_without_the_index() {
    let key = taladb::derive_key("pw", &[7u8; 16], taladb::MIN_PBKDF2_ITERATIONS).unwrap();
    let db = Database::open_in_memory().unwrap();
    let indexed = db
        .collection("indexed")
        .unwrap()
        .with_field_encryption(vec!["n".into()], key.clone());
    let plain = db
        .collection("plain")
        .unwrap()
        .with_field_encryption(vec!["n".into()], key);
    let values: Vec<Option<Value>> = (-5..=25).map(|i| Some(Value::Int(i))).collect();
    indexed.insert_many(documents(&values)).unwrap();
    plain.insert_many(documents(&values)).unwrap();
    indexed.create_index("n").unwrap();
    for filter in two_sided_filters().into_iter().step_by(5) {
        assert_same_answers(&indexed, &plain, &filter);
    }
}

/// Readers run two-sided queries while a writer inserts scalars and, halfway,
/// an array. Every read must be internally consistent (no duplicates, every
/// document matches), and the final answers must match a full scan.
#[test]
fn concurrent_reads_during_writes_stay_consistent() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let indexed = db.collection("indexed").unwrap();
    indexed.insert_many(documents(&scalars())).unwrap();
    indexed.create_index("n").unwrap();
    let filter = Filter::And(vec![
        Filter::Gte("n".into(), Value::Int(100)),
        Filter::Lt("n".into(), Value::Int(400)),
    ]);
    let writer = {
        let db = db.clone();
        std::thread::spawn(move || {
            let col = db.collection("indexed").unwrap();
            for i in 0..600usize {
                let value = if i == 300 {
                    Value::Array(vec![Value::Int(50), Value::Int(450)])
                } else {
                    Value::Int(i as i64)
                };
                col.insert(vec![("_id".into(), id(1_000 + i)), ("n".into(), value)])
                    .unwrap();
            }
        })
    };
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let db = db.clone();
            let filter = filter.clone();
            std::thread::spawn(move || {
                let col = db.collection("indexed").unwrap();
                for _ in 0..200 {
                    let docs = col.find(filter.clone()).unwrap();
                    assert_eq!(docs.len(), ids(&docs).len(), "duplicate documents");
                    for doc in &docs {
                        assert!(
                            filter.matches(doc).unwrap(),
                            "non-matching document {doc:?}"
                        );
                    }
                }
            })
        })
        .collect();
    writer.join().unwrap();
    for reader in readers {
        reader.join().unwrap();
    }
    let plain = db.collection("plain").unwrap();
    plain
        .insert_many(
            indexed
                .find(Filter::All)
                .unwrap()
                .into_iter()
                .map(|d| {
                    let mut fields = d.fields;
                    fields.retain(|(k, _)| k != "_id" && k != "_changed_at");
                    fields.push(("_id".into(), Value::Str(d.id.to_string())));
                    fields
                })
                .collect(),
        )
        .unwrap();
    assert_same_answers(&indexed, &plain, &filter);
}
