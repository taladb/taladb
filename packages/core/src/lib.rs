//! **TalaDB core** — an embedded, local-first document database with built-in
//! vector search, designed to run on the device rather than behind a network
//! hop.
//!
//! This crate is the engine. Applications normally reach it through a language
//! binding — `taladb` / `@taladb/web` / `@taladb/react` on npm — but it is
//! usable directly from Rust, which is what the examples below do.
//!
//! # Getting started
//!
//! ```no_run
//! use taladb_core::{Database, Filter, Value};
//!
//! # fn main() -> Result<(), taladb_core::TalaDbError> {
//! let db = Database::open_in_memory()?;
//! let books = db.collection("books")?;
//!
//! books.insert(vec![
//!     ("title".into(), Value::Str("Noli Me Tángere".into())),
//!     ("year".into(), Value::Int(1887)),
//! ])?;
//!
//! let found = books.find(Filter::Gte("year".into(), Value::Int(1800)))?;
//! assert_eq!(found.len(), 1);
//! # Ok(())
//! # }
//! ```
//!
//! # What's in here
//!
//! - [`Database`] — open a database ([`Database::open`], [`Database::open_in_memory`],
//!   or [`Database::open_with_backend`] for a custom storage backend such as
//!   OPFS in the browser) and hand out [`Collection`]s.
//! - [`Collection`] — insert, query, update, and index documents.
//! - [`Filter`] and [`FindOptions`] — the query language: comparisons, logical
//!   combinators, sorting, pagination, and projection.
//! - [`vector`] — vector indexes and `find_nearest`, exact by default and
//!   approximate (HNSW) behind the `vector-hnsw` feature.
//! - [`bm25`] / [`fts`] — full-text search over the same documents.
//! - [`TalaDbError`] — one error type for everything, carrying the originating
//!   storage error as a [`source`](std::error::Error::source).
//!
//! # Scope
//!
//! TalaDB is an *embedded* database: it owns on-device storage and retrieval,
//! and deliberately does not replicate. Applications that need to notify a
//! backend of local writes use the change webhook in the `taladb` TypeScript
//! client, which works identically on every runtime.
//!
//! # Runtime targets
//!
//! The browser (via `wasm32-unknown-unknown`) and React Native are the primary
//! targets; native and Node are supported but are not what the design optimises
//! for. Anything that would bloat a WASM bundle is feature-gated — see the
//! `config-yaml`, `encryption`, and `vector-hnsw` features.

pub mod aggregate;
pub mod audit;
pub mod bm25;
pub mod collection;
pub mod config;
pub mod crypto;
pub mod document;
pub mod engine;
pub mod error;
pub mod fts;
pub mod index;
pub mod migration;
pub mod query;
pub mod time;
pub mod vector;
pub mod watch;

pub use aggregate::{Accumulator, GroupKey, Pipeline, Stage};
pub use audit::{AuditEntry, AuditOp, read_audit_log, read_audit_log_since};
pub use collection::{Collection, CollectionIndexInfo, Update};
pub use config::{DurabilityConfig, TalaDbConfig, load_auto, load_from_path};
#[cfg(feature = "encryption")]
pub use crypto::{
    EncryptedBackend, EncryptionKey, MIN_PBKDF2_ITERATIONS, derive_key, migrate_encrypted_v0_to_v1,
    rekey,
};
pub use document::{Document, Value, derive_doc_id};
pub use engine::{RedbBackend, StorageBackend};
pub use error::TalaDbError;
pub use migration::{BUILTIN_MIGRATIONS, CURRENT_SCHEMA_VERSION, Migration, run_migrations};
pub use query::Filter;
pub use query::options::{FindOptions, SortDirection, SortSpec};
pub use time::now_ms;
/// Re-exported because [`TalaDbError`]'s storage variants carry `redb` errors as
/// their [`source`](std::error::Error::source): downcasting to them requires
/// naming the same `redb` version the engine was built against.
pub use redb;
/// Re-exported so bindings can parse and construct document ids without taking a
/// direct `ulid` dependency (and risking a version skew against the engine's).
pub use ulid::Ulid;
pub use vector::{HnswOptions, VectorMetric, VectorSearchResult};

