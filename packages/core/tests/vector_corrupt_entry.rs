//! Flat vector scan against entries that don't match the index dimension.
//!
//! The write path enforces `vec.len() == dimensions`, so these entries cannot
//! arrive through `insert`. They can arrive through `restore_from_snapshot`,
//! which writes raw table bytes straight from the blob, and through on-disk
//! corruption — `decode_f32_vec` accepts any byte count that is a multiple of
//! four, so a truncated value decodes into a short vector rather than failing.
//!
//! The scoring reductions walk query and stored vector in lockstep with
//! `chunks_exact` and stop at the shorter of the two, so without an explicit
//! length check a truncated vector is scored over its common prefix — and a
//! short prefix can easily out-score a full-length vector, putting a corrupt
//! entry at the top of the results.

use taladb_core::Database;
use taladb_core::document::Value;
use taladb_core::vector::{VectorMetric, encode_f32_vec, vec_table_name};

fn vec_field(v: &[f32]) -> Value {
    Value::Array(v.iter().map(|f| Value::Float(f64::from(*f))).collect())
}

#[test]
fn truncated_stored_vector_is_skipped_not_partially_scored() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();

    // Four dimensions; the query is a unit vector along the first axis.
    col.create_vector_index("embedding", 4, Some(VectorMetric::Cosine), None)
        .unwrap();

    let good = col
        .insert(vec![
            ("name".into(), Value::Str("good".into())),
            ("embedding".into(), vec_field(&[0.5, 0.5, 0.5, 0.5])),
        ])
        .unwrap();

    // A second, real document — so it survives the id→document fetch — whose
    // *vector-table* entry is then truncated behind the write path's back,
    // exactly as a crafted snapshot or a half-written page would leave it.
    let corrupt_id = col
        .insert(vec![
            ("name".into(), Value::Str("corrupt".into())),
            ("embedding".into(), vec_field(&[0.0, 0.0, 0.0, 1.0])),
        ])
        .unwrap();
    {
        let table = vec_table_name("docs", "embedding");
        let mut wtxn = db.backend().begin_write().unwrap();
        wtxn.put(&table, &corrupt_id.to_bytes(), &encode_f32_vec(&[1.0, 0.0]))
            .unwrap();
        wtxn.commit().unwrap();
    }

    // A perfect match on the truncated prefix: cosine over the first two
    // elements is 1.0, which beats the real document's 0.5.
    let results = col
        .find_nearest("embedding", &[1.0, 0.0, 0.0, 0.0], 10, None)
        .unwrap();

    assert!(
        results.iter().all(|r| r.document.id != corrupt_id),
        "a wrong-dimension entry must not be scored into the results"
    );
    assert_eq!(results.len(), 1, "only the well-formed document remains");
    assert_eq!(results[0].document.id, good);
}

/// The same guard must not disturb ordinary, correctly-sized entries.
#[test]
fn well_formed_vectors_are_still_ranked_normally() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.create_vector_index("embedding", 3, Some(VectorMetric::Cosine), None)
        .unwrap();

    let near = col
        .insert(vec![
            ("name".into(), Value::Str("near".into())),
            ("embedding".into(), vec_field(&[1.0, 0.1, 0.0])),
        ])
        .unwrap();
    col.insert(vec![
        ("name".into(), Value::Str("far".into())),
        ("embedding".into(), vec_field(&[0.0, 0.0, 1.0])),
    ])
    .unwrap();

    let results = col
        .find_nearest("embedding", &[1.0, 0.0, 0.0], 10, None)
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].document.id, near, "closest vector ranks first");
}
