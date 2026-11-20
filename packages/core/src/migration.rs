use std::ops::Bound;

use crate::document::{Document, Value};
use crate::engine::{KvOp, ReadTxn, ScanFlow, StorageBackend, WriteTxn};
use crate::error::TalaDbError;
use crate::index::{
    CompoundIndexDef, IndexDef, META_COMPOUND_TABLE, META_INDEX_ARRAYS_TABLE, META_INDEXES_TABLE,
    META_VERSION_KEY, META_VERSION_TABLE, compound_table_name, docs_table_name,
    encode_compound_keys, encode_index_keys, index_table_name, meta_key,
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
///
/// Only the exact encoding [`write_version`] produces is accepted: one postcard
/// `u32` with nothing after it. `postcard::from_bytes` alone ignores trailing
/// bytes, and that is how an *encrypted* version record used to be read as
/// plaintext: the ciphertext's leading `0x01` format byte decoded as "version
/// 1" and the remaining 29 bytes were dropped, so the v1→v2 migration ran on
/// ciphertext (S1).
pub fn read_version(txn: &dyn ReadTxn) -> Result<u32, TalaDbError> {
    match txn.get(META_VERSION_TABLE, META_VERSION_KEY)? {
        Some(bytes) => decode_version(&bytes),
        None => Ok(0),
    }
}

/// The shortest value [`EncryptedBackend`](crate::crypto::EncryptedBackend)
/// can store: a 12-byte nonce and a 16-byte GCM tag around an empty
/// plaintext, in the pre-0.7.3 layout. The current layout adds a format byte,
/// so it is never shorter. A plaintext version record is at most 5 bytes.
const MIN_STORED_CIPHERTEXT: usize = 28;

fn decode_version(bytes: &[u8]) -> Result<u32, TalaDbError> {
    match postcard::take_from_bytes::<u32>(bytes) {
        Ok((version, [])) => Ok(version),
        _ if bytes.len() >= MIN_STORED_CIPHERTEXT => {
            Err(unreadable("schema version record", bytes))
        }
        // Not decodable at all: the decode error, as before.
        Err(e) => Err(e.into()),
        // Decodes, with bytes left over: never something this build wrote.
        Ok(_) => Err(unreadable("schema version record", bytes)),
    }
}

/// The error for stored metadata that is not what this build writes. Long
/// enough to be ciphertext → most likely an encrypted database opened without
/// its key (or a plaintext one with a key it was never encrypted with).
fn unreadable(what: &str, bytes: &[u8]) -> TalaDbError {
    if bytes.len() >= MIN_STORED_CIPHERTEXT {
        TalaDbError::Encryption(format!(
            "the database's {what} cannot be read with the key it was opened with \
             (none, or a different one): the database is encrypted, or is not \
             encrypted the way this open assumes, or is damaged. Nothing was changed"
        ))
    } else {
        TalaDbError::Migration(format!(
            "the database's {what} is malformed ({} bytes); refusing to migrate it. \
             Nothing was changed",
            bytes.len()
        ))
    }
}

/// What a database's stored bytes say, read through one backend, before any
/// migration is allowed to write.
pub(crate) enum StoredState {
    /// No version record and no stored document: nothing a migration could
    /// misread.
    Empty,
    /// Readable through this backend, at this schema version (0 when the
    /// database predates the version record).
    Readable(u32),
    /// Present but not readable through this backend.
    Unreadable(TalaDbError),
}

/// Classify the database behind `txn` without writing anything.
///
/// This is what makes an open in the wrong encryption mode fail *before* any
/// migration runs (S1). It decides from the stored bytes alone — not from a
/// marker or the `.taladb-salt` sidecar — so it also covers databases written
/// before either existed, and custom backends such as OPFS:
///
/// - A version record must decode exactly ([`read_version`]). Every open since
///   0.7.3 writes one, so this alone settles nearly every database, at the cost
///   of the single read `run_migrations` always made.
/// - Without one (releases before 0.7.3 never wrote it), the v0 chain would
///   rebuild indexes from the documents, so the first value of every
///   collection must not be ciphertext: either it decodes as a [`Document`]
///   with nothing left over, or it is too short to be ciphertext. A plaintext
///   document always starts with its ULID as a 26-character string; ciphertext
///   read without its key never decodes that way.
///
/// Through an `EncryptedBackend`, the same reads decrypt, so a wrong key fails
/// here as the cipher's authentication error, also before any write.
pub(crate) fn stored_state(txn: &dyn ReadTxn) -> Result<StoredState, TalaDbError> {
    if let Some(bytes) = txn.get(META_VERSION_TABLE, META_VERSION_KEY)? {
        return Ok(match decode_version(&bytes) {
            Ok(version) => StoredState::Readable(version),
            Err(e) => StoredState::Unreadable(e),
        });
    }
    probe_documents(txn)
}

/// The document half of [`stored_state`]: reads the first value of every
/// collection through `txn`. `Unreadable` when one is long enough to be
/// ciphertext and is not exactly a [`Document`]; otherwise `Readable(0)` when
/// any collection holds a value, `Empty` when none does.
pub(crate) fn probe_documents(txn: &dyn ReadTxn) -> Result<StoredState, TalaDbError> {
    let mut saw_plaintext = false;
    for table in txn.list_tables()? {
        if !table.starts_with(crate::index::DOCS_TABLE_PREFIX) {
            continue;
        }
        let mut first: Option<Vec<u8>> = None;
        txn.scan(
            &table,
            Bound::Unbounded,
            Bound::Unbounded,
            &mut |_, value| {
                first = Some(value.to_vec());
                Ok(ScanFlow::Stop)
            },
        )?;
        let Some(bytes) = first else { continue };
        let is_document = matches!(postcard::take_from_bytes::<Document>(&bytes), Ok((_, [])));
        // Refuse only what could be ciphertext and is not a document. A value
        // too short to be ciphertext is plaintext even if it is not a valid
        // document; it stays the reader's problem, exactly as before this check
        // existed, rather than making the whole database unopenable.
        if !is_document && bytes.len() >= MIN_STORED_CIPHERTEXT {
            return Ok(StoredState::Unreadable(unreadable(
                &format!("first stored document in `{table}`"),
                &bytes,
            )));
        }
        saw_plaintext = true;
    }
    Ok(if saw_plaintext {
        StoredState::Readable(0)
    } else {
        StoredState::Empty
    })
}

/// Length of the header every plaintext [`Document`] starts with: postcard's
/// one-byte length prefix (26) and the 26-character ULID of its `id`.
const DOCUMENT_HEADER_LEN: usize = 27;

/// Whether `bytes` starts the way every plaintext [`Document`] this crate has
/// ever written starts: its ULID as a 26-character Crockford base32 string.
/// Stored ciphertext starts with the `0x01` format byte (or, before 0.7.3, a
/// random nonce), so it never matches. Only the header is read, so a document
/// that is otherwise unreadable — nested past `MAX_VALUE_DEPTH`, or from a newer
/// build — still counts as plaintext here and stays the reader's problem.
fn has_document_header(bytes: &[u8]) -> bool {
    match bytes.get(..DOCUMENT_HEADER_LEN) {
        Some([26, ulid @ ..]) => ulid
            .iter()
            .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z')),
        _ => false,
    }
}

/// Refuse a database whose version record is readable but whose documents are
/// stored ciphertext — what an open without the passphrase in 0.13.1 and
/// earlier leaves behind (S1). Such a file decodes its version record as
/// plaintext, so [`stored_state`] alone lets the plain open through, and
/// writes would then mix plaintext into the encrypted file. Only the first
/// value of each collection is read, before anything is written.
///
/// Through an `EncryptedBackend` the reads decrypt to plaintext and pass. A
/// value that fails to decrypt there is not this damage (the version record
/// already decrypted with the same key), so it is left to the reader, as before.
fn refuse_ciphertext_documents(txn: &dyn ReadTxn) -> Result<(), TalaDbError> {
    for table in txn.list_tables()? {
        if !table.starts_with(crate::index::DOCS_TABLE_PREFIX) {
            continue;
        }
        let mut ciphertext = false;
        let scanned = txn.scan(
            &table,
            Bound::Unbounded,
            Bound::Unbounded,
            &mut |_, value| {
                ciphertext = value.len() >= MIN_STORED_CIPHERTEXT && !has_document_header(value);
                Ok(ScanFlow::Stop)
            },
        );
        match scanned {
            Ok(()) | Err(TalaDbError::Encryption(_)) => {}
            Err(e) => return Err(e),
        }
        if ciphertext {
            return Err(TalaDbError::Encryption(format!(
                "this database's schema version record is not encrypted but the documents in \
                 `{table}` are: the damage an open without the passphrase causes in TalaDB \
                 0.13.1 and earlier. The documents are intact; see the 0.13.2 release notes to \
                 repair it. Nothing was changed"
            )));
        }
    }
    Ok(())
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

    // Refuse unreadable metadata before the first write — see `stored_state`.
    let rtxn = backend.begin_read()?;
    let mut current_version = match stored_state(rtxn.as_ref())? {
        StoredState::Empty => 0,
        StoredState::Readable(version) => {
            refuse_ciphertext_documents(rtxn.as_ref())?;
            version
        }
        StoredState::Unreadable(e) => return Err(e),
    };
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
        let mut arrays = 0u64;
        for (_, doc_bytes) in docs {
            let doc: Document = postcard::from_bytes(&doc_bytes)?;
            if let Some(val) = doc.get(&def.field) {
                arrays += u64::from(matches!(val, Value::Array(_)));
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
        txn.put(
            META_INDEX_ARRAYS_TABLE,
            meta_key(&def.collection, &def.field).as_bytes(),
            &arrays.to_le_bytes(),
        )?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_header_matches_plaintext_documents_only() {
        let doc = Document::new(vec![("a".into(), Value::Int(1))]);
        let plain = postcard::to_allocvec(&doc).unwrap();
        assert!(has_document_header(&plain));

        // Current ciphertext layout: format byte, then nonce/ciphertext/tag.
        let mut sealed = vec![0x01];
        sealed.extend(plain.iter().skip(1));
        assert!(!has_document_header(&sealed));
        // Too short, a lowercase ULID, and a letter Crockford base32 omits.
        assert!(!has_document_header(&plain[..DOCUMENT_HEADER_LEN - 1]));
        let mut lower = plain.clone();
        lower[1..DOCUMENT_HEADER_LEN].make_ascii_lowercase();
        assert!(!has_document_header(&lower));
        let mut omitted = plain;
        omitted[5] = b'U';
        assert!(!has_document_header(&omitted));
    }
}