use std::path::Path;
use std::sync::Arc;

/// Narrow a `u64` count to `usize`, saturating instead of wrapping.
///
/// `skip`, `limit`, and stored entry counts are `u64` in the public API but
/// index into `Vec`s, and `usize` is 32 bits on `wasm32` — the primary target.
/// A plain `as usize` there wraps: `skip = 2^32 + 5` silently becomes `5` and
/// returns the *first* page instead of an empty one. Saturating turns an absurd
/// value into "past the end", which every caller already handles.
pub(crate) fn clamp_to_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

// Snapshot magic + version bytes written at the start of every snapshot.
const SNAPSHOT_MAGIC: &[u8; 4] = b"TDBS";
const SNAPSHOT_VERSION: u32 = 1;

/// Per-entry size cap for snapshot deserialization.
///
/// A crafted snapshot with a `u32::MAX`-length key or value would cause a
/// 4 GiB allocation before the total-size check has a chance to catch it.
/// This cap prevents that while being far larger than any real document.
const MAX_SNAPSHOT_ENTRY_BYTES: usize = 64 * 1024 * 1024; // 64 MiB

/// The main TalaDB database handle.
#[derive(Clone)]
pub struct Database {
    backend: Arc<dyn StorageBackend>,
    /// Index-definition cache shared by every Collection handle from this
    /// Database, so index DDL through one handle is visible to all others.
    index_cache: collection::SharedIndexCache,
    /// One watch registry per collection name, shared across handles so a
    /// watcher created through one handle observes writes made through any
    /// other handle of the same collection.
    watch_registries:
        Arc<std::sync::Mutex<std::collections::HashMap<String, watch::SharedRegistry>>>,
    /// Decoded-vector cache for flat search, shared by every Collection handle
    /// from this Database (keyed by `collection::field`).
    vector_cache: vector::SharedVectorCache,
    #[cfg(feature = "vector-hnsw")]
    hnsw_cache: vector::SharedHnswCache,
}

