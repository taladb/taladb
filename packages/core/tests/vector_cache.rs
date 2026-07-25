//! Correctness of the decoded-vector cache on the flat search path.
//!
//! The cache must never be served stale: flat search is the exact, always-current
//! path. These tests pin the invalidation contract — every kind of write, plus
//! index DDL, cross-collection isolation, and multi-handle sharing — because a
//! missed invalidation would silently return outdated rankings.

use taladb_core::Database;
use taladb_core::document::{Document, Value};

fn vec_val(v: &[f32]) -> Value {
    Value::Array(v.iter().map(|f| Value::Float(*f as f64)).collect())
}

fn insert_vec(col: &taladb_core::Collection, title: &str, v: &[f32]) -> taladb_core::Ulid {
    col.insert(vec![
        ("title".into(), Value::Str(title.into())),
        ("embedding".into(), vec_val(v)),
    ])
    .unwrap()
}

fn titles(results: &[taladb_core::VectorSearchResult]) -> Vec<String> {
    results
        .iter()
        .map(|r| match r.document.get("title") {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        })
        .collect()
}

fn db_with_index() -> Database {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.create_vector_index("embedding", 2, Default::default(), None)
        .unwrap();
    db
}

#[test]
fn repeated_queries_are_consistent() {
    let db = db_with_index();
    let col = db.collection("docs").unwrap();
    insert_vec(&col, "a", &[1.0, 0.0]);
    insert_vec(&col, "b", &[0.0, 1.0]);

    let q = [1.0, 0.0];
    let first = col.find_nearest("embedding", &q, 5, None).unwrap();
    let second = col.find_nearest("embedding", &q, 5, None).unwrap();
    let third = col.find_nearest("embedding", &q, 5, None).unwrap();
    assert_eq!(titles(&first), titles(&second));
    assert_eq!(titles(&second), titles(&third));
    assert_eq!(titles(&first)[0], "a");
}

#[test]
fn insert_after_a_cached_query_is_visible() {
    let db = db_with_index();
    let col = db.collection("docs").unwrap();
    insert_vec(&col, "far", &[0.0, 1.0]);

    // Populate the cache.
    let q = [1.0, 0.0];
    assert_eq!(col.find_nearest("embedding", &q, 5, None).unwrap().len(), 1);

    // A new, closer vector must appear — and rank first.
    insert_vec(&col, "near", &[1.0, 0.0]);
    let after = col.find_nearest("embedding", &q, 5, None).unwrap();
    assert_eq!(after.len(), 2, "cached query must see the new document");
    assert_eq!(titles(&after)[0], "near");
}

#[test]
fn updating_an_embedding_is_visible() {
    let db = db_with_index();
    let col = db.collection("docs").unwrap();
    let id = insert_vec(&col, "doc", &[0.0, 1.0]);

    let q = [1.0, 0.0];
    let before = col.find_nearest("embedding", &q, 5, None).unwrap();
    assert!(before[0].score < 0.5, "starts far from the query");

    // Rewrite the embedding to point straight at the query.
    col.update_one(
        taladb_core::query::Filter::Eq("_id".into(), Value::Str(id.to_string())),
        taladb_core::collection::Update::Set(vec![("embedding".into(), vec_val(&[1.0, 0.0]))]),
    )
    .unwrap();

    let after = col.find_nearest("embedding", &q, 5, None).unwrap();
    assert!(
        after[0].score > 0.99,
        "cache must reflect the updated embedding, got {}",
        after[0].score
    );
}

#[test]
fn delete_after_a_cached_query_is_visible() {
    let db = db_with_index();
    let col = db.collection("docs").unwrap();
    let keep = insert_vec(&col, "keep", &[1.0, 0.0]);
    let _drop = insert_vec(&col, "drop", &[0.9, 0.1]);

    let q = [1.0, 0.0];
    assert_eq!(col.find_nearest("embedding", &q, 5, None).unwrap().len(), 2);

    col.delete_one(taladb_core::query::Filter::Eq(
        "_id".into(),
        Value::Str(_drop.to_string()),
    ))
    .unwrap();

    let after = col.find_nearest("embedding", &q, 5, None).unwrap();
    assert_eq!(titles(&after), vec!["keep"]);
    assert_eq!(after[0].document.id, keep);
}

