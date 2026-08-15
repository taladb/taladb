use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ulid::Ulid;

use crate::clamp_to_usize;
use crate::aggregate::{Stage, execute_pipeline};
use crate::audit::{AuditOp, write_audit_entry};
use crate::bm25::{Bm25Params, FtsStats};
use crate::document::{Document, Value};
use crate::engine::StorageBackend;
use crate::error::TalaDbError;
use crate::fts::{
    FTS_STATS_KEY, FTS_VERSION, FtsDef, HybridQuery, HybridSearchResult, TextSearchResult,
    decode_doc_len, decode_fts_def, encode_doc_len, encode_fts_key, encode_tf, fts_len_table_name,
    fts_stats_table_name, fts_table_name, token_frequencies, tokenize,
};
use crate::index::{
    CompoundIndexDef, IndexDef, META_COMPOUND_TABLE, META_INDEXES_TABLE, compound_meta_key,
    compound_table_name, docs_table_name, encode_compound_keys, encode_index_keys,
    index_table_name, meta_key,
};
use crate::query::executor::{execute, fetch_documents, index_ordered_entries};
use crate::query::filter::Filter;
use crate::query::options::{
    FindOptions, SortDirection, partial_sort_documents, project_document, sort_documents,
};
use crate::query::planner::plan_full;
use crate::time::now_ms;
use crate::vector::{
    CachedVectors, HnswOptions, META_HNSW_TABLE, META_VECTOR_TABLE, SharedVectorCache, VectorDef,
    VectorMetric, VectorSearchResult, decode_f32_vec, encode_f32_vec,
    value_to_f32_vec, vec_meta_key, vec_table_name,
};
#[cfg(feature = "vector-hnsw")]
use crate::vector::{SharedHnswCache, build_hnsw, search_hnsw};

const META_FTS_TABLE: &str = "meta::fts_indexes";

#[derive(Clone)]
pub(crate) struct CachedIndexes {
    indexes: Arc<Vec<IndexDef>>,
    fts_indexes: Arc<Vec<FtsDef>>,
    vec_indexes: Arc<Vec<VectorDef>>,
    compound_indexes: Arc<Vec<CompoundIndexDef>>,
    /// Storage table names for every index above, resolved once when the cache
    /// is built.
    ///
    /// These are pure functions of `(collection, field)` — constant for the
    /// life of the cache — but the write path used to `format!` each one *per
    /// document per index*, which on a bulk insert is N×M throwaway
    /// allocations for values that never change.
    tables: Arc<IndexTables>,
}

/// Precomputed storage table names for one collection's indexes.
pub(crate) struct IndexTables {
    docs: String,
    /// One per entry of `CachedIndexes::indexes`, same order.
    btree: Vec<String>,
    /// `(postings, lengths, stats)` per entry of `fts_indexes`, same order.
    fts: Vec<(String, String, String)>,
    /// One per entry of `vec_indexes`, same order.
    vectors: Vec<String>,
    /// One per entry of `compound_indexes`, same order.
    compound: Vec<String>,
}

impl IndexTables {
    fn build(
        collection: &str,
        indexes: &[IndexDef],
        fts_indexes: &[FtsDef],
        vec_indexes: &[VectorDef],
        compound_indexes: &[CompoundIndexDef],
    ) -> Self {
        Self {
            docs: docs_table_name(collection),
            btree: indexes
                .iter()
                .map(|d| index_table_name(collection, &d.field))
                .collect(),
            fts: fts_indexes
                .iter()
                .map(|d| {
                    (
                        fts_table_name(collection, &d.field),
                        fts_len_table_name(collection, &d.field),
                        fts_stats_table_name(collection, &d.field),
                    )
                })
                .collect(),
            vectors: vec_indexes
                .iter()
                .map(|d| vec_table_name(collection, &d.field))
                .collect(),
            compound: compound_indexes
                .iter()
                .map(|d| {
                    let refs: Vec<&str> = d.fields.iter().map(std::string::String::as_str).collect();
                    compound_table_name(collection, &refs)
                })
                .collect(),
        }
    }
}

/// Index-definition cache shared between every `Collection` handle returned
/// by the same `Database`, keyed by collection name.
///
/// Sharing matters for correctness, not just speed: with a per-handle cache,
/// creating an index through one handle would leave every other live handle
/// with stale definitions, and their writes would silently skip maintaining
/// the new index. (Handles from *different* `Database` instances on the same
/// file still don't see each other's index DDL until reopened.)
pub(crate) type SharedIndexCache = Arc<Mutex<std::collections::HashMap<String, CachedIndexes>>>;

pub(crate) fn new_shared_index_cache() -> SharedIndexCache {
    Arc::new(Mutex::new(std::collections::HashMap::new()))
}

/// An update operation on a document.
#[derive(Debug, Clone)]
pub struct CollectionIndexInfo {
    pub btree: Vec<String>,
    pub fts: Vec<String>,
    pub vector: Vec<String>,
}

#[derive(Clone)]
pub enum Update {
    /// Apply multiple update operators atomically, in document order.
    Many(Vec<Self>),
    /// $set — set or replace field values
    Set(Vec<(String, Value)>),
    /// $unset — remove fields
    Unset(Vec<String>),
    /// $inc — increment numeric fields
    Inc(Vec<(String, Value)>),
    /// $push — append a value to an array field
    Push(String, Value),
    /// $pull — remove a value from an array field
    Pull(String, Value),
}

pub struct Collection {
    pub(crate) name: String,
    backend: Arc<dyn StorageBackend>,
    /// Shared with every handle from the same `Database` so index DDL
    /// performed through one handle is visible to all others.
    index_cache: SharedIndexCache,
    /// Live-query subscribers for this collection; notified after every
    /// successful write commit. Shared per collection name across all
    /// handles from the same `Database`.
    watch_registry: crate::watch::SharedRegistry,
    /// If `Some`, every successful mutation appends an entry to the `_audit`
    /// table. The string is the caller identity recorded in each entry.
    audit_caller: Option<String>,
    /// If `Some`, the listed field values are individually encrypted at rest
    /// using the provided key.  Only the named fields are encrypted; all other
    /// fields remain in plaintext and are fully indexable.
    #[cfg(feature = "encryption")]
    field_encryption: Option<crate::crypto::FieldEncryptionConfig>,
    /// In-memory decoded vectors for the flat search path, shared across all
    /// handles from the same `Database` and invalidated by the collection's
    /// write generation (see [`crate::watch`]). Always present — the flat path
    /// is the default and the only vector path on web/React Native.
    vector_cache: SharedVectorCache,
    #[cfg(feature = "vector-hnsw")]
    hnsw_cache: SharedHnswCache,
}

impl Collection {
    pub fn new(name: impl Into<String>, backend: Arc<dyn StorageBackend>) -> Self {
        Self {
            name: name.into(),
            backend,
            index_cache: new_shared_index_cache(),
            watch_registry: crate::watch::new_registry(),
            audit_caller: None,
            #[cfg(feature = "encryption")]
            field_encryption: None,
            vector_cache: crate::vector::new_shared_vector_cache(),
            #[cfg(feature = "vector-hnsw")]
            hnsw_cache: crate::vector::new_shared_cache(),
        }
    }

    /// Attach a shared index-definition cache (called by `Database::collection()`
    /// so all handles from the same `Database` observe each other's index DDL).
    pub(crate) fn with_index_cache(mut self, cache: SharedIndexCache) -> Self {
        self.index_cache = cache;
        self
    }

    /// Attach a shared watch registry (called by `Database::collection()` so
    /// watchers created through one handle see writes made through any other
    /// handle of the same collection).
    pub(crate) fn with_watch_registry(mut self, registry: crate::watch::SharedRegistry) -> Self {
        self.watch_registry = registry;
        self
    }

    /// This collection's write generation — a counter bumped once per committed
    /// mutation, through any handle of the same `Database`.
    ///
    /// Lets a caller that cannot hold a [`crate::watch::WatchHandle`] (the
    /// worker/JS boundary, say) detect "has anything changed?" with an integer
    /// comparison, instead of re-running the query and diffing its output.
    pub fn write_generation(&self) -> u64 {
        crate::watch::generation(&self.watch_registry)
    }

    /// Subscribe to live query results.
    ///
    /// Returns a [`crate::watch::WatchHandle`] that yields a fresh snapshot of
    /// the documents matching `filter` after every write to this collection
    /// (insert, update, delete — through any handle of the same `Database`).
    /// Rapid writes may coalesce into a single snapshot; no write is ever
    /// silently skipped because the query re-runs at receive time.
    pub fn watch(&self, filter: Filter) -> crate::watch::WatchHandle {
        let reader = self.clone_reader();
        crate::watch::create_watch(&self.watch_registry, filter, move |f| {
            reader.find(f.clone())
        })
    }

    /// A read-only clone of this handle for watch callbacks: shares the
    /// backend and caches, carries the field-encryption config so snapshots
    /// are decrypted like `find`, but drops audit (it never writes).
    fn clone_reader(&self) -> Self {
        Self {
            name: self.name.clone(),
            backend: Arc::clone(&self.backend),
            index_cache: Arc::clone(&self.index_cache),
            watch_registry: Arc::clone(&self.watch_registry),
            audit_caller: None,
            #[cfg(feature = "encryption")]
            field_encryption: self.field_encryption.clone(),
            vector_cache: Arc::clone(&self.vector_cache),
            #[cfg(feature = "vector-hnsw")]
            hnsw_cache: Arc::clone(&self.hnsw_cache),
        }
    }

    /// Enable the append-only audit log for this collection handle.
    ///
    /// After each successful mutation (`insert`, `insert_many`, `update_one`,
    /// `update_many`, `delete_one`, `delete_many`) an entry is written to the
    /// `_audit` table recording the collection name, operation type, document
    /// ID, wall-clock timestamp, and the `caller` identity string supplied here.
    ///
    /// Read the log with [`crate::read_audit_log`].
    pub fn with_audit_log(mut self, caller: String) -> Self {
        self.audit_caller = Some(caller);
        self
    }

    /// Enable field-level encryption for this collection handle.
    ///
    /// Values stored in any of the listed `fields` will be individually
    /// encrypted with AES-GCM-256 using `key` before storage, and decrypted
    /// transparently on read.  All other fields remain in plaintext and are
    /// fully indexable.
    ///
    /// **Note:** Encrypted fields cannot be indexed or queried by value — the
    /// stored bytes are opaque ciphertext.  Index and filter on plaintext fields
    /// only.
    ///
    /// **Requires** the `encryption` feature flag.
    #[cfg(feature = "encryption")]
    pub fn with_field_encryption(
        mut self,
        fields: Vec<String>,
        key: crate::crypto::EncryptionKey,
    ) -> Self {
        let mut sorted = fields;
        sorted.sort();
        self.field_encryption = Some(crate::crypto::FieldEncryptionConfig {
            fields: sorted,
            key,
        });
        self
    }

    /// Attach a shared HNSW cache (called by `Database::collection()` so all
    /// handles from the same `Database` share a single graph store).
    #[cfg(feature = "vector-hnsw")]
    pub(crate) fn with_hnsw_cache(mut self, cache: SharedHnswCache) -> Self {
        self.hnsw_cache = cache;
        self
    }

    /// Attach the shared decoded-vector cache (called by `Database::collection()`
    /// so all handles from the same `Database` share one cache and invalidate
    /// each other's entries on write).
    pub(crate) fn with_vector_cache(mut self, cache: SharedVectorCache) -> Self {
        self.vector_cache = cache;
        self
    }

    /// Encrypt nominated fields in `doc` in-place, if field encryption is
    /// configured.  No-op when the `encryption` feature is disabled or when
    /// `with_field_encryption` was not called.
    fn encrypt_doc(&self, _doc: &mut Document) -> Result<(), TalaDbError> {
        #[cfg(feature = "encryption")]
        if let Some(cfg) = &self.field_encryption {
            crate::crypto::encrypt_fields(_doc, cfg)?;
        }
        Ok(())
    }

    /// Decrypt nominated fields in `doc` in-place, if field encryption is
    /// configured.  No-op when the `encryption` feature is disabled or when
    /// `with_field_encryption` was not called.
    fn decrypt_doc(&self, _doc: &mut Document) -> Result<(), TalaDbError> {
        #[cfg(feature = "encryption")]
        if let Some(cfg) = &self.field_encryption {
            crate::crypto::decrypt_fields(_doc, cfg)?;
        }
        Ok(())
    }

    /// Apply [`Self::decrypt_doc`] to every document in `docs`.
    fn decrypt_docs(&self, docs: Vec<Document>) -> Result<Vec<Document>, TalaDbError> {
        #[cfg(feature = "encryption")]
        if self.field_encryption.is_some() {
            let mut out = docs;
            for doc in &mut out {
                self.decrypt_doc(doc)?;
            }
            return Ok(out);
        }
        Ok(docs)
    }
}

