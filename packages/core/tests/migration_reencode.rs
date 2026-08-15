//! Golden test: data written under the pre-terminator string index encoding
//! must survive a `Database::open_with_backend` call and become queryable
//! under the current encoder.

use ulid::Ulid;

use taladb_core::document::Document;
use taladb_core::engine::{RedbBackend, StorageBackend};
use taladb_core::index::{
    IndexDef, META_INDEXES_TABLE, META_VERSION_KEY, META_VERSION_TABLE, docs_table_name,
    index_table_name, meta_key,
};
use taladb_core::{Database, Filter, Value};

/// The exact old-format string index key: `[0x40] ++ utf8_bytes ++ ulid_16_bytes`.
/// No null-escape, no terminator.  This is what earlier builds of TalaDB wrote
/// to disk for `Value::Str` index entries.
fn old_format_str_index_key(value: &str, id: Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + value.len() + 16);
    key.push(0x40);
    key.extend_from_slice(value.as_bytes());
    key.extend_from_slice(&id.to_bytes());
    key
}

#[test]
fn open_rewrites_pre_terminator_string_index() {
    // 1. Build an in-memory backend and seed it with stale data.
    let backend: Box<dyn StorageBackend> = Box::new(RedbBackend::open_in_memory().unwrap());

    let doc_id = Ulid::new();
    let doc = Document::with_id(
        doc_id,
        vec![
            (
                "title".into(),
                Value::Str("Getting Started with Rust".into()),
            ),
            ("category".into(), Value::Str("rust".into())),
        ],
    );

    let col_name = "articles";
    let field = "category";
    let idx_def = IndexDef {
        collection: col_name.into(),
        field: field.into(),
    };

    {
        let mut wtxn = backend.begin_write().unwrap();

        // Doc itself
        let doc_bytes = postcard::to_allocvec(&doc).unwrap();
        wtxn.put(&docs_table_name(col_name), &doc_id.to_bytes(), &doc_bytes)
            .unwrap();

        // Index metadata — this is what `create_index` would write.
        let meta_bytes = postcard::to_allocvec(&idx_def).unwrap();
        wtxn.put(
            META_INDEXES_TABLE,
            meta_key(col_name, field).as_bytes(),
            &meta_bytes,
        )
        .unwrap();

        // Old-format index entry (21 bytes total, no terminator).
        let stale_key = old_format_str_index_key("rust", doc_id);
        assert_eq!(stale_key.len(), 21);
        wtxn.put(&index_table_name(col_name, field), &stale_key, &[])
            .unwrap();

        wtxn.commit().unwrap();
    }

    // Sanity: version is still 0 pre-open (no db_version table written yet).
    {
        let rtxn = backend.begin_read().unwrap();
        assert_eq!(
            taladb_core::migration::read_version(rtxn.as_ref()).unwrap(),
            0
        );
    }

    // 2. Hand the backend to Database::open_with_backend — migration must run.
    let db = Database::open_with_backend(backend).unwrap();

    // 3. Version must now be stamped to the current schema.
    {
        let rtxn = db.backend().begin_read().unwrap();
        assert_eq!(
            taladb_core::migration::read_version(rtxn.as_ref()).unwrap(),
            taladb_core::CURRENT_SCHEMA_VERSION
        );
    }

    // 4. Equality filter on the (previously stale) category index returns the doc.
    let col = db.collection(col_name).unwrap();
    let results = col
        .find(Filter::Eq(field.into(), Value::Str("rust".into())))
        .unwrap();
    assert_eq!(
        results.len(),
        1,
        "re-encoded index must match the stored document"
    );
    assert_eq!(results[0].id, doc_id);
}

#[test]
fn open_is_idempotent_when_already_current() {
    // After one open, reopening the same backend is a no-op.
    // Note: in-memory backends can't be reopened, so we emulate by running
    // the migration twice via `run_migrations` directly.
    let backend = RedbBackend::open_in_memory().unwrap();

    taladb_core::migration::run_migrations(&backend, taladb_core::BUILTIN_MIGRATIONS).unwrap();
    taladb_core::migration::run_migrations(&backend, taladb_core::BUILTIN_MIGRATIONS).unwrap();

    let rtxn = backend.begin_read().unwrap();
    assert_eq!(
        taladb_core::migration::read_version(rtxn.as_ref()).unwrap(),
        taladb_core::CURRENT_SCHEMA_VERSION
    );
}

#[test]
fn v1_to_v2_rebuilds_array_indexes_per_element() {
    let backend: Box<dyn StorageBackend> = Box::new(RedbBackend::open_in_memory().unwrap());
    let doc_id = Ulid::new();
    let col_name = "posts";
    let field = "tags";
    let doc = Document::with_id(
        doc_id,
        vec![(
            field.into(),
            Value::Array(vec![Value::Str("rust".into()), Value::Str("db".into())]),
        )],
    );
    let idx_def = IndexDef {
        collection: col_name.into(),
        field: field.into(),
    };

    {
        let mut wtxn = backend.begin_write().unwrap();
        wtxn.put(
            &docs_table_name(col_name),
            &doc_id.to_bytes(),
            &postcard::to_allocvec(&doc).unwrap(),
        )
        .unwrap();
        wtxn.put(
            META_INDEXES_TABLE,
            meta_key(col_name, field).as_bytes(),
            &postcard::to_allocvec(&idx_def).unwrap(),
        )
        .unwrap();
        // A real 0.10 database has already applied the old v0 -> v1
        // re-encoding migration, before arrays acquired per-element keys.
        wtxn.put(
            META_VERSION_TABLE,
            META_VERSION_KEY,
            &postcard::to_allocvec(&1_u32).unwrap(),
        )
        .unwrap();
        wtxn.commit().unwrap();
    }

    let db = Database::open_with_backend(backend).unwrap();
    let hits = db
        .collection(col_name)
        .unwrap()
        .find(Filter::Eq(field.into(), Value::Str("rust".into())))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, doc_id);
}
