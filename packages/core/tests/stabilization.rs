use taladb::aggregate::Stage;
use taladb::query::options::{FindOptions, SortSpec};
use taladb::{Database, Document, Filter, Ulid, Update, Value};

fn ids(docs: &[Document]) -> Vec<Ulid> {
    docs.iter().map(|d| d.id).collect()
}

#[test]
fn sorted_pages_agree_with_full_sort_for_scalar_array_missing_and_float_fields() {
    let cases = vec![
        vec![
            Some(Value::Int(3)),
            Some(Value::Int(1)),
            Some(Value::Int(1)),
            Some(Value::Int(2)),
        ],
        // The array contributes two entries; the missing field contributes none.
        // Their entry count equals the document count but is not a valid sort.
        vec![
            Some(Value::Array(vec![Value::Int(1), Value::Int(2)])),
            None,
            Some(Value::Int(3)),
        ],
        vec![
            Some(Value::Array(vec![Value::Int(1)])),
            Some(Value::Array(vec![Value::Int(2)])),
        ],
        vec![
            Some(Value::Int(2)),
            Some(Value::Float(1.5)),
            None,
            Some(Value::Null),
        ],
        vec![
            Some(Value::Float(0.0)),
            Some(Value::Float(-0.0)),
            Some(Value::Float(1.0)),
        ],
        vec![
            Some(Value::Float(1.0)),
            Some(Value::Float(1.0)),
            Some(Value::Float(2.0)),
        ],
        vec![
            Some(Value::Str("a\0z".into())),
            Some(Value::Str("a".into())),
            Some(Value::Str("b".into())),
        ],
    ];
    for values in cases {
        let db = Database::open_in_memory().unwrap();
        let col = db.collection("docs").unwrap();
        for (i, value) in values.into_iter().enumerate() {
            let mut fields = vec![("secondary".into(), Value::Int((i % 2) as i64))];
            if let Some(value) = value {
                fields.push(("key".into(), value));
            }
            col.insert_with_id(Document::with_id(Ulid::from((100 - i) as u128), fields))
                .unwrap();
        }
        col.create_index("key").unwrap();
        for primary in [SortSpec::asc("key"), SortSpec::desc("key")] {
            for sort in [
                vec![primary.clone()],
                vec![primary, SortSpec::desc("secondary")],
            ] {
                let full = col
                    .find_with_options(
                        Filter::All,
                        FindOptions {
                            sort: sort.clone(),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                for skip in 0..=full.len() {
                    for limit in 0..=full.len() {
                        let expected = &full[skip..(skip + limit).min(full.len())];
                        let page = col
                            .find_with_options(
                                Filter::All,
                                FindOptions {
                                    sort: sort.clone(),
                                    skip: skip as u64,
                                    limit: Some(limit as u64),
                                    ..Default::default()
                                },
                            )
                            .unwrap();
                        assert_eq!(ids(&page), ids(expected));
                        let page = col
                            .aggregate(vec![
                                Stage::Sort(sort.clone()),
                                Stage::Skip(skip as u64),
                                Stage::Limit(limit as u64),
                            ])
                            .unwrap();
                        assert_eq!(ids(&page), ids(expected));
                    }
                }
            }
        }
    }
}

#[test]
fn late_bulk_update_failure_rolls_back_prior_batches_and_audit_rows() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap().with_audit_log("test".into());
    col.insert_many(
        (0..600)
            .map(|i| {
                vec![
                    ("_id".into(), Value::Str(Ulid::from(i as u128).to_string())),
                    (
                        "counter".into(),
                        if i < 500 {
                            Value::Int(i)
                        } else {
                            Value::Str("not numeric".into())
                        },
                    ),
                    ("body".into(), Value::Str("rust storage".into())),
                    ("payload".into(), Value::Str("x".repeat(8192))),
                ]
            })
            .collect(),
    )
    .unwrap();
    col.create_index("counter").unwrap();
    col.create_fts_index("body").unwrap();
    let before = db.export_snapshot().unwrap();
    assert!(
        col.update_many(
            Filter::All,
            Update::Many(vec![
                Update::Set(vec![("body".into(), Value::Str("changed".into()))]),
                Update::Inc(vec![("counter".into(), Value::Int(1))])
            ])
        )
        .is_err()
    );
    assert_eq!(db.export_snapshot().unwrap(), before);
}

#[test]
fn large_index_backfills_and_bulk_mutations_preserve_all_side_tables() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.insert_many(
        (0..1025)
            .map(|i| {
                vec![
                    ("n".into(), Value::Int(i)),
                    ("bucket".into(), Value::Int(i % 5)),
                    (
                        "body".into(),
                        Value::Str(if i % 2 == 0 { "" } else { "rust storage" }.into()),
                    ),
                    ("payload".into(), Value::Str("x".repeat(8192))),
                ]
            })
            .collect(),
    )
    .unwrap();
    col.create_index("n").unwrap();
    col.create_fts_index("body").unwrap();
    col.create_compound_index(&["bucket", "n"]).unwrap();
    assert_eq!(
        col.update_many(
            Filter::All,
            Update::Set(vec![
                ("body".into(), Value::Str("new text".into())),
                ("bucket".into(), Value::Int(10))
            ])
        )
        .unwrap(),
        1025
    );
    assert!(col.search_text("body", "rust", 10).unwrap().is_empty());
    assert_eq!(col.search_text("body", "new", 2000).unwrap().len(), 1025);
    assert_eq!(
        col.count(Filter::Eq("bucket".into(), Value::Int(10)))
            .unwrap(),
        1025
    );
    assert_eq!(col.delete_many(Filter::All).unwrap(), 1025);
    assert!(col.search_text("body", "new", 10).unwrap().is_empty());
    col.drop_compound_index(&["bucket", "n"]).unwrap();
    col.drop_fts_index("body").unwrap();
    col.drop_index("n").unwrap();
    col.create_index("n").unwrap();
    col.create_fts_index("body").unwrap();
    col.create_compound_index(&["bucket", "n"]).unwrap();
    assert_eq!(col.count(Filter::All).unwrap(), 0);
}

