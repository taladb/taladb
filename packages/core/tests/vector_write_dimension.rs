//! The write path rejects vectors whose length disagrees with their index.
//!
//! This used to be a silent skip: the document was stored, no index entry was
//! written, and `find_nearest` could never return it. Nothing surfaced —
//! neither an error at write time nor a clue at read time — so the collection
//! simply had a hole in it. On a vector database that is the failure mode that
//! matters most, because the symptom is a plausible-looking result set with
//! documents quietly missing from it.
//!
//! The rule is deliberately narrow: only a value that *parses* as a numeric
//! vector is length-checked. A missing field and an explicit `null` both remain
//! legal, because "not embedded yet" is an ordinary state for a document.
//!
//! Backfill is the documented exception — see `vector_backfill.rs`.

use taladb_core::Database;
use taladb_core::document::Value;
use taladb_core::vector::VectorMetric;

fn vec_field(v: &[f32]) -> Value {
    Value::Array(v.iter().map(|f| Value::Float(f64::from(*f))).collect())
}

fn indexed_collection(db: &Database) -> taladb_core::Collection {
    let col = db.collection("docs").unwrap();
    col.create_vector_index("embedding", 4, Some(VectorMetric::Cosine), None)
        .unwrap();
    col
}

#[test]
fn insert_with_a_wrong_dimension_vector_is_rejected() {
    let db = Database::open_in_memory().unwrap();
    let col = indexed_collection(&db);

    let err = col
        .insert(vec![("embedding".into(), vec_field(&[1.0, 0.0, 0.0]))])
        .unwrap_err();

    assert!(
        matches!(
            err,
            taladb_core::TalaDbError::VectorDimensionMismatch {
                expected: 4,
                got: 3
            }
        ),
        "expected a dimension mismatch, got {err:?}"
    );
    assert_eq!(
        col.count(taladb_core::Filter::All).unwrap(),
        0,
        "the rejected write must not leave the document behind"
    );
}

#[test]
fn insert_many_rejects_the_whole_batch() {
    let db = Database::open_in_memory().unwrap();
    let col = indexed_collection(&db);

    let err = col.insert_many(vec![
        vec![("embedding".into(), vec_field(&[1.0, 0.0, 0.0, 0.0]))],
        vec![("embedding".into(), vec_field(&[0.0, 1.0]))],
    ]);

    assert!(err.is_err(), "a bad vector anywhere fails the batch");
    assert_eq!(
        col.count(taladb_core::Filter::All).unwrap(),
        0,
        "the transaction rolls back — no partial batch is committed"
    );
}

#[test]
fn update_to_a_wrong_dimension_vector_is_rejected_and_leaves_the_old_one() {
    let db = Database::open_in_memory().unwrap();
    let col = indexed_collection(&db);
    col.insert(vec![
        ("embedding".into(), vec_field(&[1.0, 0.0, 0.0, 0.0])),
        ("k".into(), Value::Str("a".into())),
    ])
    .unwrap();

    let err = col.update_one(
        taladb_core::Filter::Eq("k".into(), Value::Str("a".into())),
        taladb_core::Update::Set(vec![("embedding".into(), vec_field(&[1.0, 2.0]))]),
    );
    assert!(
        err.is_err(),
        "a shrinking update is a dimension mismatch too"
    );

    let hits = col
        .find_nearest("embedding", &[1.0, 0.0, 0.0, 0.0], 10, None)
        .unwrap();
    assert_eq!(hits.len(), 1, "the original document is still indexed");
    assert_eq!(
        hits[0].document.get("embedding"),
        Some(&vec_field(&[1.0, 0.0, 0.0, 0.0])),
        "the stored vector is unchanged"
    );
}

#[test]
fn a_missing_or_null_vector_field_is_still_allowed() {
    let db = Database::open_in_memory().unwrap();
    let col = indexed_collection(&db);

    col.insert(vec![("k".into(), Value::Str("no-embedding".into()))])
        .unwrap();
    col.insert(vec![
        ("k".into(), Value::Str("not-embedded-yet".into())),
        ("embedding".into(), Value::Null),
    ])
    .unwrap();

    assert_eq!(
        col.count(taladb_core::Filter::All).unwrap(),
        2,
        "documents awaiting an embedding are ordinary documents"
    );
    assert!(
        col.find_nearest("embedding", &[1.0, 0.0, 0.0, 0.0], 10, None)
            .unwrap()
            .is_empty(),
        "neither is in the vector index"
    );
}

#[test]
fn a_matching_vector_still_writes_and_searches() {
    let db = Database::open_in_memory().unwrap();
    let col = indexed_collection(&db);
    col.insert(vec![("embedding".into(), vec_field(&[1.0, 0.0, 0.0, 0.0]))])
        .unwrap();
    col.insert(vec![("embedding".into(), vec_field(&[0.0, 1.0, 0.0, 0.0]))])
        .unwrap();

    let hits = col
        .find_nearest("embedding", &[1.0, 0.0, 0.0, 0.0], 1, None)
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].score > 0.99, "the identical vector ranks first");
}

#[test]
fn top_k_of_zero_returns_nothing() {
    let db = Database::open_in_memory().unwrap();
    let col = indexed_collection(&db);
    for i in 0..5 {
        col.insert(vec![(
            "embedding".into(),
            vec_field(&[i as f32, 1.0, 0.0, 0.0]),
        )])
        .unwrap();
    }

    // `top_k = 0` asks for nothing. It used to fall past the top-k partition
    // and return the entire collection — the opposite of the request, and
    // unbounded. `search_text` and `hybrid_search` already guarded this.
    let hits = col
        .find_nearest("embedding", &[1.0, 1.0, 0.0, 0.0], 0, None)
        .unwrap();
    assert!(hits.is_empty(), "got {} results for top_k = 0", hits.len());
}
