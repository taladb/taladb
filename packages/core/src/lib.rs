//! **TalaDB** — an embedded, local-first document database with built-in
//! vector search, for Rust apps. It runs inside your process and stores
//! everything in one file; the same engine powers TalaDB in the browser,
//! Node.js, React Native, Android and iOS.
//!
//! # Getting started
//!
//! ```
//! use serde::{Deserialize, Serialize};
//! use serde_json::json;
//!
//! #[derive(Serialize, Deserialize)]
//! struct Book {
//!     title: String,
//!     year: i64,
//! }
//!
//! # fn main() -> Result<(), taladb::TalaDbError> {
//! let db = taladb::Database::open_in_memory()?; // or taladb::open("app.db")
//! let books = db.typed::<Book>("books")?;
//!
//! books.insert(&Book { title: "Noli Me Tángere".into(), year: 1887 })?;
//! let found = books.find(json!({ "year": { "$gte": 1800 } }))?;
//! assert_eq!(found.len(), 1);
//! # Ok(())
//! # }
//! ```
//!
//! [`Database::typed`] gives a [`TypedCollection`] of your own serde types,
//! queried with the [JSON filter language](json) every TalaDB platform shares.
//! The untyped [`Collection`] underneath works with [`Value`]s and typed
//! [`Filter`]s directly.
//!
//! # What's in here
//!
//! - [`TypedCollection`] — serde-typed documents, JSON filters and updates, and
//!   typed live queries ([`TypedWatch`]).
//! - [`json`] — the JSON query language: documents, filters and updates.
//! - [`Database`] — open a database ([`Database::open`], [`Database::open_in_memory`],
//!   or [`Database::open_with_backend`] for a custom storage backend such as
//!   OPFS in the browser) and hand out [`Collection`]s.
//! - [`Collection`] — insert, query, update, and index documents.
//! - [`Filter`] and [`FindOptions`] — the query language: comparisons, logical
//!   combinators, sorting, pagination, and projection.
//! - [`vector`] — vector indexes and `find_nearest`, exact by default and
//!   persistent approximate (HNSW) on every platform.
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
//! Native Rust, the browser (via `wasm32-unknown-unknown`), Node.js, React
//! Native, Android and iOS all run this crate. Anything that would bloat a
//! WASM bundle is feature-gated — see the `config-yaml`, `encryption` and
//! `legacy-migration` features.

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
pub mod json;
pub mod json_depth;
pub mod migrate;
pub mod migration;
pub mod query;
pub mod time;
pub mod typed;
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
/// Re-exported because [`TalaDbError`]'s storage variants carry `redb` errors as
/// their [`source`](std::error::Error::source): downcasting to them requires
/// naming the same `redb` version the engine was built against.
pub use redb;
// Re-exported so a binding can implement redb 2's `StorageBackend` for its own
// storage and hand it to `migrate::migrate_backend_if_needed`, without having to
// depend on a second major of redb itself and risk the two versions drifting.
#[cfg(feature = "legacy-migration")]
pub use redb2;
pub use time::now_ms;
pub use typed::{Scored, TypedCollection, TypedWatch};
/// Re-exported so bindings can parse and construct document ids without taking a
/// direct `ulid` dependency (and risking a version skew against the engine's).
pub use ulid::Ulid;
pub use vector::{HnswOptions, VectorMetric, VectorSearchResult};

/// Re-exported so filters built with `taladb::serde_json::json!` always match
/// the `serde_json` version this crate's API takes.
pub use serde_json;