#[test]
fn bm25_top_k_is_an_exact_prefix_with_global_frequency_under_prefilters() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.insert_many(
        (0..600)
            .map(|i| {
                vec![
                    (
                        "body".into(),
                        Value::Str(format!("rust {} storage", "rust ".repeat(i % 7))),
                    ),
                    ("bucket".into(), Value::Int((i % 3) as i64)),
                ]
            })
            .collect(),
    )
    .unwrap();
    col.create_fts_index("body").unwrap();
    col.create_index("bucket").unwrap();
    for filter in [None, Some(Filter::Eq("bucket".into(), Value::Int(1)))] {
        let full = col
            .search_text_with(
                "body",
                "rust storage",
                1000,
                &Default::default(),
                filter.clone(),
            )
            .unwrap();
        for k in [1, 10, 256, 599, 600, 1000] {
            let page = col
                .search_text_with(
                    "body",
                    "rust storage",
                    k,
                    &Default::default(),
                    filter.clone(),
                )
                .unwrap();
            assert_eq!(
                page.iter()
                    .map(|r| (r.document.id, r.score.to_bits()))
                    .collect::<Vec<_>>(),
                full.iter()
                    .take(k)
                    .map(|r| (r.document.id, r.score.to_bits()))
                    .collect::<Vec<_>>()
            );
        }
        if filter.is_some() {
            let global = col.search_text("body", "rust storage", 1000).unwrap();
            for hit in full {
                assert_eq!(
                    hit.score.to_bits(),
                    global
                        .iter()
                        .find(|r| r.document.id == hit.document.id)
                        .unwrap()
                        .score
                        .to_bits()
                );
            }
        }
    }
}

#[test]
fn repeated_primary_ids_do_not_duplicate_results_or_apply_updates_twice() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    let id = col.insert(vec![("n".into(), Value::Int(0))]).unwrap();
    let filter = Filter::In("_id".into(), vec![Value::Str(id.to_string()); 300]);
    assert_eq!(col.find(filter.clone()).unwrap().len(), 1);
    assert_eq!(col.count(filter.clone()).unwrap(), 1);
    assert_eq!(
        col.update_many(
            filter.clone(),
            Update::Inc(vec![("n".into(), Value::Int(1))])
        )
        .unwrap(),
        1
    );
    assert_eq!(
        col.find_by_id(id).unwrap().unwrap().get("n"),
        Some(&Value::Int(1))
    );
    assert_eq!(col.delete_many(filter).unwrap(), 1);
}
