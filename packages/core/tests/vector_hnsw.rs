//! Integration tests for HNSW vector index (requires `vector-hnsw` feature).
//!
//! Run with:
//!   cargo test --features vector-hnsw -p taladb-core --test vector_hnsw

#![cfg(feature = "vector-hnsw")]

use taladb_core::{Database, Filter, HnswOptions, Value, VectorMetric};

fn vec_val(floats: &[f64]) -> Value {
    Value::Array(floats.iter().map(|&f| Value::Float(f)).collect())
}

fn fv(floats: &[f32]) -> Vec<f32> {
    floats.to_vec()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Insert `n` documents with a 4-D embedding equal to [i as f32, 0, 0, 0].
fn seed_collection(db: &Database, col: &str, n: usize) {
    let collection = db.collection(col).unwrap();
    for i in 0..n {
        collection
            .insert(vec![
                ("label".into(), Value::Str(format!("item-{i}"))),
                ("emb".into(), vec_val(&[i as f64, 0.0, 0.0, 0.0])),
            ])
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Basic HNSW round-trip
// ---------------------------------------------------------------------------

#[test]
fn hnsw_create_and_search() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("items").unwrap();

    // Insert a handful of 4-D vectors
    for (i, v) in [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
    ]
    .iter()
    .enumerate()
    {
        col.insert(vec![
            ("label".into(), Value::Str(format!("item-{i}"))),
            ("emb".into(), vec_val(v)),
        ])
        .unwrap();
    }

    col.create_vector_index(
        "emb",
        4,
        Some(VectorMetric::Cosine),
        Some(HnswOptions {
            m: 8,
            ef_construction: 50,
        }),
    )
    .unwrap();

    // Query closest to [1,0,0,0] — should return item-0 first
    let results = col
        .find_nearest("emb", &fv(&[1.0, 0.0, 0.0, 0.0]), 2, None)
        .unwrap();
    assert!(!results.is_empty());
    let top_label = results[0].document.get("label").unwrap().clone();
    assert_eq!(top_label, Value::Str("item-0".into()));
}

// ---------------------------------------------------------------------------
// upgrade_vector_index rebuilds the in-memory cache
// ---------------------------------------------------------------------------

#[test]
fn upgrade_vector_index_warms_cache() {
    let db = Database::open_in_memory().unwrap();
    seed_collection(&db, "docs", 20);

    // Create a flat index first (no HNSW)
    let col = db.collection("docs").unwrap();
    col.create_vector_index("emb", 4, Some(VectorMetric::Cosine), None)
        .unwrap();

    // Promote to HNSW by storing options, then upgrading
    // (Simulate the two-step flow: store opts, then call upgrade)
    col.create_vector_index(
        "emb",
        4,
        Some(VectorMetric::Cosine),
        Some(HnswOptions::default()),
    )
    .unwrap(); // idempotent — index already exists, but we re-call to store HNSW opts

    col.upgrade_vector_index("emb").unwrap();

    // HNSW search should now work
    let results = col
        .find_nearest("emb", &fv(&[5.0, 0.0, 0.0, 0.0]), 3, None)
        .unwrap();
    assert_eq!(results.len(), 3);
}

// ---------------------------------------------------------------------------
// rebuild_hnsw_indexes warms cache across handles
// ---------------------------------------------------------------------------

#[test]
fn rebuild_hnsw_indexes_on_open() {
    let db = Database::open_in_memory().unwrap();
    seed_collection(&db, "vecs", 10);

    {
        let col = db.collection("vecs").unwrap();
        col.create_vector_index(
            "emb",
            4,
            Some(VectorMetric::Euclidean),
            Some(HnswOptions {
                m: 4,
                ef_construction: 40,
            }),
        )
        .unwrap();
    }

    // Warm cache for all HNSW indexes (simulates what a caller does after open)
    db.rebuild_hnsw_indexes().unwrap();

    // A freshly obtained collection handle shares the same cache
    let col2 = db.collection("vecs").unwrap();
    let results = col2
        .find_nearest("emb", &fv(&[3.0, 0.0, 0.0, 0.0]), 1, None)
        .unwrap();
    assert_eq!(results.len(), 1);
}

// ---------------------------------------------------------------------------
// Cache is evicted on drop_vector_index
// ---------------------------------------------------------------------------

#[test]
fn drop_vector_index_clears_cache() {
    let db = Database::open_in_memory().unwrap();
    seed_collection(&db, "drop_test", 5);

    let col = db.collection("drop_test").unwrap();
    col.create_vector_index(
        "emb",
        4,
        Some(VectorMetric::Cosine),
        Some(HnswOptions::default()),
    )
    .unwrap();

    col.drop_vector_index("emb").unwrap();

    // After drop, find_nearest should return an error (index gone)
    let err = col
        .find_nearest("emb", &fv(&[1.0, 0.0, 0.0, 0.0]), 1, None)
        .unwrap_err();
    assert!(
        matches!(err, taladb_core::TalaDbError::VectorIndexNotFound(_)),
        "expected VectorIndexNotFound, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// HNSW falls back to flat when cache is cold (no upgrade called)
// ---------------------------------------------------------------------------

#[test]
fn flat_fallback_when_no_graph_in_cache() {
    let db = Database::open_in_memory().unwrap();
    seed_collection(&db, "fallback", 8);

    let col = db.collection("fallback").unwrap();
    // Create flat-only index (no HNSW opts)
    col.create_vector_index("emb", 4, Some(VectorMetric::Cosine), None)
        .unwrap();

    // Should still work via flat search
    let results = col
        .find_nearest("emb", &fv(&[2.0, 0.0, 0.0, 0.0]), 2, None)
        .unwrap();
    assert_eq!(results.len(), 2);
}

// ---------------------------------------------------------------------------
// Pre-filter is respected (forces flat path even with HNSW)
// ---------------------------------------------------------------------------

#[test]
fn pre_filter_forces_flat_path() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("filtered").unwrap();

    for i in 0..10_usize {
        col.insert(vec![
            (
                "tag".into(),
                Value::Str(if i < 5 { "a" } else { "b" }.into()),
            ),
            ("emb".into(), vec_val(&[i as f64, 0.0, 0.0, 0.0])),
        ])
        .unwrap();
    }

    col.create_vector_index(
        "emb",
        4,
        Some(VectorMetric::Cosine),
        Some(HnswOptions::default()),
    )
    .unwrap();

    // Only documents tagged "a" (i in 0..5) should appear
    let results = col
        .find_nearest(
            "emb",
            &fv(&[4.0, 0.0, 0.0, 0.0]),
            10,
            Some(Filter::Eq("tag".into(), Value::Str("a".into()))),
        )
        .unwrap();

    assert!(!results.is_empty());
    for r in &results {
        assert_eq!(r.document.get("tag"), Some(&Value::Str("a".into())));
    }
}

// ---------------------------------------------------------------------------
// Multiple collections share the same Database but have independent caches
// ---------------------------------------------------------------------------

#[test]
fn multiple_collections_independent() {
    let db = Database::open_in_memory().unwrap();
    seed_collection(&db, "col_a", 5);
    seed_collection(&db, "col_b", 5);

    let col_a = db.collection("col_a").unwrap();
    let col_b = db.collection("col_b").unwrap();

    col_a
        .create_vector_index(
            "emb",
            4,
            Some(VectorMetric::Cosine),
            Some(HnswOptions::default()),
        )
        .unwrap();
    col_b
        .create_vector_index(
            "emb",
            4,
            Some(VectorMetric::Cosine),
            Some(HnswOptions::default()),
        )
        .unwrap();

    let ra = col_a
        .find_nearest("emb", &fv(&[1.0, 0.0, 0.0, 0.0]), 1, None)
        .unwrap();
    let rb = col_b
        .find_nearest("emb", &fv(&[1.0, 0.0, 0.0, 0.0]), 1, None)
        .unwrap();

    assert_eq!(ra.len(), 1);
    assert_eq!(rb.len(), 1);
}

// ---------------------------------------------------------------------------
// A caller-supplied `top_k` must not overflow the HNSW over-fetch margin
// ---------------------------------------------------------------------------

/// The HNSW branch over-fetches by `top_k + (top_k / 5).max(8)` so churn in the
/// graph doesn't starve the caller of results. That addition used to be a plain
/// `+`, so a `top_k` near the top of the range panicked with "attempt to add
/// with overflow" — reachable from every binding, since `top_k` is passed
/// straight through from JS (`u32`, and a 32-bit `usize` on wasm32) and from the
/// React Native FFI (a raw `usize`).
///
/// Only the graph path over-fetches, so the cache has to be warm for this to
/// exercise anything: a cold collection takes the flat path and never computes
/// `fetch_k` at all.
#[test]
fn a_huge_top_k_saturates_the_hnsw_over_fetch() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("items").unwrap();
    seed_collection(&db, "items", 6);
    col.create_vector_index(
        "emb",
        4,
        Some(VectorMetric::Cosine),
        Some(HnswOptions {
            m: 8,
            ef_construction: 50,
        }),
    )
    .unwrap();

    // Warm the graph cache so the next call takes the HNSW path.
    col.find_nearest("emb", &fv(&[1.0, 0.0, 0.0, 0.0]), 1, None)
        .unwrap();

    let results = col
        .find_nearest("emb", &fv(&[1.0, 0.0, 0.0, 0.0]), usize::MAX, None)
        .expect("a top_k past the overflow boundary must still answer");

    // Asking for more than the graph holds returns what it holds.
    assert!(!results.is_empty());
    assert!(results.len() <= 6);
}