#[test]
fn dropping_and_recreating_the_index_does_not_serve_stale_vectors() {
    let db = db_with_index();
    let col = db.collection("docs").unwrap();
    insert_vec(&col, "old", &[1.0, 0.0]);
    // Cache it.
    assert_eq!(
        col.find_nearest("embedding", &[1.0, 0.0], 5, None)
            .unwrap()
            .len(),
        1
    );

    // Drop the index (removes vectors), delete the doc, add a different one,
    // recreate the index. A stale cache entry would resurrect "old".
    col.drop_vector_index("embedding").unwrap();
    for d in col.find(taladb_core::query::Filter::All).unwrap() {
        col.delete_one(taladb_core::query::Filter::Eq(
            "_id".into(),
            Value::Str(d.id.to_string()),
        ))
        .unwrap();
    }
    insert_vec(&col, "new", &[0.0, 1.0]);
    col.create_vector_index("embedding", 2, Default::default(), None)
        .unwrap();

    let after = col.find_nearest("embedding", &[0.0, 1.0], 5, None).unwrap();
    assert_eq!(titles(&after), vec!["new"]);
}

#[test]
fn filtered_search_is_correct_with_the_cache() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.create_vector_index("embedding", 2, Default::default(), None)
        .unwrap();
    col.insert(vec![
        ("title".into(), Value::Str("en".into())),
        ("locale".into(), Value::Str("en".into())),
        ("embedding".into(), vec_val(&[1.0, 0.0])),
    ])
    .unwrap();
    col.insert(vec![
        ("title".into(), Value::Str("tl".into())),
        ("locale".into(), Value::Str("tl".into())),
        ("embedding".into(), vec_val(&[1.0, 0.0])),
    ])
    .unwrap();

    let q = [1.0, 0.0];
    // Prime the cache with an unfiltered query, then filter.
    let _ = col.find_nearest("embedding", &q, 5, None).unwrap();
    let filtered = col
        .find_nearest(
            "embedding",
            &q,
            5,
            Some(taladb_core::query::Filter::Eq(
                "locale".into(),
                Value::Str("en".into()),
            )),
        )
        .unwrap();
    assert_eq!(titles(&filtered), vec!["en"]);
}

#[test]
fn writes_to_another_collection_do_not_invalidate_this_ones_results() {
    // Per-collection generation: a chatty collection must not corrupt this
    // collection's cached vectors (it would still be *correct*, just a
    // needless miss — but the results must stay right regardless).
    let db = Database::open_in_memory().unwrap();
    let a = db.collection("a").unwrap();
    a.create_vector_index("embedding", 2, Default::default(), None)
        .unwrap();
    insert_vec(&a, "a1", &[1.0, 0.0]);

    let b = db.collection("b").unwrap();
    b.create_vector_index("embedding", 2, Default::default(), None)
        .unwrap();

    let q = [1.0, 0.0];
    let before = col_titles(&a, &q);
    // Hammer collection b.
    for i in 0..20 {
        insert_vec(&b, &format!("b{i}"), &[0.0, 1.0]);
    }
    let after = col_titles(&a, &q);
    assert_eq!(before, after);
    assert_eq!(after, vec!["a1"]);
}

#[test]
fn a_write_through_one_handle_invalidates_reads_through_another() {
    let db = Database::open_in_memory().unwrap();
    let writer = db.collection("docs").unwrap();
    writer
        .create_vector_index("embedding", 2, Default::default(), None)
        .unwrap();
    insert_vec(&writer, "first", &[0.0, 1.0]);

    // A separate handle to the same collection primes its cache.
    let reader = db.collection("docs").unwrap();
    let q = [1.0, 0.0];
    assert_eq!(
        reader.find_nearest("embedding", &q, 5, None).unwrap().len(),
        1
    );

    // Write through the other handle — the shared cache must invalidate.
    insert_vec(&writer, "closer", &[1.0, 0.0]);
    let after = reader.find_nearest("embedding", &q, 5, None).unwrap();
    assert_eq!(after.len(), 2, "reader must see the writer's insert");
    assert_eq!(titles(&after)[0], "closer");
}

fn col_titles(col: &taladb_core::Collection, q: &[f32]) -> Vec<String> {
    titles(&col.find_nearest("embedding", q, 5, None).unwrap())
}

// Keep the Document import used even if helpers change.
#[allow(dead_code)]
fn _doc_marker(_: &Document) {}
