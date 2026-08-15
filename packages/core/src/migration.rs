use std::ops::Bound;

use crate::document::{Document, Value};
use crate::engine::{KvOp, StorageBackend, WriteTxn};
use crate::error::TalaDbError;
use crate::index::{
    CompoundIndexDef, IndexDef, META_COMPOUND_TABLE, META_INDEXES_TABLE, META_VERSION_KEY,
    META_VERSION_TABLE, compound_table_name, docs_table_name, encode_compound_keys,
    encode_index_keys, index_table_name,
};

/// A single schema migration step.
pub struct Migration {
    pub from_version: u32,
    pub to_version: u32,
    pub description: &'static str,
    pub up: fn(&mut dyn WriteTxn) -> Result<(), TalaDbError>,
}

/// Schema version this build of TalaDB targets — bump whenever a
/// [`BUILTIN_MIGRATIONS`] entry is added.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// Migrations TalaDB runs automatically on every [`Database::open*`].
///
/// v0 → v1 rewrites every single-field and compound secondary index using the
/// current [`encode_index_key`]/[`encode_compound_key`] output.  An earlier
/// release stored string index keys without a null-escape terminator; after
/// that encoder was fixed, old keys fell outside the range produced by
/// `index_range_eq`, so equality filters against persisted data returned zero
/// rows.  This migration re-derives every index from the live docs table.
///
/// v1 → v2 both drops the tables retired with replication and rebuilds indexes.
/// The rebuild is required because 0.11 changed array indexes from "not
/// encodable" to one key per element. Databases already at v1 would otherwise
/// keep empty/stale array indexes forever while newly-written rows used the new
/// representation.
pub const BUILTIN_MIGRATIONS: &[Migration] = &[
    Migration {
        from_version: 0,
        to_version: 1,
        description: "re-encode secondary indexes after string null-terminator fix",
        up: rebuild_all_secondary_indexes,
    },
    Migration {
        from_version: 1,
        to_version: 2,
        description: "drop replication tables and rebuild indexes for array containment",
        up: migrate_v1_to_v2,
    },
];

/// Drop every table belonging to the removed replication engine.
///
/// Deliberately *not* dropped: the `_changed_at` secondary index. Earlier
/// releases created it behind the caller's back to serve changeset exports, but
/// by now an application may equally have created it on purpose, and the two are
/// indistinguishable on disk. Dropping an index somebody's queries depend on is
/// the worse failure, so it is left in place and documented as opt-out —
/// `dropIndex('_changed_at')` removes it.
fn drop_replication_tables(txn: &mut dyn WriteTxn) -> Result<(), TalaDbError> {
    let doomed: Vec<String> = txn
        .list_tables()?
        .into_iter()
        .filter(|name| {
            crate::index::REMOVED_TABLE_PREFIXES
                .iter()
                .any(|p| name.starts_with(p))
        })
        .collect();
    for name in doomed {
        txn.delete_table(&name)?;
    }
    Ok(())
}

fn migrate_v1_to_v2(txn: &mut dyn WriteTxn) -> Result<(), TalaDbError> {
    drop_replication_tables(txn)?;
    rebuild_all_secondary_indexes(txn)
}

/// Read the current database version (0 if unset).
pub fn read_version(txn: &dyn crate::engine::ReadTxn) -> Result<u32, TalaDbError> {
    match txn.get(META_VERSION_TABLE, META_VERSION_KEY)? {
        Some(bytes) => Ok(postcard::from_bytes(&bytes)?),
        None => Ok(0),
    }
}

/// Write the current database version.
fn write_version(txn: &mut dyn WriteTxn, version: u32) -> Result<(), TalaDbError> {
    let bytes = postcard::to_allocvec(&version)?;
    txn.put(META_VERSION_TABLE, META_VERSION_KEY, &bytes)?;
    Ok(())
}