impl Database {
    /// Open a file-backed database with transparent authenticated encryption.
    #[cfg(feature = "encryption")]
    pub fn open_encrypted(path: &Path, passphrase: &str) -> Result<Self, TalaDbError> {
        if passphrase.is_empty() {
            return Err(TalaDbError::Config(
                "encryption passphrase must not be empty".into(),
            ));
        }
        let salt_path = path.with_extension(format!(
            "{}taladb-salt",
            path.extension()
                .and_then(|v| v.to_str())
                .map(|v| format!("{v}."))
                .unwrap_or_default()
        ));
        let salt = match std::fs::read(&salt_path) {
            Ok(salt) if salt.len() == 16 => salt,
            Ok(_) => {
                return Err(TalaDbError::Encryption(
                    "invalid encryption salt file".into(),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut salt = vec![0u8; 16];
                getrandom::fill(&mut salt).map_err(|e| TalaDbError::Encryption(e.to_string()))?;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                use std::io::Write;
                options
                    .open(&salt_path)
                    .and_then(|mut file| file.write_all(&salt))
                    .map_err(|e| {
                        TalaDbError::Storage(format!("cannot write encryption salt: {e}"))
                    })?;
                salt
            }
            Err(e) => {
                return Err(TalaDbError::Storage(format!(
                    "cannot read encryption salt: {e}"
                )));
            }
        };
        let raw: Arc<dyn StorageBackend> = Arc::new(RedbBackend::open(path)?);
        let key = crypto::derive_key(passphrase, &salt, crypto::MIN_PBKDF2_ITERATIONS)?;
        Self::open_with_backend(Box::new(crypto::EncryptedBackend::new(raw, key)))
    }
    /// Build a `Database` around an already-constructed backend, running the
    /// built-in migration chain first.
    ///
    /// Every public constructor funnels through here so the cache set is
    /// declared exactly once — adding a cache field is otherwise a four-site
    /// edit that compiles fine if you miss one.
    fn from_backend(backend: Arc<dyn StorageBackend>) -> Result<Self, TalaDbError> {
        run_migrations(backend.as_ref(), BUILTIN_MIGRATIONS)?;
        Ok(Self {
            backend,
            index_cache: collection::new_shared_index_cache(),
            watch_registries: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            vector_cache: vector::new_shared_vector_cache(),
            #[cfg(feature = "vector-hnsw")]
            hnsw_cache: vector::new_shared_cache(),
        })
    }

    /// Open a file-backed database at the given path.
    pub fn open(path: &Path) -> Result<Self, TalaDbError> {
        Self::from_backend(Arc::new(RedbBackend::open(path)?))
    }

    /// Open a database with a custom storage backend (e.g. OPFS in WASM).
    pub fn open_with_backend(backend: Box<dyn StorageBackend>) -> Result<Self, TalaDbError> {
        Self::from_backend(Arc::from(backend))
    }

    /// Open an in-memory database (useful for tests).
    pub fn open_in_memory() -> Result<Self, TalaDbError> {
        Self::from_backend(Arc::new(RedbBackend::open_in_memory()?))
    }

    /// Open a database and run any pending migrations before returning.
    ///
    /// Built-in migrations run first, then the caller-supplied list.  User
    /// migrations must start at [`CURRENT_SCHEMA_VERSION`] so they pick up
    /// where the built-in chain ends.
    pub fn open_with_migrations(
        path: &Path,
        migrations: &[Migration],
    ) -> Result<Self, TalaDbError> {
        let db = Self::from_backend(Arc::new(RedbBackend::open(path)?))?;
        run_migrations(db.backend.as_ref(), migrations)?;
        Ok(db)
    }

    /// Access the raw storage backend.
    ///
    /// Useful for calling lower-level APIs such as [`read_audit_log`] and
    /// [`rekey`] that operate directly on the backend rather than through a
    /// `Collection` handle.
    pub fn backend(&self) -> &dyn StorageBackend {
        self.backend.as_ref()
    }

    /// Set write durability. `eventual = false` (default) fsyncs every commit;
    /// `true` batches commits (higher write throughput) and requires
    /// [`flush`](Self::flush) to force a durable sync. Maps to a
    /// [`DurabilityConfig`]'s `flush_every_write` (`eventual = !flush_every_write`).
    pub fn set_durability(&self, eventual: bool) {
        self.backend.set_durability(eventual);
    }

    /// Force any batched (eventual) writes to durable storage. A no-op when
    /// committing immediately or on in-memory backends.
    pub fn flush(&self) -> Result<(), TalaDbError> {
        self.backend.flush()
    }

    /// Get a collection handle by name.
    ///
    /// # Errors
    /// Returns [`TalaDbError::InvalidName`] if `name` is empty, longer than 128
    /// characters, or contains the `"::"` separator reserved for internal table
    /// naming.
    pub fn collection(&self, name: &str) -> Result<Collection, TalaDbError> {
        collection::validate_collection_name(name)?;
        let registry = {
            let mut map = self
                .watch_registries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(
                map.entry(name.to_string())
                    .or_insert_with(watch::new_registry),
            )
        };
        let col = Collection::new(name, Arc::clone(&self.backend))
            .with_index_cache(Arc::clone(&self.index_cache))
            .with_watch_registry(registry)
            .with_vector_cache(Arc::clone(&self.vector_cache));
        #[cfg(feature = "vector-hnsw")]
        let col = col.with_hnsw_cache(Arc::clone(&self.hnsw_cache));
        Ok(col)
    }

    /// Warm the in-memory HNSW cache by rebuilding all graphs whose options are
    /// stored in [`META_HNSW_TABLE`].  Call this once after `Database::open*`
    /// if you want approximate-nearest-neighbor search to be available
    /// immediately without waiting for the first `upgrade_vector_index` call.
    ///
    /// No-op when the `vector-hnsw` feature is disabled.
    #[cfg(feature = "vector-hnsw")]
    pub fn rebuild_hnsw_indexes(&self) -> Result<(), TalaDbError> {
        let txn = self.backend.begin_read()?;
        let all = txn.scan_all(vector::META_HNSW_TABLE)?;
        drop(txn);

        for (k, _) in all {
            let key_str = match std::str::from_utf8(&k) {
                Ok(s) => s.to_string(),
                Err(_) => continue,
            };
            let mut parts = key_str.splitn(2, "::");
            let col_name = match parts.next() {
                Some(s) => s.to_string(),
                None => continue,
            };
            let field = match parts.next() {
                Some(s) => s.to_string(),
                None => continue,
            };
            self.collection(&col_name)?.upgrade_vector_index(&field)?;
        }
        Ok(())
    }

    /// Compact the underlying storage file, reclaiming space freed by deletes
    /// and updates. Useful after bulk deletes.
    ///
    /// This is a blocking operation proportional to database size; call it
    /// during idle periods (e.g. at startup).
    ///
    /// No-op on in-memory backends.
    pub fn compact(&self) -> Result<(), TalaDbError> {
        self.backend.compact()
    }

    /// Return the names of all collections stored in this database.
    ///
    /// Derived by scanning table names for the `docs::` prefix used by the
    /// document storage layer. System collections (names starting with `_`,
    /// e.g. the `_audit` log) are excluded — they are not addressable via
    /// [`Database::collection`] and have their own read APIs.
    pub fn list_collection_names(&self) -> Result<Vec<String>, TalaDbError> {
        let txn = self.backend.begin_read()?;
        let mut names: Vec<String> = txn
            .list_tables()?
            .into_iter()
            .filter_map(|t| t.strip_prefix("docs::").map(str::to_string))
            .filter(|name| !name.starts_with('_'))
            .collect();
        names.sort();
        Ok(names)
    }

    /// Read the current **application** migration version (0 if never set).
    ///
    /// This is the version advanced by user-defined `openDB({ migrations })`
    /// runners, kept separate from the engine storage-schema version so the two
    /// never collide. See [`set_user_version`](Self::set_user_version).
    pub fn user_version(&self) -> Result<u32, TalaDbError> {
        let rtxn = self.backend.begin_read()?;
        match rtxn.get(
            crate::index::META_USER_VERSION_TABLE,
            crate::index::META_VERSION_KEY,
        )? {
            Some(bytes) => Ok(postcard::from_bytes(&bytes)?),
            None => Ok(0),
        }
    }

    /// Persist the application migration version. A migration runner calls this
    /// after each migration's body succeeds, so a crash mid-run leaves the
    /// counter at the last fully-applied migration (checkpoint-per-version).
    pub fn set_user_version(&self, version: u32) -> Result<(), TalaDbError> {
        let bytes = postcard::to_allocvec(&version)?;
        let mut wtxn = self.backend.begin_write()?;
        wtxn.put(
            crate::index::META_USER_VERSION_TABLE,
            crate::index::META_VERSION_KEY,
            &bytes,
        )?;
        wtxn.commit()?;
        Ok(())
    }

    /// Serialize the entire database state to a compact binary snapshot.
    ///
    /// The snapshot can be stored anywhere (e.g. OPFS) and later passed to
    /// [`Database::restore_from_snapshot`] to recreate an identical in-memory
    /// database.  Intended for the browser WASM snapshot-flush persistence
    /// strategy; not suited for databases larger than ~50 MB.
    ///
    /// Format: `TDBS` magic (4 B) + version u32 LE (4 B) + table count u32 LE,
    /// then for each table: name length u32 LE, name bytes, entry count u64 LE,
    /// then for each entry: key length u32 LE, key bytes, value length u32 LE,
    /// value bytes.
    #[tracing::instrument(skip(self))]
    pub fn export_snapshot(&self) -> Result<Vec<u8>, TalaDbError> {
        let txn = self.backend.begin_read()?;
        let table_names = txn.list_tables()?;

        let mut buf = Vec::new();
        buf.extend_from_slice(SNAPSHOT_MAGIC);
        buf.extend_from_slice(&SNAPSHOT_VERSION.to_le_bytes());

        // The table count is known from `list_tables` alone, so each table can
        // be scanned and written straight into `buf` instead of being collected
        // into an intermediate that holds the entire database alongside the
        // output. Only one table is resident at a time now — this is the API
        // the WASM persistence path calls, where peak heap is the constraint.
        // Every length prefix is written through `len_prefix`, which fails
        // rather than saturating. A saturated prefix would claim `u32::MAX`
        // bytes while the real (shorter) bytes are still appended, so the
        // reader would run off into the next field — producing a snapshot that
        // restores as corrupt instead of one that refuses to be written.
        buf.extend_from_slice(&len_prefix(table_names.len())?);
        for name in &table_names {
            let pairs = txn.scan_all(name)?;
            let name_bytes = name.as_bytes();

            // One growth up front for the whole table rather than one per
            // `extend_from_slice`: 4 B key length + 4 B value length per entry,
            // plus the table header.
            let table_bytes: usize = pairs
                .iter()
                .map(|(key, val)| key.len() + val.len() + 8)
                .sum();
            buf.reserve(table_bytes + name_bytes.len() + 12);

            buf.extend_from_slice(&len_prefix(name_bytes.len())?);
            buf.extend_from_slice(name_bytes);
            buf.extend_from_slice(&(pairs.len() as u64).to_le_bytes());
            for (key, val) in &pairs {
                buf.extend_from_slice(&len_prefix(key.len())?);
                buf.extend_from_slice(key);
                buf.extend_from_slice(&len_prefix(val.len())?);
                buf.extend_from_slice(val);
            }
        }
        Ok(buf)
    }

    /// Restore a database from a snapshot produced by [`Database::export_snapshot`].
    ///
    /// Returns an in-memory database pre-loaded with all data from the snapshot.
    /// Returns [`TalaDbError::InvalidSnapshot`] if the data is corrupt or from an
    /// incompatible snapshot version.
    pub fn restore_from_snapshot(data: &[u8]) -> Result<Self, TalaDbError> {
        /// Hard cap — prevents OOM from corrupted or crafted snapshots.
        /// 2 GiB on 32-bit targets (WASM), 10 GiB on 64-bit targets.
        #[cfg(target_pointer_width = "32")]
        const MAX_SNAPSHOT_SIZE: usize = 2 * 1024 * 1024 * 1024; // 2 GiB fits in u32
        #[cfg(not(target_pointer_width = "32"))]
        const MAX_SNAPSHOT_SIZE: usize = 10 * 1024 * 1024 * 1024; // 10 GiB
        if data.len() > MAX_SNAPSHOT_SIZE {
            return Err(TalaDbError::InvalidSnapshot);
        }
        if data.len() < 12 || &data[..4] != SNAPSHOT_MAGIC {
            return Err(TalaDbError::InvalidSnapshot);
        }
        let version = u32::from_le_bytes(
            data[4..8]
                .try_into()
                .map_err(|_| TalaDbError::InvalidSnapshot)?,
        );
        if version != SNAPSHOT_VERSION {
            return Err(TalaDbError::InvalidSnapshot);
        }

        let db = Self::open_in_memory()?;
        let mut cursor: usize = 8;

        let table_count = read_u32(data, &mut cursor)? as usize;
        for _ in 0..table_count {
            let name_len = read_u32(data, &mut cursor)? as usize;
            let name = std::str::from_utf8(read_slice(data, &mut cursor, name_len)?)
                .map_err(|_| TalaDbError::InvalidSnapshot)?
                .to_string();

            let entry_count = clamp_to_usize(read_u64(data, &mut cursor)?);

            // Write all entries for this table in a single transaction.
            let mut wtxn = db.backend.begin_write()?;
            for _ in 0..entry_count {
                let key_len = read_u32(data, &mut cursor)? as usize;
                if key_len > MAX_SNAPSHOT_ENTRY_BYTES {
                    return Err(TalaDbError::InvalidSnapshot);
                }
                let key = read_slice(data, &mut cursor, key_len)?.to_vec();
                let val_len = read_u32(data, &mut cursor)? as usize;
                if val_len > MAX_SNAPSHOT_ENTRY_BYTES {
                    return Err(TalaDbError::InvalidSnapshot);
                }
                let val = read_slice(data, &mut cursor, val_len)?.to_vec();
                wtxn.put(&name, &key, &val)?;
            }
            wtxn.commit()?;
        }

        Ok(db)
    }
}

// ---------------------------------------------------------------------------
// Snapshot binary helpers
// ---------------------------------------------------------------------------

/// Encode a length as the little-endian `u32` prefix the snapshot format uses,
/// refusing anything that does not fit.
///
/// The write side must never emit a prefix that disagrees with the bytes that
/// follow it: the reader trusts the prefix to find the *next* field, so a
/// wrong one desynchronises the whole remaining stream rather than corrupting
/// one entry. Failing here keeps `restore_from_snapshot`'s strict bounds
/// checking meaningful — the two sides agree on what is representable.
fn len_prefix(len: usize) -> Result<[u8; 4], TalaDbError> {
    u32::try_from(len)
        .map(u32::to_le_bytes)
        .map_err(|_| TalaDbError::InvalidSnapshot)
}

fn read_u32(data: &[u8], cursor: &mut usize) -> Result<u32, TalaDbError> {
    let end = cursor.checked_add(4).ok_or(TalaDbError::InvalidSnapshot)?;
    if end > data.len() {
        return Err(TalaDbError::InvalidSnapshot);
    }
    let val = u32::from_le_bytes(
        data[*cursor..end]
            .try_into()
            .map_err(|_| TalaDbError::InvalidSnapshot)?,
    );
    *cursor = end;
    Ok(val)
}

fn read_u64(data: &[u8], cursor: &mut usize) -> Result<u64, TalaDbError> {
    let end = cursor.checked_add(8).ok_or(TalaDbError::InvalidSnapshot)?;
    if end > data.len() {
        return Err(TalaDbError::InvalidSnapshot);
    }
    let val = u64::from_le_bytes(
        data[*cursor..end]
            .try_into()
            .map_err(|_| TalaDbError::InvalidSnapshot)?,
    );
    *cursor = end;
    Ok(val)
}

fn read_slice<'a>(data: &'a [u8], cursor: &mut usize, len: usize) -> Result<&'a [u8], TalaDbError> {
    let end = cursor
        .checked_add(len)
        .ok_or(TalaDbError::InvalidSnapshot)?;
    if end > data.len() {
        return Err(TalaDbError::InvalidSnapshot);
    }
    let slice = &data[*cursor..end];
    *cursor = end;
    Ok(slice)
}

