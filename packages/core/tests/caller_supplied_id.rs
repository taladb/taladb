//! `insert` honours a caller-supplied `_id`.
//!
//! It used to discard it silently. The document came back under a fresh ULID,
//! `find({ _id: … })` on the supplied value matched nothing, and re-running a
//! seed script produced a second copy of every row instead of recognising the
//! first. The three runtimes even discarded it in two different ways — the web
//! and React Native bindings filtered the field out on the way in, Node passed
//! it through and let the read path overwrite it.
//!
//! Since replication was removed, hydrating from a server is ordinary
//! application code — fetch, then `insert_many` — and that code is only safe to
//! run twice if the caller controls the id.

use std::sync::{Arc, Barrier};
use taladb_core::document::Value;
use taladb_core::{Database, Filter, TalaDbError};
use ulid::Ulid;

const ID_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const ID_B: &str = "01BX5ZZKBKACTAV9WEVGEMMVRZ";

fn collection() -> (Database, taladb_core::Collection) {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("products").unwrap();
    (db, col)
}

#[test]
fn a_supplied_id_is_the_document_id() {
    let (_db, col) = collection();
    let id = col
        .insert(vec![
            ("_id".into(), Value::Str(ID_A.into())),
            ("name".into(), Value::Str("Kettle".into())),
        ])
        .unwrap();

    assert_eq!(id.to_string(), ID_A, "insert reports the id it was given");
    let found = col
        .find_one(Filter::Eq("_id".into(), Value::Str(ID_A.into())))
        .unwrap()
        .expect("the document is retrievable by the supplied id");
    assert_eq!(found.id, Ulid::from_string(ID_A).unwrap());
    assert_eq!(
        found.get("_id"),
        None,
        "`_id` is the document's identity, not one of its fields"
    );
}

#[test]
fn re_inserting_the_same_id_is_refused_rather_than_duplicated() {
    let (_db, col) = collection();
    col.insert(vec![
        ("_id".into(), Value::Str(ID_A.into())),
        ("name".into(), Value::Str("Kettle".into())),
    ])
    .unwrap();

    let err = col
        .insert(vec![
            ("_id".into(), Value::Str(ID_A.into())),
            ("name".into(), Value::Str("Kettle (again)".into())),
        ])
        .unwrap_err();

    assert!(
        matches!(err, TalaDbError::DuplicateId(_)),
        "expected a duplicate id error, got {err:?}"
    );
    assert_eq!(
        col.count(Filter::All).unwrap(),
        1,
        "the collection still holds one document"
    );
    let stored = col
        .find_one(Filter::Eq("_id".into(), Value::Str(ID_A.into())))
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.get("name"),
        Some(&Value::Str("Kettle".into())),
        "insert must never overwrite — a re-run seed cannot clobber later edits"
    );
}

#[test]
fn insert_many_rejects_a_duplicate_within_its_own_batch() {
    let (_db, col) = collection();
    let err = col
        .insert_many(vec![
            vec![("_id".into(), Value::Str(ID_A.into()))],
            vec![("_id".into(), Value::Str(ID_A.into()))],
        ])
        .unwrap_err();

    assert!(matches!(err, TalaDbError::DuplicateId(_)), "got {err:?}");
    assert_eq!(col.count(Filter::All).unwrap(), 0, "the batch rolled back");
}

#[test]
fn concurrent_inserts_with_one_supplied_id_have_one_winner() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let barrier = Arc::new(Barrier::new(12));
    let mut threads = Vec::new();

    for n in 0..12 {
        let db = Arc::clone(&db);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            let col = db.collection("products").unwrap();
            barrier.wait();
            col.insert(vec![
                ("_id".into(), Value::Str(ID_A.into())),
                ("writer".into(), Value::Int(n)),
            ])
        }));
    }

    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(TalaDbError::DuplicateId(_))))
            .count(),
        11
    );
    assert_eq!(
        db.collection("products")
            .unwrap()
            .count(Filter::All)
            .unwrap(),
        1
    );
}

#[test]
fn supplied_and_generated_ids_mix_in_one_batch() {
    let (_db, col) = collection();
    let ids = col
        .insert_many(vec![
            vec![("_id".into(), Value::Str(ID_A.into()))],
            vec![("name".into(), Value::Str("no id".into()))],
            vec![("_id".into(), Value::Str(ID_B.into()))],
        ])
        .unwrap();

    assert_eq!(ids[0].to_string(), ID_A);
    assert_eq!(ids[2].to_string(), ID_B);
    assert_ne!(ids[1].to_string(), ID_A);
    assert_eq!(col.count(Filter::All).unwrap(), 3);
}

#[test]
fn a_non_ulid_id_is_refused_with_a_message_pointing_at_derive_doc_id() {
    let (_db, col) = collection();
    let err = col
        .insert(vec![("_id".into(), Value::Str("sku-1".into()))])
        .unwrap_err();

    assert!(
        matches!(err, TalaDbError::InvalidDocumentId(_)),
        "got {err:?}"
    );
    assert!(
        err.to_string().contains("deriveDocId"),
        "the error must say how to get a usable id from a natural key: {err}"
    );
}

#[test]
fn a_non_string_id_is_refused() {
    let (_db, col) = collection();
    let err = col.insert(vec![("_id".into(), Value::Int(7))]).unwrap_err();
    assert!(
        matches!(err, TalaDbError::InvalidDocumentId(_)),
        "got {err:?}"
    );
}

#[test]
fn a_supplied_id_indexes_and_searches_like_any_other() {
    let (_db, col) = collection();
    col.create_index("name").unwrap();
    col.insert(vec![
        ("_id".into(), Value::Str(ID_A.into())),
        ("name".into(), Value::Str("Kettle".into())),
    ])
    .unwrap();

    let hits = col
        .find(Filter::Eq("name".into(), Value::Str("Kettle".into())))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id.to_string(), ID_A);

    assert!(
        col.delete_one(Filter::Eq("_id".into(), Value::Str(ID_A.into())))
            .unwrap(),
        "and deletes by that id"
    );
    assert_eq!(col.count(Filter::All).unwrap(), 0);
}