/// Run all pending migrations in version order, each in its own write transaction.
/// Called at database open time before the handle is returned to the caller.
pub fn run_migrations(
    backend: &dyn StorageBackend,
    migrations: &[Migration],
) -> Result<(), TalaDbError> {
    // Validate that migrations form a contiguous chain
    for pair in migrations.windows(2) {
        if pair[0].to_version != pair[1].from_version {
            return Err(TalaDbError::Migration(format!(
                "migration gap: {} -> {} then {} -> {}",
                pair[0].from_version, pair[0].to_version, pair[1].from_version, pair[1].to_version,
            )));
        }
    }

    let rtxn = backend.begin_read()?;
    let mut current_version = read_version(rtxn.as_ref())?;
    drop(rtxn);

    for migration in migrations {
        if migration.from_version < current_version {
            continue; // already applied
        }
        if migration.from_version != current_version {
            return Err(TalaDbError::Migration(format!(
                "migration out of order: db is at v{}, migration starts at v{}",
                current_version, migration.from_version
            )));
        }

        let mut wtxn = backend.begin_write()?;
        (migration.up)(wtxn.as_mut())?;
        write_version(wtxn.as_mut(), migration.to_version)?;
        wtxn.commit()?;
        current_version = migration.to_version;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Built-in migration bodies
// ---------------------------------------------------------------------------

fn rebuild_all_secondary_indexes(txn: &mut dyn WriteTxn) -> Result<(), TalaDbError> {
    rebuild_secondary_indexes(txn)?;
    rebuild_compound_indexes(txn)?;
    Ok(())
}

/// Clear and re-derive every single-field secondary index from the current
/// docs table using the current [`encode_index_key`] output.
pub fn rebuild_secondary_indexes(txn: &mut dyn WriteTxn) -> Result<(), TalaDbError> {
    let meta = txn.range(META_INDEXES_TABLE, Bound::Unbounded, Bound::Unbounded)?;
    let mut defs: Vec<IndexDef> = Vec::with_capacity(meta.len());
    for (_, v) in meta {
        defs.push(postcard::from_bytes(&v)?);
    }

    for def in &defs {
        let idx_table = index_table_name(&def.collection, &def.field);
        let old_keys = txn.range(&idx_table, Bound::Unbounded, Bound::Unbounded)?;

        let docs_table = docs_table_name(&def.collection);
        let docs = txn.range(&docs_table, Bound::Unbounded, Bound::Unbounded)?;
        let mut new_keys: Vec<Vec<u8>> = Vec::with_capacity(docs.len());
        for (_, doc_bytes) in docs {
            let doc: Document = postcard::from_bytes(&doc_bytes)?;
            if let Some(val) = doc.get(&def.field) {
                // Per-element keys for arrays — the rebuild must produce what
                // the current write path produces, or an array field would stay
                // unqueryable on any database that came through this migration.
                new_keys.extend(encode_index_keys(val, doc.id));
            }
        }

        // Clear-then-rebuild as one batch: this runs at open time on every
        // database still at v0, so it is squarely on the cold-start path.
        let ops: Vec<KvOp<'_>> = old_keys
            .iter()
            .map(|(k, _)| KvOp::Delete(k.as_slice()))
            .chain(new_keys.iter().map(|k| KvOp::Put(k.as_slice(), &[])))
            .collect();
        txn.apply_batch(&idx_table, &ops)?;
    }
    Ok(())
}

/// Clear and re-derive every compound secondary index from the current docs
/// table using the current [`encode_compound_key`] output.
pub fn rebuild_compound_indexes(txn: &mut dyn WriteTxn) -> Result<(), TalaDbError> {
    let meta = txn.range(META_COMPOUND_TABLE, Bound::Unbounded, Bound::Unbounded)?;
    let mut defs: Vec<CompoundIndexDef> = Vec::with_capacity(meta.len());
    for (_, v) in meta {
        defs.push(postcard::from_bytes(&v)?);
    }

    for def in &defs {
        let fields: Vec<&str> = def.fields.iter().map(std::string::String::as_str).collect();
        let ctable = compound_table_name(&def.collection, &fields);

        let old_keys = txn.range(&ctable, Bound::Unbounded, Bound::Unbounded)?;

        let docs_table = docs_table_name(&def.collection);
        let docs = txn.range(&docs_table, Bound::Unbounded, Bound::Unbounded)?;
        let mut new_keys: Vec<Vec<u8>> = Vec::with_capacity(docs.len());
        for (_, doc_bytes) in docs {
            let doc: Document = postcard::from_bytes(&doc_bytes)?;
            let vals: Option<Vec<&Value>> = def.fields.iter().map(|f| doc.get(f)).collect();
            if let Some(v) = vals {
                new_keys.extend(encode_compound_keys(&v, doc.id)?);
            }
        }

        let ops: Vec<KvOp<'_>> = old_keys
            .iter()
            .map(|(k, _)| KvOp::Delete(k.as_slice()))
            .chain(new_keys.iter().map(|k| KvOp::Put(k.as_slice(), &[])))
            .collect();
        txn.apply_batch(&ctable, &ops)?;
    }
    Ok(())
}