#[cfg(test)]
mod snapshot_encoding_tests {
    use super::*;

    /// The write side must refuse a length it cannot encode rather than
    /// saturating. A saturated prefix claims `u32::MAX` bytes while the real
    /// (shorter) bytes follow, so the reader walks off into the next field and
    /// the whole remaining stream desynchronises — a snapshot that restores as
    /// corrupt rather than one that refuses to be written.
    #[test]
    fn len_prefix_round_trips_and_rejects_the_unrepresentable() {
        assert_eq!(len_prefix(0).unwrap(), 0u32.to_le_bytes());
        assert_eq!(len_prefix(16).unwrap(), 16u32.to_le_bytes());
        assert_eq!(
            len_prefix(u32::MAX as usize).unwrap(),
            u32::MAX.to_le_bytes()
        );

        // Only reachable on 64-bit targets; on wasm32 `usize` is 32 bits and
        // every value is representable, which is itself the correct outcome.
        #[cfg(not(target_pointer_width = "32"))]
        {
            let too_big = u32::MAX as usize + 1;
            assert!(
                matches!(len_prefix(too_big), Err(TalaDbError::InvalidSnapshot)),
                "an unencodable length must be an error, not a saturated prefix"
            );
        }
    }

    /// A prefix written by `len_prefix` is exactly what `read_u32` expects.
    #[test]
    fn len_prefix_is_readable_by_the_snapshot_reader() {
        let encoded = len_prefix(1234).unwrap();
        let mut cursor = 0usize;
        assert_eq!(read_u32(&encoded, &mut cursor).unwrap(), 1234);
        assert_eq!(cursor, 4);
    }
}
