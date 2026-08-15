//! Vector-index backfill, on the default feature set.
//!
//! Deliberately not in `vector_hnsw.rs`: that file is `#![cfg(feature =
//! "vector-hnsw")]`, and the allocation these tests guard is the one the
//! default build (browser and React Native) was paying for without the feature.

use taladb_core::{Database, Value, VectorMetric};

// ---------------------------------------------------------------------------
// Backfill
//
// `create_vector_index` builds a decoded copy of every vector only to seed an
// HNSW graph. That copy is now gated on the `vector-hnsw` feature *and* on an
// HNSW index actually being requested — on the default build it was a second
// full copy of every vector that nothing ever read. These tests pin the
// behaviour the gating must not change.
// ---------------------------------------------------------------------------

#[test]
fn flat_index_backfills_existing_documents() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();

    for i in 0..20i64 {
        let x = i as f64 / 20.0;
        col.insert(vec![(
            "emb".into(),
            Value::Array(vec![
                Value::Float(x),
                Value::Float(1.0 - x),
                Value::Float(0.0),
                Value::Float(0.0),
            ]),
        )])
        .unwrap();
    }

    // Index created *after* the writes: everything must be found by search.
    col.create_vector_index("emb", 4, Some(VectorMetric::Cosine), None)
        .unwrap();

    let hits = col
        .find_nearest("emb", &[1.0, 0.0, 0.0, 0.0], 20, None)
        .unwrap();
    assert_eq!(
        hits.len(),
        20,
        "every pre-existing document must be indexed"
    );
}

#[test]
fn backfill_skips_documents_without_a_usable_vector() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();

    col.insert(vec![(
        "emb".into(),
        Value::Array(vec![Value::Float(1.0), Value::Float(0.0)]),
    )])
    .unwrap();
    // No `emb` field at all.
    col.insert(vec![("other".into(), Value::Int(1))]).unwrap();
    // Wrong dimensionality.
    col.insert(vec![("emb".into(), Value::Array(vec![Value::Float(1.0)]))])
        .unwrap();
    // Not a numeric array.
    col.insert(vec![("emb".into(), Value::Str("nope".into()))])
        .unwrap();

    col.create_vector_index("emb", 2, Some(VectorMetric::Cosine), None)
        .unwrap();

    let hits = col.find_nearest("emb", &[1.0, 0.0], 10, None).unwrap();
    assert_eq!(hits.len(), 1, "only the well-formed vector is indexed");
}