/// System collections that ARE addressable through [`crate::Database::collection`]
/// despite the reserved `_` prefix. They stay hidden from
/// [`crate::Database::list_collection_names`] and are never synced (the sync
/// orchestration skips every `_`-prefixed name).
///
/// - `__taladb_sync` — bidirectional-sync cursor store, one document per sync
///   target. The JS `db.sync()` orchestration reads and advances cursors
///   through the ordinary collection API, so the name must pass validation.
/// - `__taladb_replica` — replication coverage store, one document per
///   replicated scope: how far a bootstrap walk got, and whether the local copy
///   is complete enough to answer queries without the network. Read and written
///   by the JS replication coordinator through the ordinary collection API.
const ADDRESSABLE_SYSTEM_COLLECTIONS: &[&str] = &["__taladb_sync", "__taladb_replica"];

/// Validate a collection name.
///
/// Rules:
/// - Must not be empty.
/// - Must not exceed 128 characters.
/// - Must not contain `"::"` (reserved for internal table naming).
/// - Must not start with `"_"` (reserved for system collections such as
///   `_audit`; without this, `db.collection("_audit")` would allow normal
///   mutations against the append-only audit log). The explicit
///   [`ADDRESSABLE_SYSTEM_COLLECTIONS`] allowlist is exempt.
///
/// Take a caller-supplied `_id` out of an insert's fields.
///
/// ## Why the engine accepts one at all
///
/// It did not, and it did not say so: `_id` was dropped on the way in (the web
/// and React Native bindings filtered it; Node passed it through as a field that
/// the read path then overwrote with the real id). A document inserted as
/// `{ _id: 'sku-1' }` came back under a fresh ULID, `find({ _id: 'sku-1' })`
/// matched nothing, and re-running the same seed produced a second copy of every
/// row rather than recognising the first.
///
/// That matters more since replication was removed: hydrating from a server is
/// now ordinary application code — fetch, then `insert_many` — and without a
/// caller-controlled id that code cannot be run twice.
///
/// ## Why it must be a ULID
///
/// Ids are 16 raw bytes on disk; every index key ends with them, and their byte
/// order is what makes index order equal `(value, id)` order. An arbitrary
/// string cannot be stored as one. `deriveDocId(collection, key)` hashes a
/// natural key into a ULID for exactly this purpose, and the error points there.
fn take_supplied_id(fields: &mut Vec<(String, Value)>) -> Result<Option<Ulid>, TalaDbError> {
    let Some(position) = fields.iter().position(|(k, _)| k == "_id") else {
        return Ok(None);
    };
    let (_, value) = fields.remove(position);
    // A later duplicate would survive `dedupe_fields` as a plain field and
    // reappear on read, shadowing the real id.
    fields.retain(|(k, _)| k != "_id");
    let Value::Str(raw) = value else {
        return Err(TalaDbError::InvalidDocumentId(format!(
            "_id must be a string, got {}",
            value.type_name()
        )));
    };
    Ulid::from_string(&raw).map(Some).map_err(|_| {
        TalaDbError::InvalidDocumentId(format!(
            "_id \"{raw}\" is not a ULID — derive one from your natural key with \
             deriveDocId(collection, key), which maps the same key to the same id every time"
        ))
    })
}

/// Called by [`crate::Database::collection`] so callers get an error
/// immediately rather than at index-creation time.
pub fn validate_collection_name(name: &str) -> Result<(), TalaDbError> {
    if ADDRESSABLE_SYSTEM_COLLECTIONS.contains(&name) {
        return Ok(());
    }
    if name.is_empty() {
        return Err(TalaDbError::InvalidName(
            "collection name must not be empty".into(),
        ));
    }
    if name.starts_with('_') {
        return Err(TalaDbError::InvalidName(format!(
            "collection name \"{name}\" must not start with \"_\" \
             (reserved for system collections)"
        )));
    }
    if name.len() > 128 {
        return Err(TalaDbError::InvalidName(format!(
            "collection name is too long ({} chars); maximum is 128",
            name.len()
        )));
    }
    if name.contains("::") {
        return Err(TalaDbError::InvalidName(format!(
            "collection name \"{name}\" must not contain \"::\" \
             (reserved for internal table naming)"
        )));
    }
    Ok(())
}

impl Collection {
    fn invalidate_index_cache(&self) {
        let mut guard = self.index_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.remove(&self.name);
    }

    /// Drop the decoded-vector cache entry for a field. Data writes invalidate
    /// via the write generation, but `create`/`drop_vector_index` rewrite the
    /// vec table without bumping it, so they evict explicitly.
    fn evict_vector_cache(&self, field: &str) {
        let key = format!("{}::{}", self.name, field);
        let mut cache = self.vector_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.remove(&key);
    }

