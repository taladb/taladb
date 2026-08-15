//! v1 → v2: the tombstone and quarantine tables that backed replication are
//! dropped when a database written by an earlier release is opened.
//!
//! The tables are seeded by raw key writes rather than through the old APIs,
//! which no longer exist — what matters to the migration is the table name.

use taladb_core::document::{Document, Value};
use taladb_core::engine::{RedbBackend, StorageBackend};
use taladb_core::{Database, Filter};

/// Seed the tables a pre-0.11 database would carry, and stamp it as v1 so only
/// the new migration is pending.
fn seed_pre_0_11(backend: &dyn StorageBackend) {
    let mut wtxn = backend.begin_write().unwrap();
    wtxn.put("tomb::notes", &[1u8; 16], &postcard::to_allocvec(&1_i64).unwrap())
        .unwrap();
    wtxn.put("tomb::tasks", &[2u8; 16], &postcard::to_allocvec(&2_i64).unwrap())
        .unwrap();
    wtxn.put("quarantine::notes", &[3u8; 16], b"whatever").unwrap();
    // A collection whose name merely *contains* the prefix must survive.
    wtxn.put("docs::tombolas", &[4u8; 16], b"keep me").unwrap();
    wtxn.put(
        "meta::version",
        b"version",
        &postcard::to_allocvec(&1_u32).unwrap(),
    )
    .unwrap();
    wtxn.commit().unwrap();
}

fn table_names(backend: &dyn StorageBackend) -> Vec<String> {
    let rtxn = backend.begin_read().unwrap();
    let mut names = rtxn.list_tables().unwrap();
    names.sort();
    names
}

#[test]
fn open_drops_tombstone_and_quarantine_tables() {
    let backend = RedbBackend::open_in_memory().unwrap();
    seed_pre_0_11(&backend);

    taladb_core::migration::run_migrations(&backend, taladb_core::BUILTIN_MIGRATIONS).unwrap();

    let names = table_names(&backend);
    assert!(
        !names.iter().any(|n| n.starts_with("tomb::")),
        "tombstone tables must be gone, got {names:?}",
    );
    assert!(
        !names.iter().any(|n| n.starts_with("quarantine::")),
        "quarantine tables must be gone, got {names:?}",
    );
    assert!(
        names.iter().any(|n| n == "docs::tombolas"),
        "a collection whose name starts with the prefix text must survive, got {names:?}",
    );

    let rtxn = backend.begin_read().unwrap();
    assert_eq!(
        taladb_core::migration::read_version(rtxn.as_ref()).unwrap(),
        taladb_core::CURRENT_SCHEMA_VERSION,
    );
}

#[test]
fn migration_is_idempotent_and_safe_on_a_fresh_database() {
    let backend = RedbBackend::open_in_memory().unwrap();
    // No replication tables to find, and running twice must not error.
    taladb_core::migration::run_migrations(&backend, taladb_core::BUILTIN_MIGRATIONS).unwrap();
    taladb_core::migration::run_migrations(&backend, taladb_core::BUILTIN_MIGRATIONS).unwrap();

    let rtxn = backend.begin_read().unwrap();
    assert_eq!(
        taladb_core::migration::read_version(rtxn.as_ref()).unwrap(),
        taladb_core::CURRENT_SCHEMA_VERSION,
    );
}

#[test]
fn documents_and_indexes_are_untouched_by_the_drop() {
    let backend: Box<dyn StorageBackend> = Box::new(RedbBackend::open_in_memory().unwrap());
    let db = Database::open_with_backend(backend).unwrap();
    let notes = db.collection("notes").unwrap();
    notes.create_index("tag").unwrap();
    let id = notes
        .insert(vec![
            ("tag".into(), Value::Str("x".into())),
            ("body".into(), Value::Str("hello".into())),
        ])
        .unwrap();
    notes.delete_by_id(id).unwrap();
    let kept = notes
        .insert(vec![("tag".into(), Value::Str("x".into()))])
        .unwrap();

    // The migration already ran at open; the collection must still be intact.
    let found: Vec<Document> = notes
        .find(Filter::Eq("tag".into(), Value::Str("x".into())))
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, kept);
}