/// Open (or create) the database file at `path`. Shorthand for [`Database::open`].
pub fn open(path: impl AsRef<std::path::Path>) -> Result<Database, TalaDbError> {
    Database::open(path.as_ref())
}

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
    /// Shared retained-memory budget for exact vectors and graph nodes.
    search_cache: search_cache::SharedSearchCache,
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
        // The first read decrypts the schema metadata, so a wrong passphrase
        // fails here as an AEAD tag mismatch — reported by the cipher as just
        // "aead::Error". Say what it means; the code stays "Encryption".
        Self::open_with_backend(Box::new(crypto::EncryptedBackend::new(raw, key))).map_err(|e| {
            match e {
                TalaDbError::Encryption(detail) => TalaDbError::Encryption(format!(
                    "wrong passphrase, or the file was not encrypted with it ({detail})"
                )),
                other => other,
            }
        })
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
            search_cache: search_cache::new_shared_search_cache(),
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

    /// Set the shared decoded search-memory budget across retained caches,
    /// active graph loans, vector-block construction and ANN traversal scratch.
    /// Zero disables retention; a shared 64 KiB minimum workspace allows short
    /// ANN walks. Walks that cannot fit fall back to exact on the same snapshot.
    /// Existing loans finish under their original allowance after a reduction.
    /// Filters, results, single-record decoding, build scratch and storage-engine
    /// caches are separate; this is not a limit on process RAM.
    pub fn set_vector_cache_budget(&self, bytes: usize) {
        self.search_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_budget(bytes);
    }

    pub fn vector_cache_stats(&self) -> VectorCacheStats {
        self.search_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stats()
    }

    /// Use a host memory hint (bytes), with a conservative per-database cap.
    /// None restores the platform fallback. Reported pressure remains active
    /// until the host explicitly reports recovery. Zero is not a valid hint.
    pub fn set_vector_cache_adaptive(&self, memory_bytes: Option<u64>) -> Result<(), TalaDbError> {
        self.search_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_adaptive(memory_bytes)
    }

    /// Reduce retention and new search admissions under host memory pressure.
    /// Existing loans finish under their original allowance. Normal restores
    /// the configured baseline; critical disables retention, keeping the shared
    /// 64 KiB ANN workspace and same-snapshot exact fallback.
    pub fn notify_memory_pressure(&self, pressure: MemoryPressure) {
        self.search_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .notify_pressure(pressure);
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
            .with_search_cache(Arc::clone(&self.search_cache));
        Ok(col)
    }

    /// Rebuild configured HNSW indexes for maintenance or legacy migration.
    /// Persistent graphs need no startup warm-up. For large indexes use the
    /// collection batch-build API instead.
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
    /// database. Intended for explicit export/backup and the bounded IndexedDB
    /// fallback; OPFS-backed databases write pages directly.
    ///
    /// Format: `TDBS` magic (4 B) + version u32 LE (4 B) + table count u32 LE,
    /// then for each table: name length u32 LE, name bytes, entry count u64 LE,
    /// then for each entry: key length u32 LE, key bytes, value length u32 LE,
    /// value bytes.
    #[tracing::instrument(skip(self))]
    pub fn export_snapshot(&self) -> Result<Vec<u8>, TalaDbError> {
        self.export_snapshot_with_limit(usize::MAX)
    }

    /// Export one consistent snapshot while limiting output allocation.
    pub fn export_snapshot_with_limit(&self, max_bytes: usize) -> Result<Vec<u8>, TalaDbError> {
        if max_bytes < 12 {
            return Err(TalaDbError::InvalidSnapshot);
        }
        let txn = self.backend.begin_read()?;
        let table_names = txn.list_tables()?;

        let mut buf = Vec::new();
        buf.extend_from_slice(SNAPSHOT_MAGIC);
        buf.extend_from_slice(&SNAPSHOT_VERSION.to_le_bytes());

        // The table count is known from `list_tables` alone, so each table can
        // be scanned and written straight into `buf` instead of being collected
        // into an intermediate that holds the entire database alongside the
        // output. This is the API the bounded IndexedDB fallback calls, where
        // peak heap is the constraint.
        // Every length prefix is written through `len_prefix`, which fails
        // rather than saturating. A saturated prefix would claim `u32::MAX`
        // bytes while the real (shorter) bytes are still appended, so the
        // reader would run off into the next field — producing a snapshot that
        // restores as corrupt instead of one that refuses to be written.
        buf.extend_from_slice(&len_prefix(table_names.len())?);
        for name in &table_names {
            let name_bytes = name.as_bytes();
            if buf
                .len()
                .saturating_add(name_bytes.len())
                .saturating_add(12)
                > max_bytes
            {
                return Err(TalaDbError::Storage(
                    "snapshot exceeds configured byte limit".into(),
                ));
            }
            buf.extend_from_slice(&len_prefix(name_bytes.len())?);
            buf.extend_from_slice(name_bytes);
            buf.extend_from_slice(&txn.count_entries(name)?.to_le_bytes());
            txn.scan(
                name,
                std::ops::Bound::Unbounded,
                std::ops::Bound::Unbounded,
                &mut |key, val| {
                    if buf
                        .len()
                        .saturating_add(key.len())
                        .saturating_add(val.len())
                        .saturating_add(8)
                        > max_bytes
                    {
                        return Err(TalaDbError::Storage(
                            "snapshot exceeds configured byte limit".into(),
                        ));
                    }
                    buf.extend_from_slice(&len_prefix(key.len())?);
                    buf.extend_from_slice(key);
                    buf.extend_from_slice(&len_prefix(val.len())?);
                    buf.extend_from_slice(val);
                    Ok(crate::engine::ScanFlow::Continue)
                },
            )?;
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
        // A table requires a name prefix, a nonempty name and an entry count.
        if table_count > (data.len() - cursor) / 13 {
            return Err(TalaDbError::InvalidSnapshot);
        }
        for _ in 0..table_count {
            let name_len = read_u32(data, &mut cursor)? as usize;
            // redb documents "name must not be empty" as an invariant of
            // `TableDefinition::new` and enforces it with an `assert!`, so an
            // empty name reaches it as a *panic* — inside a function whose entire
            // contract is to return `Err(InvalidSnapshot)` for bad input, and
            // which exists to be pointed at bytes from disk or the network. The
            // export side never writes one, so this only ever means a corrupt or
            // crafted snapshot.
            //
            // No upper bound is needed alongside it: `read_slice` bounds `len`
            // against the buffer and returns a borrowed slice, so an absurd
            // `name_len` already fails without allocating. A length cap here
            // could only start rejecting valid snapshots — internal table names
            // are built from collection and field names and have no fixed
            // ceiling of their own.
            if name_len == 0 {
                return Err(TalaDbError::InvalidSnapshot);
            }
            let name = std::str::from_utf8(read_slice(data, &mut cursor, name_len)?)
                .map_err(|_| TalaDbError::InvalidSnapshot)?;

            let entry_count = usize::try_from(read_u64(data, &mut cursor)?)
                .map_err(|_| TalaDbError::InvalidSnapshot)?;
            // Even an empty key/value pair consumes two length prefixes.
            if entry_count > (data.len() - cursor) / 8 {
                return Err(TalaDbError::InvalidSnapshot);
            }

            // Write all entries for this table in a single transaction.
            let mut wtxn = db.backend.begin_write()?;
            for _ in 0..entry_count {
                let key_len = read_u32(data, &mut cursor)? as usize;
                if key_len > MAX_SNAPSHOT_ENTRY_BYTES {
                    return Err(TalaDbError::InvalidSnapshot);
                }
                let key = read_slice(data, &mut cursor, key_len)?;
                let val_len = read_u32(data, &mut cursor)? as usize;
                if val_len > MAX_SNAPSHOT_ENTRY_BYTES {
                    return Err(TalaDbError::InvalidSnapshot);
                }
                let val = read_slice(data, &mut cursor, val_len)?;
                wtxn.put(name, key, val)?;
            }
            wtxn.commit()?;
        }

        if cursor != data.len() {
            return Err(TalaDbError::InvalidSnapshot);
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

mod search_cache;
mod vector_graph;
pub use collection::{
    VectorBuildProgress, VectorExecution, VectorIndexStatus, VectorQueryOptions, VectorQueryResult,
    VectorSearchMode,
};
pub use search_cache::{MemoryPressure, VectorCachePolicy, VectorCacheStats};
pub use vector_graph::{GraphOptions, Quantization};

// Compiles the README's examples as doctests, so the crates.io page cannot
// drift from the API it describes.
#[doc = include_str!("../README.md")]
#[cfg(doctest)]
pub struct ReadmeDoctests;