    fn load_indexes_cached(&self) -> Result<CachedIndexes, TalaDbError> {
        {
            let guard = self.index_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cached) = guard.get(&self.name) {
                return Ok(cached.clone());
            }
        }
        // Load outside the lock so a slow storage read does not block other
        // collections; a racing loader just overwrites with identical data.
        //
        // One transaction for all four meta tables: this runs on the first
        // touch of every collection, so it is squarely on the cold-start path,
        // and four separate `begin_read`s bought nothing.
        let rtxn = self.backend.begin_read()?;
        let indexes = self.read_indexes(rtxn.as_ref())?;
        let fts_indexes = self.read_fts_indexes(rtxn.as_ref())?;
        let vec_indexes = self.read_vector_indexes(rtxn.as_ref())?;
        let compound_indexes = self.read_compound_indexes(rtxn.as_ref())?;
        drop(rtxn);
        let tables = IndexTables::build(
            &self.name,
            &indexes,
            &fts_indexes,
            &vec_indexes,
            &compound_indexes,
        );
        let cached = CachedIndexes {
            indexes: Arc::new(indexes),
            fts_indexes: Arc::new(fts_indexes),
            vec_indexes: Arc::new(vec_indexes),
            compound_indexes: Arc::new(compound_indexes),
            tables: Arc::new(tables),
        };
        let mut guard = self.index_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.insert(self.name.clone(), cached.clone());
        Ok(cached)
    }

    // ------------------------------------------------------------------
    // Index management
    // ------------------------------------------------------------------

    pub fn create_index(&self, field: &str) -> Result<(), TalaDbError> {
        let meta_key = meta_key(&self.name, field);
        let mut wtxn = self.backend.begin_write()?;

        // Idempotent: no-op if already exists
        if wtxn.get(META_INDEXES_TABLE, meta_key.as_bytes())?.is_some() {
            return Ok(());
        }

        // Write index metadata
        let def = IndexDef {
            collection: self.name.clone(),
            field: field.to_string(),
        };
        let bytes = postcard::to_allocvec(&def)?;
        wtxn.put(META_INDEXES_TABLE, meta_key.as_bytes(), &bytes)?;

        // Backfill existing documents into the new index — one table open for
        // the whole backfill rather than one per document.
        let docs_table = docs_table_name(&self.name);
        let existing = wtxn.range(
            &docs_table,
            std::ops::Bound::Unbounded,
            std::ops::Bound::Unbounded,
        )?;
        let idx_table = index_table_name(&self.name, field);
        let mut keys: Vec<Vec<u8>> = Vec::with_capacity(existing.len());
        for (_, doc_bytes) in existing {
            let doc: Document = postcard::from_bytes(&doc_bytes)?;
            if let Some(val) = doc.get(field) {
                keys.extend(encode_index_keys(val, doc.id));
            }
        }
        let ops: Vec<crate::engine::KvOp<'_>> = keys
            .iter()
            .map(|k| crate::engine::KvOp::Put(k.as_slice(), &[]))
            .collect();
        wtxn.apply_batch(&idx_table, &ops)?;

        wtxn.commit()?;
        self.invalidate_index_cache();
        Ok(())
    }

    pub fn drop_index(&self, field: &str) -> Result<(), TalaDbError> {
        let meta_key = meta_key(&self.name, field);
        let mut wtxn = self.backend.begin_write()?;

        if wtxn.get(META_INDEXES_TABLE, meta_key.as_bytes())?.is_none() {
            return Err(TalaDbError::IndexNotFound(meta_key));
        }

        // Remove all index entries (range scan on the index table)
        let idx_table = index_table_name(&self.name, field);
        let all_entries = wtxn.range(
            &idx_table,
            std::ops::Bound::Unbounded,
            std::ops::Bound::Unbounded,
        )?;
        let ops: Vec<crate::engine::KvOp<'_>> = all_entries
            .iter()
            .map(|(k, _)| crate::engine::KvOp::Delete(k.as_slice()))
            .collect();
        wtxn.apply_batch(&idx_table, &ops)?;

        // Remove metadata
        wtxn.delete(META_INDEXES_TABLE, meta_key.as_bytes())?;
        wtxn.commit()?;
        self.invalidate_index_cache();
        Ok(())
    }

    // ------------------------------------------------------------------
    // FTS index management
    // ------------------------------------------------------------------

    /// Create a full-text search index on a string field.
    /// After calling this, `Filter::Contains(field, query)` will use the index.
    pub fn create_fts_index(&self, field: &str) -> Result<(), TalaDbError> {
        let meta_key = format!("{}::{}", self.name, field);
        let mut wtxn = self.backend.begin_write()?;

        // Idempotent: no-op if already exists
        if wtxn.get(META_FTS_TABLE, meta_key.as_bytes())?.is_some() {
            return Ok(());
        }

        let def = FtsDef {
            collection: self.name.clone(),
            field: field.to_string(),
            version: FTS_VERSION,
        };
        let bytes = postcard::to_allocvec(&def)?;
        wtxn.put(META_FTS_TABLE, meta_key.as_bytes(), &bytes)?;

        // Backfill existing documents
        let docs_table = docs_table_name(&self.name);
        let existing = wtxn.range(
            &docs_table,
            std::ops::Bound::Unbounded,
            std::ops::Bound::Unbounded,
        )?;
        let fts_table = fts_table_name(&self.name, field);
        let len_table = fts_len_table_name(&self.name, field);
        let mut stats = FtsStats::default();
        // Accumulate the whole backfill, then write each table once.
        let mut postings: Vec<(Vec<u8>, [u8; 4])> = Vec::new();
        let mut lengths: Vec<([u8; 16], [u8; 4])> = Vec::new();
        for (_, doc_bytes) in existing {
            let doc: Document = postcard::from_bytes(&doc_bytes)?;
            if let Some(crate::document::Value::Str(text)) = doc.get(field) {
                let (freqs, doc_len) = token_frequencies(text);
                for (token, tf) in &freqs {
                    postings.push((encode_fts_key(token, &doc.id), encode_tf(*tf)));
                }
                lengths.push((doc.id.to_bytes(), encode_doc_len(doc_len)));
                stats.add_doc(doc_len);
            }
        }
        let posting_ops: Vec<crate::engine::KvOp<'_>> = postings
            .iter()
            .map(|(k, v)| crate::engine::KvOp::Put(k.as_slice(), v.as_slice()))
            .collect();
        wtxn.apply_batch(&fts_table, &posting_ops)?;
        let len_ops: Vec<crate::engine::KvOp<'_>> = lengths
            .iter()
            .map(|(k, v)| crate::engine::KvOp::Put(k.as_slice(), v.as_slice()))
            .collect();
        wtxn.apply_batch(&len_table, &len_ops)?;
        let stats_table = fts_stats_table_name(&self.name, field);
        wtxn.put(&stats_table, FTS_STATS_KEY, &postcard::to_allocvec(&stats)?)?;

        wtxn.commit()?;
        self.invalidate_index_cache();
        Ok(())
    }

    /// Drop a full-text search index and all its entries.
    pub fn drop_fts_index(&self, field: &str) -> Result<(), TalaDbError> {
        let meta_key = format!("{}::{}", self.name, field);
        let mut wtxn = self.backend.begin_write()?;

        if wtxn.get(META_FTS_TABLE, meta_key.as_bytes())?.is_none() {
            return Err(TalaDbError::IndexNotFound(format!("fts:{meta_key}")));
        }

        // Clear all FTS entries for this field, plus its ranking side tables
        for table in [
            fts_table_name(&self.name, field),
            fts_len_table_name(&self.name, field),
            fts_stats_table_name(&self.name, field),
        ] {
            let all = wtxn.range(
                &table,
                std::ops::Bound::Unbounded,
                std::ops::Bound::Unbounded,
            )?;
            let ops: Vec<crate::engine::KvOp<'_>> = all
                .iter()
                .map(|(k, _)| crate::engine::KvOp::Delete(k.as_slice()))
                .collect();
            wtxn.apply_batch(&table, &ops)?;
        }
        wtxn.delete(META_FTS_TABLE, meta_key.as_bytes())?;
        wtxn.commit()?;
        self.invalidate_index_cache();
        Ok(())
    }

    /// Rank documents against a free-text query using BM25.
    ///
    /// Unlike the `$contains` filter, which requires *every* token to be
    /// present, this uses OR semantics: a document matching more query terms
    /// simply scores higher. That is what BM25 is built for, and it is the
    /// behaviour hybrid retrieval needs.
    ///
    /// Returns at most `top_k` results, most relevant first.
    pub fn search_text(
        &self,
        field: &str,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<TextSearchResult>, TalaDbError> {
        self.search_text_with(field, query, top_k, &Bm25Params::default(), None)
    }

    /// [`Collection::search_text`] with explicit BM25 tuning and an optional
    /// metadata pre-filter.
    ///
    /// As with `find_nearest`, the filter is resolved *before* ranking, so
    /// `top_k` is `k` documents that actually match rather than whatever
    /// survives a post-filter.
    pub fn search_text_with(
        &self,
        field: &str,
        query: &str,
        top_k: usize,
        params: &Bm25Params,
        pre_filter: Option<Filter>,
    ) -> Result<Vec<TextSearchResult>, TalaDbError> {
        let def = self
            .load_fts_indexes()?
            .into_iter()
            .find(|d| d.field == field)
            .ok_or_else(|| TalaDbError::IndexNotFound(format!("fts:{}::{}", self.name, field)))?;

        // A v0 index has no term frequencies, lengths, or corpus stats, so
        // every match would score identically. Failing loudly beats returning
        // a ranking that only looks like one.
        if !def.supports_ranking() {
            return Err(TalaDbError::InvalidOperation(format!(
                "fts index on '{}.{}' predates ranking support; drop and recreate it to enable search_text",
                self.name, field
            )));
        }

        let tokens = {
            let (freqs, _) = token_frequencies(query);
            freqs.into_keys().collect::<Vec<_>>()
        };
        if tokens.is_empty() || top_k == 0 {
            return Ok(vec![]);
        }

        // Resolve the pre-filter up front so scoring skips candidates that
        // could never appear in the result.
        let id_filter: Option<HashSet<[u8; 16]>> = match pre_filter {
            Some(filter) => Some(self.find(filter)?.iter().map(|d| d.id.to_bytes()).collect()),
            None => None,
        };
        if let Some(ids) = &id_filter
            && ids.is_empty()
        {
            return Ok(vec![]);
        }

        let rtxn = self.backend.begin_read()?;
        let fts_table = fts_table_name(&self.name, field);
        let len_table = fts_len_table_name(&self.name, field);
        let stats_table = fts_stats_table_name(&self.name, field);

        let stats: FtsStats = rtxn
            .get(&stats_table, FTS_STATS_KEY)?
            .and_then(|b| postcard::from_bytes(&b).ok())
            .unwrap_or_default();
        let avg_len = stats.avg_doc_len();

        let mut scores: HashMap<[u8; 16], f32> = HashMap::new();
        let mut lengths: HashMap<[u8; 16], u32> = HashMap::new();

        for token in &tokens {
            let (start, end) = crate::fts::fts_token_range(token);
            let postings = rtxn.range(
                &fts_table,
                std::ops::Bound::Included(start.as_slice()),
                std::ops::Bound::Included(end.as_slice()),
            )?;
            // The postings scan *is* the document frequency — no counter to keep.
            // Note this counts the whole corpus, not the filtered subset: IDF
            // describes how informative a term is overall, and recomputing it
            // per filter would make scores incomparable between queries.
            let doc_freq = postings.len() as u64;
            if doc_freq == 0 {
                continue;
            }
            let token_idf = crate::bm25::idf(stats.doc_count, doc_freq);

            for (key, value) in postings {
                let Some(ulid) = crate::fts::ulid_from_fts_key(&key) else {
                    continue;
                };
                let id = ulid.to_bytes();
                if let Some(ids) = &id_filter
                    && !ids.contains(&id)
                {
                    continue;
                }
                let doc_len = match lengths.get(&id) {
                    Some(len) => *len,
                    None => {
                        let len = rtxn
                            .get(&len_table, &id)?
                            .map(|b| decode_doc_len(&b))
                            .unwrap_or(0);
                        lengths.insert(id, len);
                        len
                    }
                };
                let score = crate::bm25::term_score(
                    crate::fts::decode_tf(&value),
                    doc_len,
                    avg_len,
                    token_idf,
                    params,
                );
                *scores.entry(id).or_insert(0.0) += score;
            }
        }

        let mut ranked: Vec<([u8; 16], f32)> = scores.into_iter().collect();
        // Ties break on ULID so results are stable across identical queries.
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        ranked.truncate(top_k);

        let docs_table = docs_table_name(&self.name);
        let mut out = Vec::with_capacity(ranked.len());
        for (id, score) in ranked {
            if let Some(bytes) = rtxn.get(&docs_table, &id)? {
                let document: Document = postcard::from_bytes(&bytes)?;
                out.push(TextSearchResult { document, score });
            }
        }
        Ok(out)
    }

    /// Hybrid retrieval: rank by BM25 *and* vector similarity, then fuse the
    /// two rankings with reciprocal rank fusion.
    ///
    /// This is what "hybrid search" means everywhere else in the industry —
    /// sparse plus dense — and it exists because the two retrievers fail
    /// differently. Keyword search misses paraphrases; vector search misses
    /// exact identifiers, product codes, and rare proper nouns. Fusing them
    /// recovers both, and a document both retrievers like outranks one that
    /// only a single retriever loved.
    ///
    /// The optional filter is applied to both sides before ranking, so the
    /// two candidate pools stay comparable.
    pub fn hybrid_search(
        &self,
        query: HybridQuery<'_>,
    ) -> Result<Vec<HybridSearchResult>, TalaDbError> {
        let HybridQuery {
            text_field,
            text,
            vector_field,
            vector,
            top_k,
            filter,
            rrf,
            bm25,
            candidates,
        } = query;

        if top_k == 0 {
            return Ok(vec![]);
        }
        // Fusing only `top_k` from each side loses documents that sit just
        // outside one ranking but high in the other — exactly the recall the
        // technique is meant to recover. Over-fetch, fuse, then truncate.
        let pool = candidates.unwrap_or_else(|| (top_k * 4).max(20));

        let text_hits = self.search_text_with(text_field, text, pool, &bm25, filter.clone())?;
        let vector_hits = self.find_nearest(vector_field, vector, pool, filter)?;

        let mut fused: HashMap<[u8; 16], FusedEntry> = HashMap::new();
        let mut docs: HashMap<[u8; 16], Document> = HashMap::new();

        for (rank, hit) in text_hits.into_iter().enumerate() {
            let id = hit.document.id.to_bytes();
            let entry = fused.entry(id).or_default();
            entry.score += crate::bm25::rrf_contribution(rank, rrf.k, rrf.text_weight);
            entry.text_rank = Some(rank);
            docs.entry(id).or_insert(hit.document);
        }

        for (rank, hit) in vector_hits.into_iter().enumerate() {
            let id = hit.document.id.to_bytes();
            let entry = fused.entry(id).or_default();
            entry.score += crate::bm25::rrf_contribution(rank, rrf.k, rrf.vector_weight);
            entry.vector_rank = Some(rank);
            docs.entry(id).or_insert(hit.document);
        }

        let mut ranked: Vec<([u8; 16], FusedEntry)> = fused.into_iter().collect();
        // Ties break on ULID so identical queries return identical order.
        ranked.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        ranked.truncate(top_k);

        Ok(ranked
            .into_iter()
            .filter_map(|(id, entry)| {
                docs.remove(&id).map(|document| HybridSearchResult {
                    document,
                    score: entry.score,
                    text_rank: entry.text_rank,
                    vector_rank: entry.vector_rank,
                })
            })
            .collect())
    }

    // ------------------------------------------------------------------
    // Compound index management
    // ------------------------------------------------------------------

    /// Create a compound index on an ordered list of fields.
    ///
    /// A compound index accelerates queries where all listed fields are
    /// constrained by equality (`Filter::Eq`), e.g.:
    ///
    /// ```ignore
    /// col.create_compound_index(&["lastName", "firstName"])?;
    /// col.find(Filter::And(vec![
    ///     Filter::Eq("lastName".into(), Value::Str("Smith".into())),
    ///     Filter::Eq("firstName".into(), Value::Str("John".into())),
    /// ]))?;
    /// ```
    ///
    /// Backfills existing documents. The index name is derived from the
    /// field list, so `["a","b"]` and `["b","a"]` are two distinct indexes.
    pub fn create_compound_index(&self, fields: &[&str]) -> Result<(), TalaDbError> {
        if fields.len() < 2 {
            return Err(TalaDbError::InvalidOperation(
                "compound index requires at least 2 fields".into(),
            ));
        }
        if fields
            .iter()
            .any(|field| field.is_empty() || field.contains("::"))
        {
            return Err(TalaDbError::InvalidOperation(
                "compound index fields must be non-empty and must not contain '::'".into(),
            ));
        }
        let unique: std::collections::HashSet<&str> = fields.iter().copied().collect();
        if unique.len() != fields.len() {
            return Err(TalaDbError::InvalidOperation(
                "compound index fields must be unique".into(),
            ));
        }
        let meta_key = compound_meta_key(&self.name, fields);
        let mut wtxn = self.backend.begin_write()?;

        // Idempotent
        if wtxn
            .get(META_COMPOUND_TABLE, meta_key.as_bytes())?
            .is_some()
        {
            return Ok(());
        }

        let def = CompoundIndexDef {
            collection: self.name.clone(),
            fields: fields.iter().map(std::string::ToString::to_string).collect(),
        };
        let bytes = postcard::to_allocvec(&def)?;
        wtxn.put(META_COMPOUND_TABLE, meta_key.as_bytes(), &bytes)?;

        // Backfill existing documents
        let docs_table = docs_table_name(&self.name);
        let existing = wtxn.range(
            &docs_table,
            std::ops::Bound::Unbounded,
            std::ops::Bound::Unbounded,
        )?;
        let ctable = compound_table_name(&self.name, fields);
        let mut keys: Vec<Vec<u8>> = Vec::with_capacity(existing.len());
        for (_, doc_bytes) in existing {
            let doc: Document = postcard::from_bytes(&doc_bytes)?;
            let vals: Option<Vec<&crate::document::Value>> =
                fields.iter().map(|f| doc.get(f)).collect();
            if let Some(v) = vals {
                keys.extend(encode_compound_keys(&v, doc.id)?);
            }
        }
        let ops: Vec<crate::engine::KvOp<'_>> = keys
            .iter()
            .map(|k| crate::engine::KvOp::Put(k.as_slice(), &[]))
            .collect();
        wtxn.apply_batch(&ctable, &ops)?;

        wtxn.commit()?;
        self.invalidate_index_cache();
        Ok(())
    }

    /// Drop a compound index and all its stored entries.
    pub fn drop_compound_index(&self, fields: &[&str]) -> Result<(), TalaDbError> {
        let meta_key = compound_meta_key(&self.name, fields);
        let mut wtxn = self.backend.begin_write()?;

        if wtxn
            .get(META_COMPOUND_TABLE, meta_key.as_bytes())?
            .is_none()
        {
            return Err(TalaDbError::IndexNotFound(format!("compound:{meta_key}")));
        }

        let ctable = compound_table_name(&self.name, fields);
        let all = wtxn.range(
            &ctable,
            std::ops::Bound::Unbounded,
            std::ops::Bound::Unbounded,
        )?;
        let ops: Vec<crate::engine::KvOp<'_>> = all
            .iter()
            .map(|(k, _)| crate::engine::KvOp::Delete(k.as_slice()))
            .collect();
        wtxn.apply_batch(&ctable, &ops)?;
        wtxn.delete(META_COMPOUND_TABLE, meta_key.as_bytes())?;
        wtxn.commit()?;
        self.invalidate_index_cache();
        Ok(())
    }

    fn read_compound_indexes(
        &self,
        rtxn: &dyn crate::engine::ReadTxn,
    ) -> Result<Vec<CompoundIndexDef>, TalaDbError> {
        self.scan_meta(rtxn, META_COMPOUND_TABLE, |v| Ok(postcard::from_bytes(v)?))
    }

    /// Decode every entry of a meta table whose key belongs to this collection.
    ///
    /// Meta keys are `"{collection}::{...}"`, so the collection's entries form a
    /// contiguous key range — this seeks straight to it instead of reading the
    /// whole table and filtering, and streams the values rather than
    /// materialising every row first.
    fn scan_meta<T>(
        &self,
        rtxn: &dyn crate::engine::ReadTxn,
        table: &str,
        decode: impl Fn(&[u8]) -> Result<T, TalaDbError>,
    ) -> Result<Vec<T>, TalaDbError> {
        let prefix = format!("{}::", self.name);
        let start = prefix.into_bytes();
        // Successor of the prefix: same bytes with the final one incremented,
        // which is the first key that cannot start with `prefix`. `:` is 0x3A,
        // so it never overflows.
        let mut end = start.clone();
        if let Some(last) = end.last_mut() {
            *last += 1;
        }
        let mut out = Vec::new();
        let mut err = None;
        rtxn.scan(
            table,
            std::ops::Bound::Included(start.as_slice()),
            std::ops::Bound::Excluded(end.as_slice()),
            &mut |_, v| match decode(v) {
                Ok(def) => {
                    out.push(def);
                    Ok(crate::engine::ScanFlow::Continue)
                }
                Err(e) => {
                    err = Some(e);
                    Ok(crate::engine::ScanFlow::Stop)
                }
            },
        )?;
        match err {
            Some(e) => Err(e),
            None => Ok(out),
        }
    }

    // ------------------------------------------------------------------
    // Vector index management
    // ------------------------------------------------------------------

    /// Create a vector index on `field`.
    ///
    /// - `dimensions`: expected length of every stored vector.
    /// - `metric`: similarity metric used by `find_nearest` (default: Cosine).
    /// - `hnsw`: when `Some`, builds an HNSW approximate-nearest-neighbor index
    ///   in addition to the flat vector table.  Requires the `vector-hnsw` feature;
    ///   ignored (with a no-op) if the feature is disabled.
    ///
    /// Backfills any existing documents that already have a numeric array in
    /// `field`. Silently skips documents where `field` is absent or not a
    /// numeric array.
    pub fn create_vector_index(
        &self,
        field: &str,
        dimensions: usize,
        metric: Option<VectorMetric>,
        hnsw: Option<HnswOptions>,
    ) -> Result<(), TalaDbError> {
        let meta_key = vec_meta_key(&self.name, field);
        let mut wtxn = self.backend.begin_write()?;

        // Idempotent: no-op if already exists
        if wtxn.get(META_VECTOR_TABLE, meta_key.as_bytes())?.is_some() {
            return Ok(());
        }

        let resolved_metric = metric.unwrap_or_default();
        let def = VectorDef {
            collection: self.name.clone(),
            field: field.to_string(),
            dimensions,
            metric: resolved_metric,
        };
        let bytes = postcard::to_allocvec(&def)?;
        wtxn.put(META_VECTOR_TABLE, meta_key.as_bytes(), &bytes)?;

        // Backfill existing documents into flat vec table
        let docs_table = docs_table_name(&self.name);
        let existing = wtxn.range(
            &docs_table,
            std::ops::Bound::Unbounded,
            std::ops::Bound::Unbounded,
        )?;
        let vtable = vec_table_name(&self.name, field);
        // The decoded copy exists only to seed the HNSW graph, so it is built
        // only when a graph is actually going to be built. Populating it
        // unconditionally cost a second full copy of every vector — on 100k
        // documents at 384 dimensions that is ~150 MB of WASM heap held
        // alongside `encoded` for the whole backfill, and on the default build
        // (no `vector-hnsw`) it was never read at all.
        #[cfg(feature = "vector-hnsw")]
        let mut backfill: Vec<(ulid::Ulid, Vec<f32>)> = Vec::new();
        let mut encoded: Vec<([u8; 16], Vec<u8>)> = Vec::new();
        for (_, doc_bytes) in existing {
            let doc: Document = postcard::from_bytes(&doc_bytes)?;
            // Backfill stays lenient where the write path is strict: a document
            // written *before* this index existed is not a caller error, and
            // failing the whole `create_vector_index` on one stray vector leaves
            // no way forward. Pinned by `backfill_skips_documents_without_a_
            // usable_vector`. Writes made once the index exists are rejected —
            // see `write_docs_and_indexes`.
            if let Some(val) = doc.get(field)
                && let Some(vec) = value_to_f32_vec(val)
                && vec.len() == dimensions
            {
                encoded.push((doc.id.to_bytes(), encode_f32_vec(&vec)));
                #[cfg(feature = "vector-hnsw")]
                if hnsw.is_some() {
                    backfill.push((doc.id, vec));
                }
            }
        }
        let ops: Vec<crate::engine::KvOp<'_>> = encoded
            .iter()
            .map(|(k, v)| crate::engine::KvOp::Put(k.as_slice(), v.as_slice()))
            .collect();
        wtxn.apply_batch(&vtable, &ops)?;

        // Persist HNSW options and build the in-memory graph when requested
        if let Some(hnsw_opts) = hnsw {
            let hnsw_meta_key = format!("{}::{}", self.name, field);
            let opts_bytes = postcard::to_allocvec(&hnsw_opts)?;
            wtxn.put(META_HNSW_TABLE, hnsw_meta_key.as_bytes(), &opts_bytes)?;

            #[cfg(feature = "vector-hnsw")]
            {
                let graph = build_hnsw(&backfill, &resolved_metric, hnsw_opts.ef_construction)?;
                let cache_key = format!("{}::{}", self.name, field);
                let mut cache = self.hnsw_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                cache.insert(cache_key, graph);
            }
        }

        wtxn.commit()?;
        self.invalidate_index_cache();
        self.evict_vector_cache(field);
        Ok(())
    }

    /// Drop a vector index and remove all stored vectors for that field.
    /// Also removes any HNSW graph and options associated with this index.
    pub fn drop_vector_index(&self, field: &str) -> Result<(), TalaDbError> {
        let meta_key = vec_meta_key(&self.name, field);
        let mut wtxn = self.backend.begin_write()?;

        if wtxn.get(META_VECTOR_TABLE, meta_key.as_bytes())?.is_none() {
            return Err(TalaDbError::VectorIndexNotFound(format!(
                "{}::{}",
                self.name, field
            )));
        }

        // Remove flat vector entries
        let vtable = vec_table_name(&self.name, field);
        let all = wtxn.range(
            &vtable,
            std::ops::Bound::Unbounded,
            std::ops::Bound::Unbounded,
        )?;
        let ops: Vec<crate::engine::KvOp<'_>> = all
            .iter()
            .map(|(k, _)| crate::engine::KvOp::Delete(k.as_slice()))
            .collect();
        wtxn.apply_batch(&vtable, &ops)?;

        // Remove HNSW metadata (if present) and evict from in-memory cache
        let hnsw_meta_key = format!("{}::{}", self.name, field);
        let _ = wtxn.delete(META_HNSW_TABLE, hnsw_meta_key.as_bytes());
        #[cfg(feature = "vector-hnsw")]
        {
            let cache_key = format!("{}::{}", self.name, field);
            let mut cache = self.hnsw_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            cache.remove(&cache_key);
        }

        wtxn.delete(META_VECTOR_TABLE, meta_key.as_bytes())?;
        wtxn.commit()?;
        self.invalidate_index_cache();
        self.evict_vector_cache(field);
        Ok(())
    }

    /// Search for the `top_k` most similar documents to `query` using the
    /// named vector index.
    ///
    /// When the index was created with `hnsw: Some(...)` and the `vector-hnsw`
    /// feature is enabled, uses the HNSW approximate-nearest-neighbor graph for
    /// sub-linear search.  Falls back automatically to the flat brute-force
    /// scan when no HNSW graph is stored (e.g. the feature is disabled, or the
    /// graph has not been built yet).
    ///
    /// **HNSW staleness:** the graph is built in memory at
    /// `create_vector_index` / [`Self::upgrade_vector_index`] /
    /// `Database::rebuild_hnsw_indexes` time and is *not* updated by later
    /// inserts, updates, or deletes. Documents inserted after the last build
    /// are invisible to HNSW search until the graph is rebuilt; deleted
    /// documents are dropped from results (the search over-fetches to
    /// compensate, so `top_k` is still honoured when possible). Rebuild after
    /// bulk writes with [`Self::upgrade_vector_index`]. The flat (non-HNSW)
    /// path is always exact and current.
    ///
    /// If `pre_filter` is `Some`, only documents matching that filter are
    /// considered. This lets you combine metadata filtering with vector
    /// similarity in one call.  Pre-filtering forces flat search regardless of
    /// whether an HNSW graph exists, because the graph does not support
    /// arbitrary set-membership constraints.
    ///
    /// Results are ordered by descending similarity score (highest first).
    #[tracing::instrument(skip(self, query, pre_filter), fields(collection = %self.name, field, top_k))]
    pub fn find_nearest(
        &self,
        field: &str,
        query: &[f32],
        top_k: usize,
        pre_filter: Option<Filter>,
    ) -> Result<Vec<VectorSearchResult>, TalaDbError> {
        // 1. Load the vector index definition
        let defs = self.load_vector_indexes()?;
        let def = defs
            .iter()
            .find(|d| d.field == field)
            .ok_or_else(|| TalaDbError::VectorIndexNotFound(format!("{}::{}", self.name, field)))?;

        // 2. Validate query dimensions
        if query.len() != def.dimensions {
            return Err(TalaDbError::VectorDimensionMismatch {
                expected: def.dimensions,
                got: query.len(),
            });
        }

        // 3a. HNSW path — only when no pre-filter and a graph is in the cache.
        #[cfg(feature = "vector-hnsw")]
        if pre_filter.is_none() {
            let cache_key = format!("{}::{}", self.name, field);
            let graph_opt = {
                let cache = self.hnsw_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                cache.get(&cache_key).cloned()
            };
            if let Some(graph) = graph_opt {
                // The graph may contain ids deleted since the last rebuild —
                // load_results drops those. Over-fetch so the caller still
                // receives top_k results despite a moderate amount of churn.
                let fetch_k = top_k + (top_k / 5).max(8);
                let scored = search_hnsw(&graph, query, &def.metric, fetch_k);
                let mut results = self.load_results(scored)?;
                results.truncate(top_k);
                return Ok(results);
            }
        }

        // 3b. Flat (brute-force) path — served from the decoded-vector cache
        //     when fresh, so repeated queries never re-read storage.
        //
        //     The generation is captured *before* the scan. If a write commits
        //     during a cache-miss scan, it bumps the generation via
        //     `watch::notify`, so the entry stored under the older generation
        //     simply misses on the next read and is rebuilt — the flat path is
        //     never served stale.
        let generation = crate::watch::generation(&self.watch_registry);
        let cache_key = format!("{}::{}", self.name, field);
        let vectors: Arc<Vec<(ulid::Ulid, Vec<f32>)>> = {
            let cached = {
                let cache = self.vector_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                cache
                    .get(&cache_key)
                    .filter(|c| c.generation == generation)
                    .map(|c| Arc::clone(&c.vectors))
            };
            match cached {
                Some(vecs) => vecs,
                None => {
                    // Miss — read and decode the whole vec table once. The lock
                    // is released during storage IO; a concurrent miss just
                    // rebuilds redundantly, which is correct.
                    let vtable = vec_table_name(&self.name, field);
                    let rtxn = self.backend.begin_read()?;
                    let all_entries = rtxn.scan_all(&vtable)?;
                    drop(rtxn);
                    let mut decoded: Vec<(ulid::Ulid, Vec<f32>)> =
                        Vec::with_capacity(all_entries.len());
                    for (key_bytes, val_bytes) in &all_entries {
                        let arr: [u8; 16] = match key_bytes.as_slice().try_into() {
                            Ok(a) => a,
                            Err(_) => continue,
                        };
                        if let Some(v) = decode_f32_vec(val_bytes) {
                            decoded.push((ulid::Ulid::from_bytes(arr), v));
                        }
                    }
                    let arc = Arc::new(decoded);
                    let mut cache = self.vector_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    cache.insert(
                        cache_key,
                        CachedVectors {
                            generation,
                            vectors: Arc::clone(&arc),
                        },
                    );
                    arc
                }
            }
        };

        // 4. Resolve the pre-filter to a set of matching ids up front, so the
        //    scoring loop skips candidates that can't appear in the result
        //    (a 10%-selective filter skips scoring the other 90%).
        let id_filter: Option<std::collections::HashSet<ulid::Ulid>> = match pre_filter {
            Some(filter) => Some(self.find(filter)?.iter().map(|d| d.id).collect()),
            None => None,
        };

        // 5. Score the in-memory decoded vectors against the query.
        //
        //    The query's L2 norm is constant across every candidate, so it is
        //    computed once here instead of being recomputed inside
        //    `cosine_similarity` for each stored vector (one extra dot product
        //    plus a sqrt per candidate, over the whole table).
        let metric = &def.metric;
        let query_norm = crate::vector::l2_norm(query);
        let mut scored: Vec<(ulid::Ulid, f32)> = Vec::with_capacity(vectors.len());
        for (id, vec) in vectors.iter() {
            if let Some(ids) = &id_filter
                && !ids.contains(id)
            {
                continue;
            }
            // Skip entries whose dimension doesn't match the query. The scoring
            // reductions walk both slices with `chunks_exact` and stop at the
            // shorter, so a wrong-length vector would otherwise be *partially*
            // scored and could rank into the top-k on a truncated dot product.
            //
            // The write path enforces `vec.len() == vdef.dimensions`, so this is
            // unreachable from ordinary inserts — it guards the paths that
            // bypass it: `restore_from_snapshot` (which writes raw table bytes)
            // and on-disk corruption, where `decode_f32_vec` happily returns a
            // short vector as long as the byte count is a multiple of four.
            if vec.len() != query.len() {
                continue;
            }
            scored.push((
                *id,
                crate::vector::score_with_query_norm(metric, query, query_norm, vec),
            ));
        }

        // 6. Select the top_k by score. `select_nth_unstable` partitions in
        //    O(n) average instead of the O(n log n) of a full sort, then only
        //    the k retained results are sorted.
        // `top_k == 0` asks for nothing. Falling through to the partition below
        // left `scored` untouched and returned the *whole* collection — the
        // opposite of the request, and unbounded.
        if top_k == 0 {
            return Ok(Vec::new());
        }
        let k = top_k.min(scored.len());
        if k > 0 && k < scored.len() {
            scored.select_nth_unstable_by(k - 1, |a, b| {
                b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
            });
            scored.truncate(k);
        }
        scored.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        self.load_results(scored)
    }

    /// Rebuild the HNSW graph for a vector index from the current flat vector
    /// table.  Use this after bulk inserts or when the graph has become stale.
    ///
    /// The in-memory graph is **not** maintained incrementally: documents
    /// written after the last build are invisible to HNSW search (and deleted
    /// ones linger in the graph) until this is called again.
    ///
    /// Requires the `vector-hnsw` feature.  Returns `Ok(())` (no-op) when the
    /// feature is disabled or when no HNSW options exist for the given field.
    pub fn upgrade_vector_index(&self, field: &str) -> Result<(), TalaDbError> {
        // Load current VectorDef to get metric & dimensions
        let defs = self.load_vector_indexes()?;
        let def = defs
            .iter()
            .find(|d| d.field == field)
            .ok_or_else(|| TalaDbError::VectorIndexNotFound(format!("{}::{}", self.name, field)))?
            .clone();

        // Load HNSW options — if not present, this index is flat-only; nothing to do
        let hnsw_meta_key = format!("{}::{}", self.name, field);
        let rtxn = self.backend.begin_read()?;
        let opts_bytes = rtxn.get(META_HNSW_TABLE, hnsw_meta_key.as_bytes())?;
        drop(rtxn);

        let hnsw_opts: HnswOptions = match opts_bytes {
            Some(b) => postcard::from_bytes(&b)?,
            None => return Ok(()), // flat index — nothing to upgrade
        };

        // Read all vectors from the flat table
        let vtable = vec_table_name(&self.name, field);
        let rtxn = self.backend.begin_read()?;
        let all_entries = rtxn.scan_all(&vtable)?;
        drop(rtxn);

        let mut vectors: Vec<(ulid::Ulid, Vec<f32>)> = Vec::with_capacity(all_entries.len());
        for (key_bytes, val_bytes) in &all_entries {
            if key_bytes.len() == 16 {
                let arr: [u8; 16] = match key_bytes.as_slice().try_into() {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                let id = ulid::Ulid::from_bytes(arr);
                if let Some(v) = decode_f32_vec(val_bytes) {
                    vectors.push((id, v));
                }
            }
        }

        #[cfg(feature = "vector-hnsw")]
        {
            let graph = build_hnsw(&vectors, &def.metric, hnsw_opts.ef_construction)?;
            let cache_key = format!("{}::{}", self.name, field);
            let mut cache = self.hnsw_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            cache.insert(cache_key, graph);
        }

        // Suppress unused-variable warning when feature is disabled
        let _ = (hnsw_opts, vectors, def);
        Ok(())
    }

    /// Load full documents for a set of `(Ulid, score)` pairs.
    fn load_results(
        &self,
        scored: Vec<(ulid::Ulid, f32)>,
    ) -> Result<Vec<VectorSearchResult>, TalaDbError> {
        let docs_table = docs_table_name(&self.name);
        let rtxn = self.backend.begin_read()?;
        let mut results = Vec::with_capacity(scored.len());
        for (id, score) in scored {
            if let Some(bytes) = rtxn.get(&docs_table, &id.to_bytes())? {
                let mut document: Document = postcard::from_bytes(&bytes)?;
                // Match `find`: encrypted fields come back as plaintext.
                self.decrypt_doc(&mut document)?;
                results.push(VectorSearchResult { document, score });
            }
        }
        Ok(results)
    }

    /// Return a description of all indexes on this collection.
    pub fn list_indexes(&self) -> Result<CollectionIndexInfo, TalaDbError> {
        let btree = self.load_indexes()?.into_iter().map(|d| d.field).collect();
        let fts = self
            .load_fts_indexes()?
            .into_iter()
            .map(|d| d.field)
            .collect();
        let vector = self
            .load_vector_indexes()?
            .into_iter()
            .map(|d| d.field)
            .collect();
        Ok(CollectionIndexInfo { btree, fts, vector })
    }

    fn load_vector_indexes(&self) -> Result<Vec<VectorDef>, TalaDbError> {
        let rtxn = self.backend.begin_read()?;
        self.read_vector_indexes(rtxn.as_ref())
    }

    fn read_vector_indexes(
        &self,
        rtxn: &dyn crate::engine::ReadTxn,
    ) -> Result<Vec<VectorDef>, TalaDbError> {
        self.scan_meta(rtxn, META_VECTOR_TABLE, |v| Ok(postcard::from_bytes(v)?))
    }

    fn load_fts_indexes(&self) -> Result<Vec<FtsDef>, TalaDbError> {
        let rtxn = self.backend.begin_read()?;
        self.read_fts_indexes(rtxn.as_ref())
    }

    fn read_fts_indexes(
        &self,
        rtxn: &dyn crate::engine::ReadTxn,
    ) -> Result<Vec<FtsDef>, TalaDbError> {
        // Tolerates pre-versioning metadata; see `decode_fts_def`.
        self.scan_meta(rtxn, META_FTS_TABLE, |v| Ok(decode_fts_def(v)?))
    }

    fn load_indexes(&self) -> Result<Vec<IndexDef>, TalaDbError> {
        let rtxn = self.backend.begin_read()?;
        self.read_indexes(rtxn.as_ref())
    }

    fn read_indexes(
        &self,
        rtxn: &dyn crate::engine::ReadTxn,
    ) -> Result<Vec<IndexDef>, TalaDbError> {
        self.scan_meta(rtxn, META_INDEXES_TABLE, |v| Ok(postcard::from_bytes(v)?))
    }

    // ------------------------------------------------------------------
    // Write helpers
    // ------------------------------------------------------------------

    fn write_doc_and_indexes_with_compound(
        &self,
        doc: &Document,
        old_doc: Option<&Document>,
        cache: &CachedIndexes,
        wtxn: &mut dyn crate::engine::WriteTxn,
    ) -> Result<(), TalaDbError> {
        self.write_docs_and_indexes(&[(doc, old_doc)], cache, wtxn)
    }

    /// Write `docs` and maintain every index, grouping storage operations by
    /// table so each table is resolved **once per batch** rather than once per
    /// document per index.
    ///
    /// Ordering within a table is deletes-before-puts, which is safe because
    /// every index key embeds the document's ULID: one document's delete can
    /// never target another's insert. FTS corpus statistics are likewise
    /// accumulated in memory and written once, instead of a read-modify-write
    /// of the same record per document.
    ///
    /// Each element is `(new_doc, previous_version)`; `None` means an insert.
    fn write_docs_and_indexes(
        &self,
        docs: &[(&Document, Option<&Document>)],
        cache: &CachedIndexes,
        wtxn: &mut dyn crate::engine::WriteTxn,
    ) -> Result<(), TalaDbError> {
        use crate::engine::KvOp;
        if docs.is_empty() {
            return Ok(());
        }
        let tables = &cache.tables;

        // An update whose indexed field did not change produces a delete and a
        // put of the *same* key — work that cancels out. Detecting that here is
        // what keeps `$set` on one field from paying for every other index on
        // the collection: without it, flagging 2500 documents re-tokenized a
        // full-text field none of them touched, once per document, plus a
        // storage read each for the recorded document length.
        //
        // Only meaningful for updates (`old_doc` is `Some`) and only when the
        // id is unchanged, since every index key embeds it.
        fn unchanged<'a>(
            doc: &'a Document,
            old_doc: Option<&&'a Document>,
            field: &str,
        ) -> bool {
            old_doc.is_some_and(|old| old.id == doc.id && old.get(field) == doc.get(field))
        }

        // --- document bodies ---
        let mut doc_keys: Vec<[u8; 16]> = Vec::with_capacity(docs.len());
        let mut doc_bodies: Vec<Vec<u8>> = Vec::with_capacity(docs.len());
        for (doc, _) in docs {
            doc_keys.push(doc.id.to_bytes());
            doc_bodies.push(postcard::to_allocvec(doc)?);
        }
        let body_ops: Vec<KvOp<'_>> = doc_keys
            .iter()
            .zip(&doc_bodies)
            .map(|(k, v)| KvOp::Put(k.as_slice(), v.as_slice()))
            .collect();
        wtxn.apply_batch(&tables.docs, &body_ops)?;

        // --- secondary indexes ---
        for (idx, table) in cache.indexes.iter().zip(&tables.btree) {
            let mut keys: Vec<(Vec<u8>, bool)> = Vec::new(); // (key, is_delete)
            for (doc, old_doc) in docs {
                if unchanged(doc, old_doc.as_ref(), &idx.field) {
                    continue;
                }
                if let Some(old) = old_doc
                    && let Some(old_val) = old.get(&idx.field)
                {
                    keys.extend(encode_index_keys(old_val, old.id).into_iter().map(|k| (k, true)));
                }
                if let Some(new_val) = doc.get(&idx.field) {
                    keys.extend(
                        encode_index_keys(new_val, doc.id).into_iter().map(|k| (k, false)),
                    );
                }
            }
            if keys.is_empty() {
                continue;
            }
            // Deletes first: an update whose indexed value is unchanged emits
            // the same key twice, and the put must win.
            let ops: Vec<KvOp<'_>> = keys
                .iter()
                .filter(|(_, del)| *del)
                .map(|(k, _)| KvOp::Delete(k.as_slice()))
                .chain(
                    keys.iter()
                        .filter(|(_, del)| !*del)
                        .map(|(k, _)| KvOp::Put(k.as_slice(), &[])),
                )
                .collect();
            wtxn.apply_batch(table, &ops)?;
        }

        // --- FTS indexes ---
        for (fts, (fts_table, len_table, stats_table)) in
            cache.fts_indexes.iter().zip(&tables.fts)
        {
            let mut posting_deletes: Vec<Vec<u8>> = Vec::new();
            let mut posting_puts: Vec<(Vec<u8>, [u8; 4])> = Vec::new();
            let mut len_deletes: Vec<[u8; 16]> = Vec::new();
            let mut len_puts: Vec<([u8; 16], [u8; 4])> = Vec::new();
            let mut removed_total: Vec<u32> = Vec::new();
            let mut added_total: Vec<u32> = Vec::new();

            for (doc, old_doc) in docs {
                // Re-tokenizing unchanged text is the most expensive no-op in
                // this function — see `unchanged`.
                if unchanged(doc, old_doc.as_ref(), &fts.field) {
                    continue;
                }
                if let Some(old) = old_doc
                    && let Some(crate::document::Value::Str(old_text)) = old.get(&fts.field)
                {
                    for token in tokenize(old_text) {
                        posting_deletes.push(encode_fts_key(&token, &old.id));
                    }
                    // Trust the recorded length over a re-tokenization of the
                    // old text: if the tokenizer ever changes, the stored value
                    // is what the running totals were actually built from.
                    let stored = wtxn
                        .get(len_table, &old.id.to_bytes())?
                        .map(|b| decode_doc_len(&b));
                    removed_total.push(stored.unwrap_or_else(|| token_frequencies(old_text).1));
                    len_deletes.push(old.id.to_bytes());
                }
                if let Some(crate::document::Value::Str(new_text)) = doc.get(&fts.field) {
                    let (freqs, doc_len) = token_frequencies(new_text);
                    for (token, tf) in &freqs {
                        posting_puts.push((encode_fts_key(token, &doc.id), encode_tf(*tf)));
                    }
                    len_puts.push((doc.id.to_bytes(), encode_doc_len(doc_len)));
                    added_total.push(doc_len);
                }
            }

            if !posting_deletes.is_empty() || !posting_puts.is_empty() {
                let ops: Vec<KvOp<'_>> = posting_deletes
                    .iter()
                    .map(|k| KvOp::Delete(k.as_slice()))
                    .chain(
                        posting_puts
                            .iter()
                            .map(|(k, v)| KvOp::Put(k.as_slice(), v.as_slice())),
                    )
                    .collect();
                wtxn.apply_batch(fts_table, &ops)?;
            }
            if !len_deletes.is_empty() || !len_puts.is_empty() {
                let ops: Vec<KvOp<'_>> = len_deletes
                    .iter()
                    .map(|k| KvOp::Delete(k.as_slice()))
                    .chain(
                        len_puts
                            .iter()
                            .map(|(k, v)| KvOp::Put(k.as_slice(), v.as_slice())),
                    )
                    .collect();
                wtxn.apply_batch(len_table, &ops)?;
            }
            if !removed_total.is_empty() || !added_total.is_empty() {
                adjust_fts_stats_batch(wtxn, stats_table, &removed_total, &added_total)?;
            }
        }

        // --- vector indexes ---
        for (vdef, vtable) in cache.vec_indexes.iter().zip(&tables.vectors) {
            let mut deletes: Vec<[u8; 16]> = Vec::new();
            let mut puts: Vec<([u8; 16], Vec<u8>)> = Vec::new();
            for (doc, old_doc) in docs {
                // An embedding is the largest value in a typical document;
                // rewriting it because some other field changed is pure cost.
                if unchanged(doc, old_doc.as_ref(), &vdef.field) {
                    continue;
                }
                if old_doc.is_some() {
                    deletes.push(doc.id.to_bytes());
                }
                if let Some(val) = doc.get(&vdef.field)
                    && let Some(vec) = value_to_f32_vec(val)
                {
                    // A wrong-length vector used to be skipped here, which
                    // stored the document but left it out of the index: it
                    // could never be returned by `find_nearest`, and nothing
                    // said so. On a vector database that is the worst kind of
                    // failure — a silently incomplete result set. Reject the
                    // write instead; the transaction rolls back uncommitted.
                    //
                    // Only a value that *parses* as a vector is checked. A
                    // missing field, or a `null` standing in for "not embedded
                    // yet", is still perfectly legal.
                    if vec.len() != vdef.dimensions {
                        return Err(TalaDbError::VectorDimensionMismatch {
                            expected: vdef.dimensions,
                            got: vec.len(),
                        });
                    }
                    puts.push((doc.id.to_bytes(), encode_f32_vec(&vec)));
                }
            }
            if deletes.is_empty() && puts.is_empty() {
                continue;
            }
            let ops: Vec<KvOp<'_>> = deletes
                .iter()
                .map(|k| KvOp::Delete(k.as_slice()))
                .chain(puts.iter().map(|(k, v)| KvOp::Put(k.as_slice(), v.as_slice())))
                .collect();
            wtxn.apply_batch(vtable, &ops)?;
        }

        // --- compound indexes ---
        for (cidx, ctable) in cache.compound_indexes.iter().zip(&tables.compound) {
            let field_refs: Vec<&str> = cidx.fields.iter().map(std::string::String::as_str).collect();
            let mut deletes: Vec<Vec<u8>> = Vec::new();
            let mut puts: Vec<Vec<u8>> = Vec::new();
            for (doc, old_doc) in docs {
                // Unchanged only when *every* member field is unchanged — the
                // compound key is built from all of them.
                if field_refs
                    .iter()
                    .all(|f| unchanged(doc, old_doc.as_ref(), f))
                    && old_doc.is_some()
                {
                    continue;
                }
                if let Some(old) = old_doc {
                    let old_vals: Option<Vec<&Value>> =
                        field_refs.iter().map(|f| old.get(f)).collect();
                    if let Some(v) = old_vals {
                        deletes.extend(encode_compound_keys(&v, old.id)?);
                    }
                }
                let new_vals: Option<Vec<&Value>> = field_refs.iter().map(|f| doc.get(f)).collect();
                if let Some(v) = new_vals {
                    puts.extend(encode_compound_keys(&v, doc.id)?);
                }
            }
            if deletes.is_empty() && puts.is_empty() {
                continue;
            }
            let ops: Vec<KvOp<'_>> = deletes
                .iter()
                .map(|k| KvOp::Delete(k.as_slice()))
                .chain(puts.iter().map(|k| KvOp::Put(k.as_slice(), &[])))
                .collect();
            wtxn.apply_batch(ctable, &ops)?;
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    /// Fail if `id` is already stored, so `insert` cannot overwrite.
    ///
    /// `insert` means create. Silently replacing a document because the caller
    /// happened to supply an id that exists would make a re-run of a seed script
    /// destroy whatever the application had since written to those documents.
    fn reject_existing_id(&self, id: Ulid) -> Result<(), TalaDbError> {
        let rtxn = self.backend.begin_read()?;
        if rtxn
            .get(&docs_table_name(&self.name), &id.to_bytes())?
            .is_some()
        {
            return Err(TalaDbError::DuplicateId(id.to_string()));
        }
        Ok(())
    }

    pub fn insert(&self, mut fields: Vec<(String, Value)>) -> Result<Ulid, TalaDbError> {
        let supplied = take_supplied_id(&mut fields)?;
        // Auto-stamp `_changed_at` so callers always have a last-modified time.
        // Engine-owned: a caller-supplied value is dropped.
        fields.retain(|(k, _)| k != "_changed_at");
        fields.push(("_changed_at".into(), Value::Int(now_ms() as i64)));
        let mut doc = match supplied {
            Some(id) => {
                self.reject_existing_id(id)?;
                Document::with_id(id, fields)
            }
            None => Document::new(fields),
        };
        // Encrypt nominated fields before writing indexes or the doc body.
        // Index entries are written from the plaintext doc, so encrypted fields
        // are not indexable (intentional — see `with_field_encryption` docs).
        self.encrypt_doc(&mut doc)?;
        let cache = self.load_indexes_cached()?;
        let mut wtxn = self.backend.begin_write()?;
        self.write_doc_and_indexes_with_compound(&doc, None, &cache, wtxn.as_mut())?;
        let id = doc.id;
        // Audit row commits atomically with the insert.
        if let Some(caller) = &self.audit_caller {
            write_audit_entry(
                wtxn.as_mut(),
                &self.name,
                AuditOp::Insert,
                &id.to_string(),
                caller,
            )?;
        }
        wtxn.commit()?;
        crate::watch::notify(&self.watch_registry);
        Ok(id)
    }

    pub fn insert_many(&self, items: Vec<Vec<(String, Value)>>) -> Result<Vec<Ulid>, TalaDbError> {
        let ts = now_ms() as i64;
        let mut supplied_ids: Vec<Ulid> = Vec::new();
        let mut docs: Vec<Document> = Vec::with_capacity(items.len());
        for mut fields in items {
            let supplied = take_supplied_id(&mut fields)?;
            fields.retain(|(k, _)| k != "_changed_at");
            fields.push(("_changed_at".into(), Value::Int(ts)));
            docs.push(match supplied {
                Some(id) => {
                    // Within the batch as well as against storage: two documents
                    // carrying one id would otherwise have the second silently
                    // overwrite the first, and `insert_many` would report two
                    // ids for one stored document.
                    if supplied_ids.contains(&id) {
                        return Err(TalaDbError::DuplicateId(id.to_string()));
                    }
                    supplied_ids.push(id);
                    self.reject_existing_id(id)?;
                    Document::with_id(id, fields)
                }
                None => Document::new(fields),
            });
        }
        for doc in &mut docs {
            self.encrypt_doc(doc)?;
        }
        let cache = self.load_indexes_cached()?;
        let mut wtxn = self.backend.begin_write()?;
        // One batched pass over the whole insert: every index table is opened
        // once, not once per document.
        let batch: Vec<(&Document, Option<&Document>)> =
            docs.iter().map(|d| (d, None)).collect();
        self.write_docs_and_indexes(&batch, &cache, wtxn.as_mut())?;
        let mut ids = Vec::with_capacity(docs.len());
        for doc in &docs {
            // Audit rows commit atomically with the batch.
            if let Some(caller) = &self.audit_caller {
                write_audit_entry(
                    wtxn.as_mut(),
                    &self.name,
                    AuditOp::Insert,
                    &doc.id.to_string(),
                    caller,
                )?;
            }
            ids.push(doc.id);
        }
        wtxn.commit()?;
        crate::watch::notify(&self.watch_registry);
        Ok(ids)
    }

    #[tracing::instrument(skip(self, filter), fields(collection = %self.name))]
    pub fn find(&self, filter: Filter) -> Result<Vec<Document>, TalaDbError> {
        let cache = self.load_indexes_cached()?;
        let qplan = plan_full(
            &filter,
            &cache.indexes,
            &cache.fts_indexes,
            &cache.compound_indexes,
        );
        let rtxn = self.backend.begin_read()?;
        let docs = execute(&qplan, &filter, rtxn.as_ref(), &self.name, None)?;
        self.decrypt_docs(docs)
    }

    /// Return the first document matching `filter`, or `None`.
    ///
    /// Stops at the first match rather than materialising the whole result set
    /// and taking its head — on an unindexed predicate that is the difference
    /// between decoding one document and decoding the entire collection.
    ///
    /// "First" means first in the plan's natural order (index order for an
    /// index-backed plan, `_id` order for a scan), which is the same document
    /// the previous implementation returned.
    pub fn find_one(&self, filter: Filter) -> Result<Option<Document>, TalaDbError> {
        let cache = self.load_indexes_cached()?;
        let qplan = plan_full(
            &filter,
            &cache.indexes,
            &cache.fts_indexes,
            &cache.compound_indexes,
        );
        let rtxn = self.backend.begin_read()?;
        let docs = crate::query::executor::execute_limited(
            &qplan,
            &filter,
            rtxn.as_ref(),
            &self.name,
            None,
            Some(1),
        )?;
        Ok(self.decrypt_docs(docs)?.into_iter().next())
    }

    /// Like `find`, but with sort, pagination, and field projection.
    ///
    /// Processing order:
    /// 1. Filter (index-accelerated where possible)
    /// 2. Sort (`options.sort`)
    /// 3. Skip (`options.skip`)
    /// 4. Limit (`options.limit`)
    /// 5. Projection (`options.fields`)
    #[tracing::instrument(skip(self, filter, options), fields(collection = %self.name))]
    pub fn find_with_options(
        &self,
        filter: Filter,
        options: FindOptions,
    ) -> Result<Vec<Document>, TalaDbError> {
        let deadline = options.timeout.map(|d| std::time::Instant::now() + d);
        let cache = self.load_indexes_cached()?;
        let qplan = plan_full(
            &filter,
            &cache.indexes,
            &cache.fts_indexes,
            &cache.compound_indexes,
        );
        let rtxn = self.backend.begin_read()?;
        // Without a sort the result is the plan's natural order, so `skip +
        // limit` documents are all that can ever be observed — stop the walk
        // there instead of decoding the rest and throwing them away. With a
        // sort, every candidate can still reach the page, so no limit is
        // pushed down.
        let pushdown = if options.sort.is_empty() {
            options
                .limit
                .map(|l| clamp_to_usize(options.skip).saturating_add(clamp_to_usize(l)))
        } else {
            None
        };
        let mut docs = crate::query::executor::execute_limited(
            &qplan,
            &filter,
            rtxn.as_ref(),
            &self.name,
            deadline,
            pushdown,
        )?;
        // Decrypt encrypted fields before sorting/projecting.
        docs = self.decrypt_docs(docs)?;

        // Sort — if both sort and limit are set, use a partial sort (O(n + k log k))
        // which is dramatically faster than the O(n log n) full sort when
        // `skip + limit` is small compared to the candidate set.
        if !options.sort.is_empty() {
            if let Some(limit) = options.limit {
                let keep = clamp_to_usize(options.skip).saturating_add(clamp_to_usize(limit));
                partial_sort_documents(&mut docs, &options.sort, keep);
            } else {
                sort_documents(&mut docs, &options.sort);
            }
        }

        // Skip
        if options.skip > 0 {
            let skip = clamp_to_usize(options.skip);
            if skip >= docs.len() {
                return Ok(vec![]);
            }
            docs.drain(..skip);
        }

        // Limit
        if let Some(limit) = options.limit {
            docs.truncate(clamp_to_usize(limit));
        }

        // Projection
        if let Some(ref fields) = options.fields {
            docs = docs
                .into_iter()
                .map(|d| project_document(d, fields))
                .collect();
        }

        Ok(docs)
    }

    /// Serve a bounded `$sort` (+`$skip`/`$limit`) straight from a secondary
    /// index, decoding only the documents on the requested page.
    ///
    /// Returns `None` when the shape doesn't qualify and the caller should fall
    /// back to the ordinary scan-then-sort path.
    ///
    /// Why this exists: without it, `[$sort, $skip, $limit]` over an unfiltered
    /// collection decodes **every** document just to order them and then throw
    /// all but a page away — the dominant cost of paging a large catalog.
    /// Walking the index touches no documents at all.
    ///
    /// Correctness rests on two facts:
    /// * index keys are `encode_value_prefix(value) ++ ulid` with an
    ///   order-preserving encoding, so index order **is** `(value, id)` order —
    ///   the total order the sort comparator defines. This holds for every value
    ///   type *except* a field mixing `Int` and `Float`: those carry distinct
    ///   type tags (`TAG_INT` < `TAG_FLOAT`), so the index groups all integers
    ///   before all floats while `cmp_values` compares them numerically. We
    ///   detect that case below and decline the fast path;
    /// * only the *first* sort key needs to be indexed. Documents past the run
    ///   of ties that straddles the page boundary compare strictly worse on that
    ///   first key, so no later key can pull them onto the page. We therefore
    ///   take the page plus the rest of its boundary tie-run, decode just those,
    ///   and let the normal pipeline order them on the full multi-key spec.
    fn aggregate_via_sorted_index(
        &self,
        pipeline: &[Stage],
    ) -> Result<Option<Vec<Document>>, TalaDbError> {
        let Some(Stage::Sort(specs)) = pipeline.first() else {
            return Ok(None);
        };
        let Some(spec) = specs.first() else {
            return Ok(None);
        };
        // Only worth it when the reachable set is bounded by a `$limit`.
        let Some(keep) = crate::aggregate::reachable_after_sort(&pipeline[1..]) else {
            return Ok(None);
        };

        let cache = self.load_indexes_cached()?;
        if !cache.indexes.iter().any(|i| i.field == spec.field) {
            return Ok(None);
        }

        let rtxn = self.backend.begin_read()?;
        let mut entries = index_ordered_entries(rtxn.as_ref(), &self.name, &spec.field)?;

        // A document whose sort field is absent has no index entry, so the index
        // is not a faithful ordering of the collection. Bail out rather than
        // silently drop those documents from the result.
        if entries.len() as u64 != rtxn.count_entries(&docs_table_name(&self.name))? {
            return Ok(None);
        }

        // Index order groups every Int before every Float (distinct type tags),
        // but the sort comparator ranks them numerically — so on a field holding
        // both, the index is not sorted the way the query asks. Truncating to a
        // page here would drop documents the comparator would have ranked onto
        // it, and the final sort could never pull them back (they are never
        // decoded). Bail out to the scan-and-sort path, which is always correct.
        let mut has_int = false;
        let mut has_float = false;
        for entry in &entries {
            match entry.value_prefix.first() {
                Some(&crate::index::TAG_INT) => has_int = true,
                Some(&crate::index::TAG_FLOAT) => has_float = true,
                _ => {}
            }
            if has_int && has_float {
                return Ok(None);
            }
        }

        if spec.direction == SortDirection::Desc {
            entries.reverse();
        }

        // Take the page, then extend through the end of the tie-run it lands in.
        let mut take = keep.min(entries.len());
        if take > 0 {
            let boundary = entries[take - 1].value_prefix.clone();
            while take < entries.len() && entries[take].value_prefix == boundary {
                take += 1;
            }
        }

        let ulids = entries.into_iter().take(take).map(|e| e.id).collect();
        let docs = fetch_documents(rtxn.as_ref(), &self.name, ulids)?;

        // Hand the (small) candidate set to the normal pipeline: it applies the
        // full multi-key sort, then the same $skip/$limit/$project as always.
        Ok(Some(execute_pipeline(docs, pipeline)?))
    }

    /// Execute an aggregation pipeline against the collection.
    ///
    /// If the first stage is `Stage::Match`, the query planner is consulted so
    /// that any available index can be used to narrow the candidate set before
    /// the remaining stages run. An unfiltered, bounded `$sort` is instead served
    /// directly from that field's index — see `aggregate_via_sorted_index`.
    pub fn aggregate(
        &self,
        pipeline: crate::aggregate::Pipeline,
    ) -> Result<Vec<Document>, TalaDbError> {
        if let Some(page) = self.aggregate_via_sorted_index(&pipeline)? {
            return Ok(page);
        }

        let (initial_docs, rest_start) = if let Some(Stage::Match(filter)) = pipeline.first() {
            let cache = self.load_indexes_cached()?;
            let plan = plan_full(
                filter,
                &cache.indexes,
                &cache.fts_indexes,
                &cache.compound_indexes,
            );
            let rtxn = self.backend.begin_read()?;
            let docs = execute(&plan, filter, rtxn.as_ref(), &self.name, None)?;
            (docs, 1usize)
        } else {
            let rtxn = self.backend.begin_read()?;
            let docs = execute(
                &crate::query::planner::QueryPlan::FullScan,
                &Filter::All,
                rtxn.as_ref(),
                &self.name,
                None,
            )?;
            (docs, 0usize)
        };

        execute_pipeline(initial_docs, &pipeline[rest_start..])
    }

    pub fn insert_with_id(&self, doc: Document) -> Result<Ulid, TalaDbError> {
        let cache = self.load_indexes_cached()?;
        let mut wtxn = self.backend.begin_write()?;
        let id = doc.id;
        self.write_doc_and_indexes_with_compound(&doc, None, &cache, wtxn.as_mut())?;
        wtxn.commit()?;
        crate::watch::notify(&self.watch_registry);
        Ok(id)
    }

    /// Insert or replace a document preserving its ULID, in a single write
    /// transaction.
    ///
    /// Unlike `delete_by_id` followed by `insert_with_id`, this:
    /// - maintains secondary/FTS/vector/compound indexes against the previous
    ///   version of the document,
    /// - is atomic (no window where the document is absent).
    pub fn replace_with_id(&self, mut doc: Document) -> Result<Ulid, TalaDbError> {
        // Symmetric with the find/find_by_id read paths, which decrypt:
        // documents flowing through sync (find → export → import → replace)
        // arrive here as plaintext and must be re-encrypted before storage.
        self.encrypt_doc(&mut doc)?;
        let cache = self.load_indexes_cached()?;
        let docs_table = docs_table_name(&self.name);
        let id = doc.id;
        let mut wtxn = self.backend.begin_write()?;
        // Read the previous version inside the write txn so old index entries
        // are computed from the bytes actually being replaced.
        let old_doc: Option<Document> = match wtxn.get(&docs_table, &id.to_bytes())? {
            Some(bytes) => Some(postcard::from_bytes(&bytes)?),
            None => None,
        };
        self.write_doc_and_indexes_with_compound(&doc, old_doc.as_ref(), &cache, wtxn.as_mut())?;
        wtxn.commit()?;
        crate::watch::notify(&self.watch_registry);
        Ok(id)
    }

    pub fn find_by_id(&self, id: Ulid) -> Result<Option<Document>, TalaDbError> {
        let docs_table = docs_table_name(&self.name);
        let rtxn = self.backend.begin_read()?;
        match rtxn.get(&docs_table, &id.to_bytes())? {
            Some(bytes) => {
                let mut doc: Document = postcard::from_bytes(&bytes)?;
                // Match `find`: encrypted fields come back as plaintext.
                self.decrypt_doc(&mut doc)?;
                Ok(Some(doc))
            }
            None => Ok(None),
        }
    }

    pub fn delete_by_id(&self, id: Ulid) -> Result<bool, TalaDbError> {
        let cache = self.load_indexes_cached()?;
        let docs_table = docs_table_name(&self.name);
        let rtxn = self.backend.begin_read()?;
        let doc: Option<Document> = match rtxn.get(&docs_table, &id.to_bytes())? {
            Some(bytes) => Some(postcard::from_bytes(&bytes)?),
            None => None,
        };
        drop(rtxn);
        match doc {
            None => Ok(false),
            Some(doc) => {
                let mut wtxn = self.backend.begin_write()?;
                self.delete_doc_and_indexes_with_compound(&doc, &cache, wtxn.as_mut())?;
                wtxn.commit()?;
                crate::watch::notify(&self.watch_registry);
                Ok(true)
            }
        }
    }

    pub fn update_one(&self, filter: Filter, update: Update) -> Result<bool, TalaDbError> {
        let cache = self.load_indexes_cached()?;
        let qplan = plan_full(
            &filter,
            &cache.indexes,
            &cache.fts_indexes,
            &cache.compound_indexes,
        );
        let rtxn = self.backend.begin_read()?;
        let candidates = execute(&qplan, &filter, rtxn.as_ref(), &self.name, None)?;
        drop(rtxn);

        // Candidates were collected in a read snapshot that is now released;
        // a concurrent writer may have changed or deleted them. Re-fetch and
        // re-check each one inside the (exclusive) write transaction so the
        // mutation and its old-index cleanup are computed from current bytes.
        let regex_cache = filter.compile_regex_cache()?;
        let docs_table = docs_table_name(&self.name);
        let mut wtxn = self.backend.begin_write()?;
        for candidate in candidates {
            let stored_old: Document = match wtxn.get(&docs_table, &candidate.id.to_bytes())? {
                Some(bytes) => postcard::from_bytes(&bytes)?,
                None => continue, // deleted since the snapshot
            };
            if !filter.matches_with_cache(&stored_old, &regex_cache) {
                continue; // modified since the snapshot and no longer matches
            }
            // Clone BEFORE decryption: the write path needs the exact stored
            // ciphertext to compute matching old-index entries. Re-encrypting
            // with fresh nonces would produce different bytes and leak stale
            // index entries on any encrypted-field index.
            let mut old_doc = stored_old.clone();
            self.decrypt_doc(&mut old_doc)?;
            let mut new_doc = old_doc.clone();
            apply_update(&mut new_doc, update)?;
            self.encrypt_doc(&mut new_doc)?;
            self.write_doc_and_indexes_with_compound(
                &new_doc,
                Some(&stored_old),
                &cache,
                wtxn.as_mut(),
            )?;
            // Audit row commits atomically with the update.
            if let Some(caller) = &self.audit_caller {
                write_audit_entry(
                    wtxn.as_mut(),
                    &self.name,
                    AuditOp::Update,
                    &old_doc.id.to_string(),
                    caller,
                )?;
            }
            wtxn.commit()?;
            crate::watch::notify(&self.watch_registry);
            return Ok(true);
        }
        Ok(false)
    }

    pub fn update_many(&self, filter: Filter, update: Update) -> Result<u64, TalaDbError> {
        let cache = self.load_indexes_cached()?;
        let qplan = plan_full(
            &filter,
            &cache.indexes,
            &cache.fts_indexes,
            &cache.compound_indexes,
        );
        let rtxn = self.backend.begin_read()?;
        let candidates = execute(&qplan, &filter, rtxn.as_ref(), &self.name, None)?;
        drop(rtxn);

        let mut count = 0u64;
        // Re-fetch and re-check every candidate inside the write transaction:
        // the read snapshot used to gather them is already released, and a
        // concurrent writer may have changed or deleted them in between.
        let regex_cache = filter.compile_regex_cache()?;
        let docs_table = docs_table_name(&self.name);
        let mut wtxn = self.backend.begin_write()?;
        // Re-read every candidate in one batched pass. Candidate ids are
        // distinct (index keys embed the ULID, and the Or/In plans deduplicate),
        // so reading them all before writing any is equivalent to interleaving —
        // and costs one table open instead of one per candidate.
        let cand_keys: Vec<[u8; 16]> = candidates.iter().map(|c| c.id.to_bytes()).collect();
        let cand_refs: Vec<&[u8]> = cand_keys.iter().map(<[u8; 16]>::as_slice).collect();
        let stored: Vec<Option<Vec<u8>>> = wtxn.get_many(&docs_table, &cand_refs)?;
        // Both versions of each updated document, held until the whole set is
        // computed so the index maintenance can run as one batched pass.
        let mut pairs: Vec<(Document, Document)> = Vec::new(); // (new, stored_old)
        for bytes in stored.into_iter() {
            let Some(bytes) = bytes else { continue };
            let stored_old: Document = postcard::from_bytes(&bytes)?;
            if !filter.matches_with_cache(&stored_old, &regex_cache) {
                continue;
            }
            let mut plain_old = stored_old.clone();
            self.decrypt_doc(&mut plain_old)?;
            let mut new_doc = plain_old.clone();
            apply_update(&mut new_doc, update.clone())?;
            self.encrypt_doc(&mut new_doc)?;
            // Audit row commits atomically with the batch.
            if let Some(caller) = &self.audit_caller {
                write_audit_entry(
                    wtxn.as_mut(),
                    &self.name,
                    AuditOp::Update,
                    &stored_old.id.to_string(),
                    caller,
                )?;
            }
            pairs.push((new_doc, stored_old));
            count += 1;
        }
        let batch: Vec<(&Document, Option<&Document>)> =
            pairs.iter().map(|(new, old)| (new, Some(old))).collect();
        self.write_docs_and_indexes(&batch, &cache, wtxn.as_mut())?;
        wtxn.commit()?;
        crate::watch::notify(&self.watch_registry);
        Ok(count)
    }

    pub fn delete_one(&self, filter: Filter) -> Result<bool, TalaDbError> {
        let cache = self.load_indexes_cached()?;
        let qplan = plan_full(
            &filter,
            &cache.indexes,
            &cache.fts_indexes,
            &cache.compound_indexes,
        );
        let rtxn = self.backend.begin_read()?;
        let candidates = execute(&qplan, &filter, rtxn.as_ref(), &self.name, None)?;
        drop(rtxn);

        // Re-fetch and re-check inside the write transaction (see update_one).
        let regex_cache = filter.compile_regex_cache()?;
        let docs_table = docs_table_name(&self.name);
        let mut wtxn = self.backend.begin_write()?;
        for candidate in candidates {
            let current: Document = match wtxn.get(&docs_table, &candidate.id.to_bytes())? {
                Some(bytes) => postcard::from_bytes(&bytes)?,
                None => continue,
            };
            if !filter.matches_with_cache(&current, &regex_cache) {
                continue;
            }
            let doc_id = current.id.to_string();
            self.delete_doc_and_indexes_with_compound(&current, &cache, wtxn.as_mut())?;
            // Audit row commits atomically with the delete.
            if let Some(caller) = &self.audit_caller {
                write_audit_entry(wtxn.as_mut(), &self.name, AuditOp::Delete, &doc_id, caller)?;
            }
            wtxn.commit()?;
            crate::watch::notify(&self.watch_registry);
            return Ok(true);
        }
        Ok(false)
    }

    pub fn delete_many(&self, filter: Filter) -> Result<u64, TalaDbError> {
        let cache = self.load_indexes_cached()?;
        let qplan = plan_full(
            &filter,
            &cache.indexes,
            &cache.fts_indexes,
            &cache.compound_indexes,
        );
        let rtxn = self.backend.begin_read()?;
        let candidates = execute(&qplan, &filter, rtxn.as_ref(), &self.name, None)?;
        drop(rtxn);

        let mut count = 0u64;
        // Re-fetch and re-check every candidate inside the write transaction
        // (see update_many); only documents that still match are deleted, and
        // hooks/audit fire only for those.
        let regex_cache = filter.compile_regex_cache()?;
        let docs_table = docs_table_name(&self.name);
        let mut deleted: Vec<Document> = Vec::with_capacity(candidates.len());
        let mut wtxn = self.backend.begin_write()?;
        for candidate in &candidates {
            let current: Document = match wtxn.get(&docs_table, &candidate.id.to_bytes())? {
                Some(bytes) => postcard::from_bytes(&bytes)?,
                None => continue,
            };
            if !filter.matches_with_cache(&current, &regex_cache) {
                continue;
            }
            // Audit row commits atomically with the batch.
            if let Some(caller) = &self.audit_caller {
                write_audit_entry(
                    wtxn.as_mut(),
                    &self.name,
                    AuditOp::Delete,
                    &current.id.to_string(),
                    caller,
                )?;
            }
            deleted.push(current);
            count += 1;
        }
        // One batched pass: each index table is opened once, not once per doc.
        let batch: Vec<&Document> = deleted.iter().collect();
        self.delete_docs_and_indexes(&batch, &cache, wtxn.as_mut())?;
        wtxn.commit()?;
        crate::watch::notify(&self.watch_registry);
        Ok(count)
    }

    pub fn count(&self, filter: Filter) -> Result<u64, TalaDbError> {
        // Fast path: unfiltered count reads redb's table length directly,
        // which is O(1) and avoids deserialising every document.
        if matches!(filter, Filter::All) {
            let rtxn = self.backend.begin_read()?;
            return rtxn.count_entries(&docs_table_name(&self.name));
        }
        // Run the query without the field-decryption pass that `find` does —
        // only the match count matters, not field contents. When the plan's
        // index range exactly reproduces the filter, the index entries answer
        // the question and no document is decoded at all.
        let cache = self.load_indexes_cached()?;
        let qplan = plan_full(
            &filter,
            &cache.indexes,
            &cache.fts_indexes,
            &cache.compound_indexes,
        );
        let rtxn = self.backend.begin_read()?;
        crate::query::executor::count_matching(&qplan, &filter, rtxn.as_ref(), &self.name)
    }

    fn delete_doc_and_indexes_with_compound(
        &self,
        doc: &Document,
        cache: &CachedIndexes,
        wtxn: &mut dyn crate::engine::WriteTxn,
    ) -> Result<(), TalaDbError> {
        self.delete_docs_and_indexes(std::slice::from_ref(&doc), cache, wtxn)
    }

    /// Delete `docs` and every index entry they own, grouped by table so each
    /// table is resolved once per batch. Mirror of [`Self::write_docs_and_indexes`].
    fn delete_docs_and_indexes(
        &self,
        docs: &[&Document],
        cache: &CachedIndexes,
        wtxn: &mut dyn crate::engine::WriteTxn,
    ) -> Result<(), TalaDbError> {
        use crate::engine::KvOp;
        if docs.is_empty() {
            return Ok(());
        }
        let tables = &cache.tables;
        let ids: Vec<[u8; 16]> = docs.iter().map(|d| d.id.to_bytes()).collect();

        let body_ops: Vec<KvOp<'_>> =
            ids.iter().map(|k| KvOp::Delete(k.as_slice())).collect();
        wtxn.apply_batch(&tables.docs, &body_ops)?;

        for (idx, table) in cache.indexes.iter().zip(&tables.btree) {
            let keys: Vec<Vec<u8>> = docs
                .iter()
                .filter_map(|doc| doc.get(&idx.field).map(|val| encode_index_keys(val, doc.id)))
                .flatten()
                .collect();
            let ops: Vec<KvOp<'_>> = keys.iter().map(|k| KvOp::Delete(k.as_slice())).collect();
            wtxn.apply_batch(table, &ops)?;
        }

        for (fts, (fts_table, len_table, stats_table)) in
            cache.fts_indexes.iter().zip(&tables.fts)
        {
            let mut posting_keys: Vec<Vec<u8>> = Vec::new();
            let mut len_keys: Vec<[u8; 16]> = Vec::new();
            let mut removed: Vec<u32> = Vec::new();
            for doc in docs {
                if let Some(crate::document::Value::Str(text)) = doc.get(&fts.field) {
                    for token in tokenize(text) {
                        posting_keys.push(encode_fts_key(&token, &doc.id));
                    }
                    let stored = wtxn
                        .get(len_table, &doc.id.to_bytes())?
                        .map(|b| decode_doc_len(&b));
                    removed.push(stored.unwrap_or_else(|| token_frequencies(text).1));
                    len_keys.push(doc.id.to_bytes());
                }
            }
            if posting_keys.is_empty() {
                continue;
            }
            let ops: Vec<KvOp<'_>> = posting_keys
                .iter()
                .map(|k| KvOp::Delete(k.as_slice()))
                .collect();
            wtxn.apply_batch(fts_table, &ops)?;
            let len_ops: Vec<KvOp<'_>> =
                len_keys.iter().map(|k| KvOp::Delete(k.as_slice())).collect();
            wtxn.apply_batch(len_table, &len_ops)?;
            adjust_fts_stats_batch(wtxn, stats_table, &removed, &[])?;
        }

        for vtable in &tables.vectors {
            let ops: Vec<KvOp<'_>> =
                ids.iter().map(|k| KvOp::Delete(k.as_slice())).collect();
            wtxn.apply_batch(vtable, &ops)?;
        }

        for (cidx, ctable) in cache.compound_indexes.iter().zip(&tables.compound) {
            let field_refs: Vec<&str> = cidx.fields.iter().map(std::string::String::as_str).collect();
            let mut keys: Vec<Vec<u8>> = Vec::new();
            for doc in docs {
                let vals: Option<Vec<&Value>> =
                    field_refs.iter().map(|f| doc.get(f)).collect();
                if let Some(v) = vals {
                    // A delete must remove every key the write path wrote, or an
                    // array member leaves entries pointing at a document that no
                    // longer exists.
                    keys.extend(encode_compound_keys(&v, doc.id)?);
                }
            }
            let ops: Vec<KvOp<'_>> = keys.iter().map(|k| KvOp::Delete(k.as_slice())).collect();
            wtxn.apply_batch(ctable, &ops)?;
        }

        Ok(())
    }
}

/// A document's running fusion state while the two rankings are merged.
#[derive(Default)]
struct FusedEntry {
    score: f32,
    text_rank: Option<usize>,
    vector_rank: Option<usize>,
}

/// Apply a document's arrival and/or departure to a field's corpus statistics.
///
/// Read-modify-write is safe here because every caller already holds the
/// single write transaction.
/// Apply a batch of document arrivals and departures to a field's corpus
/// statistics: one read-modify-write of the stats record regardless of how many
/// documents moved.
///
/// `add_doc`/`remove_doc` are commutative counter updates, so folding them in
/// memory and persisting once is identical to applying each in turn — and turns
/// an O(N) sequence of read-modify-writes on a single hot key into O(1).
fn adjust_fts_stats_batch(
    wtxn: &mut dyn crate::engine::WriteTxn,
    stats_table: &str,
    removed_lens: &[u32],
    added_lens: &[u32],
) -> Result<(), TalaDbError> {
    if removed_lens.is_empty() && added_lens.is_empty() {
        return Ok(());
    }
    let mut stats: FtsStats = match wtxn.get(stats_table, FTS_STATS_KEY)? {
        // A corrupt or truncated stats record must not fail the write; the
        // worst case is a slightly wrong average until the index is rebuilt.
        Some(bytes) => postcard::from_bytes(&bytes).unwrap_or_default(),
        None => FtsStats::default(),
    };
    for len in removed_lens {
        stats.remove_doc(*len);
    }
    for len in added_lens {
        stats.add_doc(*len);
    }
    wtxn.put(stats_table, FTS_STATS_KEY, &postcard::to_allocvec(&stats)?)?;
    Ok(())
}


fn apply_update(doc: &mut Document, update: Update) -> Result<(), TalaDbError> {
    match update {
        Update::Many(updates) => {
            for update in updates {
                apply_update(doc, update)?;
            }
        }
        Update::Set(pairs) => {
            for (k, v) in pairs {
                doc.set(k, v);
            }
        }
        Update::Unset(keys) => {
            for k in keys {
                doc.remove(&k);
            }
        }
        Update::Inc(pairs) => {
            for (k, delta) in pairs {
                if !matches!(delta, Value::Int(_) | Value::Float(_)) {
                    return Err(TalaDbError::TypeError {
                        expected: "numeric $inc delta".into(),
                        got: delta.type_name().into(),
                    });
                }
                let new_val = match (doc.get(&k), &delta) {
                    (Some(Value::Int(n)), Value::Int(d)) => {
                        Value::Int(n.checked_add(*d).ok_or_else(|| {
                            TalaDbError::InvalidOperation(format!(
                                "$inc overflows i64 on field \"{k}\""
                            ))
                        })?)
                    }
                    (Some(Value::Float(n)), Value::Float(d)) => Value::Float(n + d),
                    (Some(Value::Int(n)), Value::Float(d)) => Value::Float(*n as f64 + d),
                    (Some(Value::Float(n)), Value::Int(d)) => Value::Float(n + *d as f64),
                    (None, _) => delta,
                    (Some(existing), _) => {
                        return Err(TalaDbError::TypeError {
                            expected: "numeric".into(),
                            got: existing.type_name().into(),
                        });
                    }
                };
                doc.set(k, new_val);
            }
        }
        Update::Push(key, val) => match doc.get(&key).cloned() {
            Some(Value::Array(mut arr)) => {
                arr.push(val);
                doc.set(key, Value::Array(arr));
            }
            None => {
                doc.set(key, Value::Array(vec![val]));
            }
            Some(existing) => {
                return Err(TalaDbError::TypeError {
                    expected: "array".into(),
                    got: existing.type_name().into(),
                });
            }
        },
        Update::Pull(key, val) => {
            if let Some(Value::Array(arr)) = doc.get(&key).cloned() {
                let filtered: Vec<Value> = arr.into_iter().filter(|v| v != &val).collect();
                doc.set(key, Value::Array(filtered));
            }
        }
    }
    // Auto-advance _changed_at on every mutation so LWW always has a fresh timestamp.
    doc.set("_changed_at", Value::Int(now_ms() as i64));
    Ok(())
}
