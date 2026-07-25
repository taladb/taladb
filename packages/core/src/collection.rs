use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ulid::Ulid;

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
    compound_table_name, docs_table_name, encode_compound_key, encode_index_key, index_table_name,
    meta_key, tomb_table_name,
};
use crate::query::executor::{execute, fetch_documents, index_ordered_entries};
use crate::query::filter::Filter;
use crate::query::options::{
    FindOptions, SortDirection, partial_sort_documents, project_document, sort_documents,
};
use crate::query::planner::plan_full;
use crate::sync::{SyncEvent, SyncHook, now_ms};
use crate::vector::{
    CachedVectors, HnswOptions, META_HNSW_TABLE, META_VECTOR_TABLE, SharedVectorCache, VectorDef,
    VectorMetric, VectorSearchResult, compute_similarity, decode_f32_vec, encode_f32_vec,
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

/// Provenance marker stamped on every [`WriteOrigin::AuthoritativeRemote`] write.
///
/// A document carrying this field came *from* an authoritative origin, so it must
/// never be replicated back out. `export_changes` skips it. Engine-owned: callers
/// cannot set it, and it is stripped from local writes.
pub const REMOTE_ORIGIN_FIELD: &str = "_remote";
/// Monotonic revision supplied by an authoritative origin. When both the stored
/// and incoming documents carry integer revisions, stale/equal replacements are
/// ignored atomically inside the write transaction.
pub const REMOTE_REVISION_FIELD: &str = "_remote_rev";

/// Who authored a write, and therefore whether it is replicated outward.
///
/// This is the *write-authority* axis. It exists because a document pulled from
/// a remote origin is **not** a local edit, and treating it as one corrupts
/// replication in two distinct ways — see [`Collection::replace_many_with_ids`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOrigin {
    /// A local, user-authored write. Stamps `_changed_at` from the wall clock and
    /// emits outbound [`SyncEvent`]s, so the change replicates to peers. This is
    /// what every existing write path does.
    Local,
    /// A write replicated *in* from an authoritative remote origin (e.g. a REST
    /// hydration or delta refresh). The origin already knows about these rows, so
    /// they must never be replicated back out.
    AuthoritativeRemote,
}

#[derive(Clone)]
pub enum Update {
    /// Apply multiple update operators atomically, in document order.
    Many(Vec<Update>),
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
    sync_hook: Option<Arc<dyn SyncHook>>,
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
        Collection {
            name: name.into(),
            backend,
            index_cache: new_shared_index_cache(),
            watch_registry: crate::watch::new_registry(),
            sync_hook: None,
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
    /// are decrypted like `find`, but drops hooks/audit (it never writes).
    fn clone_reader(&self) -> Collection {
        Collection {
            name: self.name.clone(),
            backend: Arc::clone(&self.backend),
            index_cache: Arc::clone(&self.index_cache),
            watch_registry: Arc::clone(&self.watch_registry),
            sync_hook: None,
            audit_caller: None,
            #[cfg(feature = "encryption")]
            field_encryption: self.field_encryption.clone(),
            vector_cache: Arc::clone(&self.vector_cache),
            #[cfg(feature = "vector-hnsw")]
            hnsw_cache: Arc::clone(&self.hnsw_cache),
        }
    }

    /// Attach a sync hook that receives a [`SyncEvent`] after every successful
    /// write commit.  Pass `Arc::new(NoopSyncHook)` to disable (default).
    pub fn with_sync_hook(mut self, hook: Arc<dyn SyncHook>) -> Self {
        self.sync_hook = Some(hook);
        self
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
        let mut guard = self.index_cache.lock().unwrap_or_else(|p| p.into_inner());
        guard.remove(&self.name);
    }

    /// Drop the decoded-vector cache entry for a field. Data writes invalidate
    /// via the write generation, but `create`/`drop_vector_index` rewrite the
    /// vec table without bumping it, so they evict explicitly.
    fn evict_vector_cache(&self, field: &str) {
        let key = format!("{}::{}", self.name, field);
        let mut cache = self.vector_cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.remove(&key);
    }

    fn load_indexes_cached(&self) -> Result<CachedIndexes, TalaDbError> {
        {
            let guard = self.index_cache.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(cached) = guard.get(&self.name) {
                return Ok(cached.clone());
            }
        }
        // Load outside the lock so a slow storage read does not block other
        // collections; a racing loader just overwrites with identical data.
        let cached = CachedIndexes {
            indexes: Arc::new(self.load_indexes()?),
            fts_indexes: Arc::new(self.load_fts_indexes()?),
            vec_indexes: Arc::new(self.load_vector_indexes()?),
            compound_indexes: Arc::new(self.load_compound_indexes()?),
        };
        let mut guard = self.index_cache.lock().unwrap_or_else(|p| p.into_inner());
        guard.insert(self.name.clone(), cached.clone());
        Ok(cached)
    }

    /// Ensure the `_changed_at` secondary index exists for this collection.
    ///
    /// Called automatically before every mutation so `export_changes` can use
    /// an index range scan instead of a full table scan.  `create_index` is
    /// idempotent, so this is a no-op after the first call.
    fn ensure_changed_at_index(&self) -> Result<(), TalaDbError> {
        // Consult the cached definitions first: opening a write transaction
        // on every mutation just to re-check existence is wasteful, and the
        // cache is invalidated by any index DDL on this Database.
        let cache = self.load_indexes_cached()?;
        if cache.indexes.iter().any(|d| d.field == "_changed_at") {
            return Ok(());
        }
        self.create_index("_changed_at")
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

        // Backfill existing documents into the new index
        let docs_table = docs_table_name(&self.name);
        let existing = wtxn.range(
            &docs_table,
            std::ops::Bound::Unbounded,
            std::ops::Bound::Unbounded,
        )?;
        let idx_table = index_table_name(&self.name, field);
        for (_, doc_bytes) in existing {
            let doc: Document = postcard::from_bytes(&doc_bytes)?;
            if let Some(val) = doc.get(field)
                && let Some(idx_key) = encode_index_key(val, doc.id)
            {
                wtxn.put(&idx_table, &idx_key, &[])?;
            }
        }

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
        for (k, _) in all_entries {
            wtxn.delete(&idx_table, &k)?;
        }

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
        for (_, doc_bytes) in existing {
            let doc: Document = postcard::from_bytes(&doc_bytes)?;
            if let Some(crate::document::Value::Str(text)) = doc.get(field) {
                let (freqs, doc_len) = token_frequencies(text);
                for (token, tf) in &freqs {
                    let fts_key = encode_fts_key(token, &doc.id);
                    wtxn.put(&fts_table, &fts_key, &encode_tf(*tf))?;
                }
                wtxn.put(&len_table, &doc.id.to_bytes(), &encode_doc_len(doc_len))?;
                stats.add_doc(doc_len);
            }
        }
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
            return Err(TalaDbError::IndexNotFound(format!("fts:{}", meta_key)));
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
            for (k, _) in all {
                wtxn.delete(&table, &k)?;
            }
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
            fields: fields.iter().map(|s| s.to_string()).collect(),
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
        for (_, doc_bytes) in existing {
            let doc: Document = postcard::from_bytes(&doc_bytes)?;
            let vals: Option<Vec<&crate::document::Value>> =
                fields.iter().map(|f| doc.get(f)).collect();
            if let Some(v) = vals
                && let Some(key) = encode_compound_key(&v, doc.id)
            {
                wtxn.put(&ctable, &key, &[])?;
            }
        }

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
            return Err(TalaDbError::IndexNotFound(format!("compound:{}", meta_key)));
        }

        let ctable = compound_table_name(&self.name, fields);
        let all = wtxn.range(
            &ctable,
            std::ops::Bound::Unbounded,
            std::ops::Bound::Unbounded,
        )?;
        for (k, _) in all {
            wtxn.delete(&ctable, &k)?;
        }
        wtxn.delete(META_COMPOUND_TABLE, meta_key.as_bytes())?;
        wtxn.commit()?;
        self.invalidate_index_cache();
        Ok(())
    }

    fn load_compound_indexes(&self) -> Result<Vec<CompoundIndexDef>, TalaDbError> {
        let rtxn = self.backend.begin_read()?;
        let prefix = format!("{}::", self.name);
        let all = rtxn.scan_all(META_COMPOUND_TABLE)?;
        let mut defs = Vec::new();
        for (k, v) in all {
            let key_str = String::from_utf8_lossy(&k);
            if key_str.starts_with(&prefix) {
                let def: CompoundIndexDef = postcard::from_bytes(&v)?;
                defs.push(def);
            }
        }
        Ok(defs)
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
            metric: resolved_metric.clone(),
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
        let mut backfill: Vec<(ulid::Ulid, Vec<f32>)> = Vec::new();
        for (_, doc_bytes) in existing {
            let doc: Document = postcard::from_bytes(&doc_bytes)?;
            if let Some(val) = doc.get(field)
                && let Some(vec) = value_to_f32_vec(val)
                && vec.len() == dimensions
            {
                wtxn.put(&vtable, &doc.id.to_bytes(), &encode_f32_vec(&vec))?;
                backfill.push((doc.id, vec));
            }
        }

        // Persist HNSW options and build the in-memory graph when requested
        if let Some(hnsw_opts) = hnsw {
            let hnsw_meta_key = format!("{}::{}", self.name, field);
            let opts_bytes = postcard::to_allocvec(&hnsw_opts)?;
            wtxn.put(META_HNSW_TABLE, hnsw_meta_key.as_bytes(), &opts_bytes)?;

            #[cfg(feature = "vector-hnsw")]
            {
                let graph = build_hnsw(&backfill, &resolved_metric, hnsw_opts.ef_construction)?;
                let cache_key = format!("{}::{}", self.name, field);
                let mut cache = self.hnsw_cache.lock().unwrap_or_else(|p| p.into_inner());
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
        for (k, _) in all {
            wtxn.delete(&vtable, &k)?;
        }

        // Remove HNSW metadata (if present) and evict from in-memory cache
        let hnsw_meta_key = format!("{}::{}", self.name, field);
        let _ = wtxn.delete(META_HNSW_TABLE, hnsw_meta_key.as_bytes());
        #[cfg(feature = "vector-hnsw")]
        {
            let cache_key = format!("{}::{}", self.name, field);
            let mut cache = self.hnsw_cache.lock().unwrap_or_else(|p| p.into_inner());
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
                let cache = self.hnsw_cache.lock().unwrap_or_else(|p| p.into_inner());
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
                let cache = self.vector_cache.lock().unwrap_or_else(|p| p.into_inner());
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
                    let mut cache = self.vector_cache.lock().unwrap_or_else(|p| p.into_inner());
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
        let metric = &def.metric;
        let mut scored: Vec<(ulid::Ulid, f32)> = Vec::with_capacity(vectors.len());
        for (id, vec) in vectors.iter() {
            if let Some(ids) = &id_filter
                && !ids.contains(id)
            {
                continue;
            }
            scored.push((*id, compute_similarity(metric, query, vec)));
        }

        // 6. Select the top_k by score. `select_nth_unstable` partitions in
        //    O(n) average instead of the O(n log n) of a full sort, then only
        //    the k retained results are sorted.
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
            let mut cache = self.hnsw_cache.lock().unwrap_or_else(|p| p.into_inner());
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
        let prefix = format!("{}::", self.name);
        let all = rtxn.scan_all(META_VECTOR_TABLE)?;
        let mut defs = Vec::new();
        for (k, v) in all {
            let key_str = String::from_utf8_lossy(&k);
            if key_str.starts_with(&prefix) {
                let def: VectorDef = postcard::from_bytes(&v)?;
                defs.push(def);
            }
        }
        Ok(defs)
    }

    fn load_fts_indexes(&self) -> Result<Vec<FtsDef>, TalaDbError> {
        let rtxn = self.backend.begin_read()?;
        let prefix = format!("{}::", self.name);
        let all = rtxn.scan_all(META_FTS_TABLE)?;
        let mut defs = Vec::new();
        for (k, v) in all {
            let key_str = String::from_utf8_lossy(&k);
            if key_str.starts_with(&prefix) {
                // Tolerates pre-versioning metadata; see `decode_fts_def`.
                defs.push(decode_fts_def(&v)?);
            }
        }
        Ok(defs)
    }

    fn load_indexes(&self) -> Result<Vec<IndexDef>, TalaDbError> {
        let rtxn = self.backend.begin_read()?;
        let prefix = format!("{}::", self.name);
        // Scan meta table and filter by collection prefix
        let all = rtxn.scan_all(META_INDEXES_TABLE)?;
        let mut defs = Vec::new();
        for (k, v) in all {
            let key_str = String::from_utf8_lossy(&k);
            if key_str.starts_with(&prefix) {
                let def: IndexDef = postcard::from_bytes(&v)?;
                defs.push(def);
            }
        }
        Ok(defs)
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
        let docs_table = docs_table_name(&self.name);
        let doc_bytes = postcard::to_allocvec(doc)?;
        wtxn.put(&docs_table, &doc.id.to_bytes(), &doc_bytes)?;

        // Secondary indexes
        for idx in cache.indexes.iter() {
            let idx_table = index_table_name(&self.name, &idx.field);
            if let Some(old) = old_doc
                && let Some(old_val) = old.get(&idx.field)
                && let Some(old_key) = encode_index_key(old_val, old.id)
            {
                wtxn.delete(&idx_table, &old_key)?;
            }
            if let Some(new_val) = doc.get(&idx.field)
                && let Some(idx_key) = encode_index_key(new_val, doc.id)
            {
                wtxn.put(&idx_table, &idx_key, &[])?;
            }
        }

        // FTS indexes
        for fts in cache.fts_indexes.iter() {
            let fts_table = fts_table_name(&self.name, &fts.field);
            let len_table = fts_len_table_name(&self.name, &fts.field);
            let stats_table = fts_stats_table_name(&self.name, &fts.field);
            let mut removed_len: Option<u32> = None;
            let mut added_len: Option<u32> = None;

            // Remove old tokens
            if let Some(old) = old_doc
                && let Some(crate::document::Value::Str(old_text)) = old.get(&fts.field)
            {
                for token in tokenize(old_text) {
                    let key = encode_fts_key(&token, &old.id);
                    wtxn.delete(&fts_table, &key)?;
                }
                // Trust the recorded length over a re-tokenization of the old
                // text: if the tokenizer ever changes, the stored value is
                // what the running totals were actually built from.
                let stored = wtxn
                    .get(&len_table, &old.id.to_bytes())?
                    .map(|b| decode_doc_len(&b));
                removed_len = Some(stored.unwrap_or_else(|| token_frequencies(old_text).1));
                wtxn.delete(&len_table, &old.id.to_bytes())?;
            }

            // Write new tokens
            if let Some(crate::document::Value::Str(new_text)) = doc.get(&fts.field) {
                let (freqs, doc_len) = token_frequencies(new_text);
                for (token, tf) in &freqs {
                    let key = encode_fts_key(token, &doc.id);
                    wtxn.put(&fts_table, &key, &encode_tf(*tf))?;
                }
                wtxn.put(&len_table, &doc.id.to_bytes(), &encode_doc_len(doc_len))?;
                added_len = Some(doc_len);
            }

            if removed_len.is_some() || added_len.is_some() {
                adjust_fts_stats(wtxn, &stats_table, removed_len, added_len)?;
            }
        }

        // Vector indexes
        for vdef in cache.vec_indexes.iter() {
            let vtable = vec_table_name(&self.name, &vdef.field);
            // Remove old vector entry if updating
            if old_doc.is_some() {
                wtxn.delete(&vtable, &doc.id.to_bytes())?;
            }
            // Write new vector if field is present and is a valid numeric array
            if let Some(val) = doc.get(&vdef.field)
                && let Some(vec) = value_to_f32_vec(val)
                && vec.len() == vdef.dimensions
            {
                wtxn.put(&vtable, &doc.id.to_bytes(), &encode_f32_vec(&vec))?;
            }
        }

        // Compound indexes
        for cidx in cache.compound_indexes.iter() {
            let field_refs: Vec<&str> = cidx.fields.iter().map(|s| s.as_str()).collect();
            let ctable = compound_table_name(&self.name, &field_refs);
            // Remove old compound entry
            if let Some(old) = old_doc {
                let old_vals: Option<Vec<&Value>> = field_refs.iter().map(|f| old.get(f)).collect();
                if let Some(v) = old_vals
                    && let Some(old_key) = encode_compound_key(&v, old.id)
                {
                    wtxn.delete(&ctable, &old_key)?;
                }
            }
            // Write new compound entry
            let new_vals: Option<Vec<&Value>> = field_refs.iter().map(|f| doc.get(f)).collect();
            if let Some(v) = new_vals
                && let Some(new_key) = encode_compound_key(&v, doc.id)
            {
                wtxn.put(&ctable, &new_key, &[])?;
            }
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // Public API
    // ------------------------------------------------------------------

    pub fn insert(&self, mut fields: Vec<(String, Value)>) -> Result<Ulid, TalaDbError> {
        // Auto-stamp _changed_at so LWW merge works correctly without manual calls.
        fields.retain(|(k, _)| {
            k != "_changed_at" && k != REMOTE_ORIGIN_FIELD && k != REMOTE_REVISION_FIELD
        });
        fields.push(("_changed_at".into(), Value::Int(now_ms() as i64)));
        let mut doc = Document::new(fields);
        // Encrypt nominated fields before writing indexes or the doc body.
        // Index entries are written from the plaintext doc, so encrypted fields
        // are not indexable (intentional — see `with_field_encryption` docs).
        self.encrypt_doc(&mut doc)?;
        self.ensure_changed_at_index()?;
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
        if let Some(hook) = &self.sync_hook {
            hook.on_event(SyncEvent::Insert {
                collection: self.name.clone(),
                id: id.to_string(),
                document: doc,
            });
        }
        Ok(id)
    }

    pub fn insert_many(&self, items: Vec<Vec<(String, Value)>>) -> Result<Vec<Ulid>, TalaDbError> {
        let ts = now_ms() as i64;
        let mut docs: Vec<Document> = items
            .into_iter()
            .map(|mut fields| {
                fields.retain(|(k, _)| {
                    k != "_changed_at" && k != REMOTE_ORIGIN_FIELD && k != REMOTE_REVISION_FIELD
                });
                fields.push(("_changed_at".into(), Value::Int(ts)));
                Document::new(fields)
            })
            .collect();
        for doc in &mut docs {
            self.encrypt_doc(doc)?;
        }
        let cache = self.load_indexes_cached()?;
        let mut wtxn = self.backend.begin_write()?;
        let mut ids = Vec::with_capacity(docs.len());
        for doc in &docs {
            self.write_doc_and_indexes_with_compound(doc, None, &cache, wtxn.as_mut())?;
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
        if let Some(hook) = &self.sync_hook {
            for doc in &docs {
                hook.on_event(SyncEvent::Insert {
                    collection: self.name.clone(),
                    id: doc.id.to_string(),
                    document: doc.clone(),
                });
            }
        }
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

    pub fn find_one(&self, filter: Filter) -> Result<Option<Document>, TalaDbError> {
        Ok(self.find(filter)?.into_iter().next())
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
        let mut docs = execute(&qplan, &filter, rtxn.as_ref(), &self.name, deadline)?;
        // Decrypt encrypted fields before sorting/projecting.
        docs = self.decrypt_docs(docs)?;

        // Sort — if both sort and limit are set, use a partial sort (O(n + k log k))
        // which is dramatically faster than the O(n log n) full sort when
        // `skip + limit` is small compared to the candidate set.
        if !options.sort.is_empty() {
            if let Some(limit) = options.limit {
                let keep = (options.skip as usize).saturating_add(limit as usize);
                partial_sort_documents(&mut docs, &options.sort, keep);
            } else {
                sort_documents(&mut docs, &options.sort);
            }
        }

        // Skip
        if options.skip > 0 {
            let skip = options.skip as usize;
            if skip >= docs.len() {
                return Ok(vec![]);
            }
            docs.drain(..skip);
        }

        // Limit
        if let Some(limit) = options.limit {
            docs.truncate(limit as usize);
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
        if let Some(hook) = &self.sync_hook {
            hook.on_event(SyncEvent::Insert {
                collection: self.name.clone(),
                id: id.to_string(),
                document: doc,
            });
        }
        Ok(id)
    }

    /// Insert or replace a document preserving its ULID, in a single write
    /// transaction.
    ///
    /// Unlike `delete_by_id` followed by `insert_with_id`, this:
    /// - maintains secondary/FTS/vector/compound indexes against the previous
    ///   version of the document,
    /// - does **not** write a delete tombstone, and removes any existing
    ///   tombstone for the ID — so a replaced document cannot later be
    ///   exported to peers as a deletion,
    /// - is atomic (no window where the document is absent).
    ///
    /// Used by the sync adapters to apply remote upserts and merges.
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
        // Clear any tombstone: this ID is alive again. Without this, a
        // replace performed during sync import would leave a tombstone newer
        // than the document and the next export would delete it on peers.
        wtxn.delete(&tomb_table_name(&self.name), &id.to_bytes())?;
        wtxn.commit()?;
        crate::watch::notify(&self.watch_registry);
        if let Some(hook) = &self.sync_hook {
            hook.on_event(SyncEvent::Insert {
                collection: self.name.clone(),
                id: id.to_string(),
                document: doc,
            });
        }
        Ok(id)
    }

    /// Upsert many documents **by caller-supplied id**, in a single commit.
    ///
    /// This is the write primitive behind replication from a remote origin. It is
    /// [`Self::replace_with_id`]'s semantics (idempotent upsert, indexes maintained
    /// against the *old* version, tombstone cleared) batched into `insert_many`'s
    /// single-transaction shape. Existing rows are replaced in place; absent rows
    /// are created. Rows not named in `docs` are untouched — so hydrating page 2
    /// never disturbs page 1.
    ///
    /// # Why the origin matters
    ///
    /// Writing remote rows as if they were local edits corrupts replication two
    /// separate ways, and [`WriteOrigin::AuthoritativeRemote`] closes both:
    ///
    /// 1. **The push hook.** [`Self::replace_with_id`] emits a [`SyncEvent`], which
    ///    the HTTP sync hook fires at the server. Replaying the origin's own rows
    ///    back at it is at best wasted traffic and at worst a write loop.
    /// 2. **The export scan.** `export_changes` does *not* consult the sync hook —
    ///    it scans the collection directly, and when the cursor is `0` (which is
    ///    every pass today, since cursors are stubbed) it exports **everything**
    ///    via `find(Filter::All)`. Suppressing events is therefore not enough on
    ///    its own. Remote rows are stamped [`REMOTE_ORIGIN_FIELD`] and
    ///    `export_changes` skips them, which makes "remote rows never replicate
    ///    outward" an invariant of the storage layer rather than a property some
    ///    caller has to remember. (See [`Self::delete_many_with_ids`] for the
    ///    matching guard on deletions.)
    ///
    /// Row *version* for a remote write is the origin's business: the caller keeps
    /// whatever revision field the origin supplied (`_rev`, say). We deliberately
    /// do not invent one from the local clock — ordering two responses by the time
    /// they happened to arrive is exactly the bug that lets a stale fetch overwrite
    /// a fresh one. `_changed_at` is stripped for the same reason.
    ///
    /// Fires **one** watch notification for the whole batch: a 500-row hydration
    /// page must not wake every live query 500 times.
    pub fn replace_many_with_ids(
        &self,
        docs: Vec<Document>,
        origin: WriteOrigin,
    ) -> Result<Vec<Ulid>, TalaDbError> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let ts = now_ms() as i64;
        let mut docs: Vec<Document> = docs
            .into_iter()
            .map(|mut doc| {
                // Both fields are engine-owned provenance: never let a caller (or a
                // remote payload that happens to carry these keys) set them.
                doc.fields
                    .retain(|(k, _)| k != "_changed_at" && k != REMOTE_ORIGIN_FIELD);
                match origin {
                    WriteOrigin::Local => {
                        doc.remove(REMOTE_REVISION_FIELD);
                        doc.fields.push(("_changed_at".into(), Value::Int(ts)));
                    }
                    WriteOrigin::AuthoritativeRemote => {
                        doc.fields
                            .push((REMOTE_ORIGIN_FIELD.into(), Value::Bool(true)));
                    }
                }
                doc
            })
            .collect();
        // Symmetric with the find/find_by_id read paths, which decrypt.
        for doc in &mut docs {
            self.encrypt_doc(doc)?;
        }
        if origin == WriteOrigin::Local {
            self.ensure_changed_at_index()?;
        }
        let cache = self.load_indexes_cached()?;
        let docs_table = docs_table_name(&self.name);
        let tomb_table = tomb_table_name(&self.name);

        let mut wtxn = self.backend.begin_write()?;
        let mut ids = Vec::with_capacity(docs.len());
        for doc in &docs {
            let id = doc.id;
            // Read the previous version inside the write txn so old index entries
            // are computed from the bytes actually being replaced.
            let old_doc: Option<Document> = match wtxn.get(&docs_table, &id.to_bytes())? {
                Some(bytes) => Some(postcard::from_bytes(&bytes)?),
                None => None,
            };
            if origin == WriteOrigin::AuthoritativeRemote
                && let (Some(Value::Int(incoming)), Some(Value::Int(stored))) = (
                    doc.get(REMOTE_REVISION_FIELD),
                    old_doc.as_ref().and_then(|d| d.get(REMOTE_REVISION_FIELD)),
                )
                && incoming <= stored
            {
                continue;
            }
            self.write_doc_and_indexes_with_compound(doc, old_doc.as_ref(), &cache, wtxn.as_mut())?;
            // Clear any tombstone: this id is alive again. Without this, a replace
            // would leave a tombstone newer than the document, and the next export
            // would delete it on peers.
            wtxn.delete(&tomb_table, &id.to_bytes())?;
            // Audit rows commit atomically with the batch.
            if let Some(caller) = &self.audit_caller {
                write_audit_entry(
                    wtxn.as_mut(),
                    &self.name,
                    AuditOp::Insert,
                    &id.to_string(),
                    caller,
                )?;
            }
            ids.push(id);
        }
        wtxn.commit()?;
        crate::watch::notify(&self.watch_registry);

        if origin == WriteOrigin::Local
            && let Some(hook) = &self.sync_hook
        {
            for doc in docs {
                hook.on_event(SyncEvent::Insert {
                    collection: self.name.clone(),
                    id: doc.id.to_string(),
                    document: doc,
                });
            }
        }
        Ok(ids)
    }

    /// Delete many documents **by id**, in a single commit. Returns how many were
    /// actually present and removed; unknown ids are silently skipped.
    ///
    /// This is the delete half of remote replication: a delta refresh reports the
    /// origin's deleted primary keys, which the caller maps through
    /// [`crate::derive_doc_id`] to reach the local rows.
    ///
    /// [`WriteOrigin::AuthoritativeRemote`] deletes write **no tombstone** and emit
    /// no sync events, for the same reason remote upserts are marked
    /// [`REMOTE_ORIGIN_FIELD`]: `export_changes` scans the tombstone table directly
    /// and cannot filter it by provenance (the document is gone), so a tombstone
    /// here would push the origin's own deletion straight back at it.
    ///
    /// Fires one watch notification for the whole batch.
    pub fn delete_many_with_ids(
        &self,
        ids: &[Ulid],
        origin: WriteOrigin,
    ) -> Result<usize, TalaDbError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let cache = self.load_indexes_cached()?;
        let docs_table = docs_table_name(&self.name);
        let deleted_at = match origin {
            WriteOrigin::Local => Some(now_ms()),
            WriteOrigin::AuthoritativeRemote => None,
        };

        let mut wtxn = self.backend.begin_write()?;
        let mut removed = Vec::new();
        for &id in ids {
            // Read inside the write txn so index cleanup is computed from the bytes
            // actually being removed.
            let doc: Option<Document> = match wtxn.get(&docs_table, &id.to_bytes())? {
                Some(bytes) => Some(postcard::from_bytes(&bytes)?),
                None => None,
            };
            let Some(doc) = doc else { continue };
            self.delete_doc_and_indexes_inner(&doc, &cache, wtxn.as_mut(), deleted_at)?;
            if let Some(caller) = &self.audit_caller {
                write_audit_entry(
                    wtxn.as_mut(),
                    &self.name,
                    AuditOp::Delete,
                    &id.to_string(),
                    caller,
                )?;
            }
            removed.push(id);
        }
        wtxn.commit()?;
        crate::watch::notify(&self.watch_registry);

        if origin == WriteOrigin::Local
            && let Some(hook) = &self.sync_hook
        {
            for id in &removed {
                hook.on_event(SyncEvent::Delete {
                    collection: self.name.clone(),
                    id: id.to_string(),
                });
            }
        }
        Ok(removed.len())
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
        self.delete_by_id_at(id, now_ms())
    }

    /// Delete a document while preserving the timestamp of the deletion.
    /// Used by sync import so forwarding a remote tombstone does not make an
    /// old deletion appear to have happened at local receipt time.
    pub(crate) fn delete_by_id_at(&self, id: Ulid, deleted_at: u64) -> Result<bool, TalaDbError> {
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
                self.delete_doc_and_indexes_with_compound_at(
                    &doc,
                    &cache,
                    wtxn.as_mut(),
                    deleted_at,
                )?;
                wtxn.commit()?;
                crate::watch::notify(&self.watch_registry);
                if let Some(hook) = &self.sync_hook {
                    hook.on_event(SyncEvent::Delete {
                        collection: self.name.clone(),
                        id: id.to_string(),
                    });
                }
                Ok(true)
            }
        }
    }

    pub fn update_one(&self, filter: Filter, update: Update) -> Result<bool, TalaDbError> {
        self.ensure_changed_at_index()?;
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
            apply_update(&mut new_doc, update.clone())?;
            // A user edit makes this a local document again. Leaving `_remote`
            // behind would make export_changes silently suppress the edit.
            new_doc.remove(REMOTE_ORIGIN_FIELD);
            new_doc.remove(REMOTE_REVISION_FIELD);
            let (changes, removed) = if self.sync_hook.is_some() {
                diff_documents(&old_doc, &new_doc)
            } else {
                (HashMap::new(), Vec::new())
            };
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
            if let Some(hook) = &self.sync_hook {
                hook.on_event(SyncEvent::Update {
                    collection: self.name.clone(),
                    id: old_doc.id.to_string(),
                    changes,
                    removed,
                });
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub fn update_many(&self, filter: Filter, update: Update) -> Result<u64, TalaDbError> {
        self.ensure_changed_at_index()?;
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
        let has_hook = self.sync_hook.is_some();
        let mut events: Vec<SyncEvent> = if has_hook {
            Vec::with_capacity(candidates.len())
        } else {
            Vec::new()
        };
        // Re-fetch and re-check every candidate inside the write transaction:
        // the read snapshot used to gather them is already released, and a
        // concurrent writer may have changed or deleted them in between.
        let regex_cache = filter.compile_regex_cache()?;
        let docs_table = docs_table_name(&self.name);
        let mut wtxn = self.backend.begin_write()?;
        for candidate in &candidates {
            let stored_old: Document = match wtxn.get(&docs_table, &candidate.id.to_bytes())? {
                Some(bytes) => postcard::from_bytes(&bytes)?,
                None => continue,
            };
            if !filter.matches_with_cache(&stored_old, &regex_cache) {
                continue;
            }
            let mut plain_old = stored_old.clone();
            self.decrypt_doc(&mut plain_old)?;
            let mut new_doc = plain_old.clone();
            apply_update(&mut new_doc, update.clone())?;
            new_doc.remove(REMOTE_ORIGIN_FIELD);
            new_doc.remove(REMOTE_REVISION_FIELD);
            if has_hook {
                let (changes, removed) = diff_documents(&plain_old, &new_doc);
                events.push(SyncEvent::Update {
                    collection: self.name.clone(),
                    id: stored_old.id.to_string(),
                    changes,
                    removed,
                });
            }
            self.encrypt_doc(&mut new_doc)?;
            self.write_doc_and_indexes_with_compound(
                &new_doc,
                Some(&stored_old),
                &cache,
                wtxn.as_mut(),
            )?;
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
            count += 1;
        }
        wtxn.commit()?;
        crate::watch::notify(&self.watch_registry);
        if let Some(hook) = &self.sync_hook {
            for event in events {
                hook.on_event(event);
            }
        }
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
            if let Some(hook) = &self.sync_hook {
                hook.on_event(SyncEvent::Delete {
                    collection: self.name.clone(),
                    id: doc_id.clone(),
                });
            }
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
            self.delete_doc_and_indexes_with_compound(&current, &cache, wtxn.as_mut())?;
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
        wtxn.commit()?;
        crate::watch::notify(&self.watch_registry);
        if let Some(hook) = &self.sync_hook {
            for doc in &deleted {
                hook.on_event(SyncEvent::Delete {
                    collection: self.name.clone(),
                    id: doc.id.to_string(),
                });
            }
        }
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
        // only the match count matters, not field contents.
        let cache = self.load_indexes_cached()?;
        let qplan = plan_full(
            &filter,
            &cache.indexes,
            &cache.fts_indexes,
            &cache.compound_indexes,
        );
        let rtxn = self.backend.begin_read()?;
        let docs = execute(&qplan, &filter, rtxn.as_ref(), &self.name, None)?;
        Ok(docs.len() as u64)
    }

    /// Remove tombstones older than `before_ms` (milliseconds since Unix epoch).
    ///
    /// Tombstones record deleted document IDs so deletions can propagate via the
    /// sync changeset API.  Once all replicas are known to have received a
    /// deletion (i.e. after your retention window has elapsed), those tombstones
    /// can be safely pruned to reclaim storage.
    ///
    /// Returns the number of tombstones removed.
    ///
    /// # Example
    /// ```ignore
    /// // Prune tombstones older than 30 days
    /// let cutoff = now_ms() - 30 * 24 * 60 * 60 * 1000;
    /// let pruned = collection.compact_tombstones(cutoff)?;
    /// ```
    pub fn compact_tombstones(&self, before_ms: u64) -> Result<u64, TalaDbError> {
        let tomb_table = tomb_table_name(&self.name);
        let rtxn = self.backend.begin_read()?;
        let all = rtxn.scan_all(&tomb_table)?;

        // Collect candidate IDs whose tombstone timestamp is older than before_ms.
        let candidates: Vec<Vec<u8>> = all
            .into_iter()
            .filter_map(|(key_bytes, val_bytes)| {
                let ts: i64 = postcard::from_bytes(&val_bytes).ok()?;
                if (ts as u64) < before_ms {
                    Some(key_bytes)
                } else {
                    None
                }
            })
            .collect();

        if candidates.is_empty() {
            return Ok(0);
        }

        drop(rtxn);

        // Re-check each candidate inside the write txn: between the read and
        // the write another writer may have resurrected the document (delete
        // → insert with new ULID reusing the key) or overwritten the tombstone
        // with a newer timestamp. Only prune entries whose stored timestamp
        // still satisfies the cutoff at commit time.
        let mut wtxn = self.backend.begin_write()?;
        let mut count: u64 = 0;
        for key in &candidates {
            let still_eligible = match wtxn.get(&tomb_table, key)? {
                Some(bytes) => postcard::from_bytes::<i64>(&bytes)
                    .map(|ts| (ts as u64) < before_ms)
                    .unwrap_or(false),
                None => false,
            };
            if still_eligible {
                wtxn.delete(&tomb_table, key)?;
                count += 1;
            }
        }
        wtxn.commit()?;
        Ok(count)
    }

    fn delete_doc_and_indexes_with_compound(
        &self,
        doc: &Document,
        cache: &CachedIndexes,
        wtxn: &mut dyn crate::engine::WriteTxn,
    ) -> Result<(), TalaDbError> {
        self.delete_doc_and_indexes_with_compound_at(doc, cache, wtxn, now_ms())
    }

    fn delete_doc_and_indexes_with_compound_at(
        &self,
        doc: &Document,
        cache: &CachedIndexes,
        wtxn: &mut dyn crate::engine::WriteTxn,
        deleted_at: u64,
    ) -> Result<(), TalaDbError> {
        self.delete_doc_and_indexes_inner(doc, cache, wtxn, Some(deleted_at))
    }

    /// `deleted_at: None` deletes the document **without** writing a tombstone.
    ///
    /// Only [`WriteOrigin::AuthoritativeRemote`] deletes do this. A tombstone
    /// exists to tell peers about a *local* deletion; the origin already knows it
    /// deleted the row, and `export_changes` scans the tombstone table directly
    /// (it cannot filter by document provenance, because the document is gone).
    /// So writing one here would push the origin's own deletion back at it.
    fn delete_doc_and_indexes_inner(
        &self,
        doc: &Document,
        cache: &CachedIndexes,
        wtxn: &mut dyn crate::engine::WriteTxn,
        deleted_at: Option<u64>,
    ) -> Result<(), TalaDbError> {
        let docs_table = docs_table_name(&self.name);
        wtxn.delete(&docs_table, &doc.id.to_bytes())?;

        // Write a tombstone so this deletion can be exported via SyncAdapter
        // and propagated to remote replicas that may not have received the
        // HTTP push event.
        if let Some(deleted_at) = deleted_at {
            let tomb_table = tomb_table_name(&self.name);
            let deleted_at = i64::try_from(deleted_at).map_err(|_| {
                TalaDbError::InvalidOperation("deletion timestamp exceeds i64::MAX".into())
            })?;
            let ts_bytes = postcard::to_allocvec(&deleted_at)?;
            wtxn.put(&tomb_table, &doc.id.to_bytes(), &ts_bytes)?;
        }

        for idx in cache.indexes.iter() {
            let idx_table = index_table_name(&self.name, &idx.field);
            if let Some(val) = doc.get(&idx.field)
                && let Some(idx_key) = encode_index_key(val, doc.id)
            {
                wtxn.delete(&idx_table, &idx_key)?;
            }
        }

        for fts in cache.fts_indexes.iter() {
            let fts_table = fts_table_name(&self.name, &fts.field);
            let len_table = fts_len_table_name(&self.name, &fts.field);
            if let Some(crate::document::Value::Str(text)) = doc.get(&fts.field) {
                for token in tokenize(text) {
                    let key = encode_fts_key(&token, &doc.id);
                    wtxn.delete(&fts_table, &key)?;
                }
                let stored = wtxn
                    .get(&len_table, &doc.id.to_bytes())?
                    .map(|b| decode_doc_len(&b));
                let removed = stored.unwrap_or_else(|| token_frequencies(text).1);
                wtxn.delete(&len_table, &doc.id.to_bytes())?;
                let stats_table = fts_stats_table_name(&self.name, &fts.field);
                adjust_fts_stats(wtxn, &stats_table, Some(removed), None)?;
            }
        }

        for vdef in cache.vec_indexes.iter() {
            let vtable = vec_table_name(&self.name, &vdef.field);
            wtxn.delete(&vtable, &doc.id.to_bytes())?;
        }

        for cidx in cache.compound_indexes.iter() {
            let field_refs: Vec<&str> = cidx.fields.iter().map(|s| s.as_str()).collect();
            let ctable = compound_table_name(&self.name, &field_refs);
            let vals: Option<Vec<&Value>> = field_refs.iter().map(|f| doc.get(f)).collect();
            if let Some(v) = vals
                && let Some(key) = encode_compound_key(&v, doc.id)
            {
                wtxn.delete(&ctable, &key)?;
            }
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
fn adjust_fts_stats(
    wtxn: &mut dyn crate::engine::WriteTxn,
    stats_table: &str,
    removed_len: Option<u32>,
    added_len: Option<u32>,
) -> Result<(), TalaDbError> {
    let mut stats: FtsStats = match wtxn.get(stats_table, FTS_STATS_KEY)? {
        // A corrupt or truncated stats record must not fail the write; the
        // worst case is a slightly wrong average until the index is rebuilt.
        Some(bytes) => postcard::from_bytes(&bytes).unwrap_or_default(),
        None => FtsStats::default(),
    };
    if let Some(len) = removed_len {
        stats.remove_doc(len);
    }
    if let Some(len) = added_len {
        stats.add_doc(len);
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

/// Compute the diff between `old` and `new`.
///
/// Returns `(changes, removed)`:
/// - A field present in `new` but not in `old` → in `changes` with the new value.
/// - A field present in both with a different value → in `changes` with the new value.
/// - A field present in `old` but absent from `new` → listed in `removed`, and
///   also in `changes` with `Value::Null` for backward compatibility with
///   receivers that predate the explicit removed list.
/// - Unchanged fields appear in neither.
fn diff_documents(old: &Document, new: &Document) -> (HashMap<String, Value>, Vec<String>) {
    let mut changes = HashMap::new();
    for (k, new_val) in &new.fields {
        match old.get(k) {
            None => {
                changes.insert(k.clone(), new_val.clone());
            }
            Some(old_val) if old_val != new_val => {
                changes.insert(k.clone(), new_val.clone());
            }
            _ => {}
        }
    }
    // Fields removed from old doc
    let mut removed = Vec::new();
    for (k, _) in &old.fields {
        if new.get(k).is_none() {
            changes.insert(k.clone(), Value::Null);
            removed.push(k.clone());
        }
    }
    (changes, removed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::Database;
    use crate::sync::RecordingSyncHook;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn hooked(db: &Database, name: &str) -> (Collection, Arc<RecordingSyncHook>) {
        let hook = Arc::new(RecordingSyncHook::new());
        let col = db
            .collection(name)
            .unwrap()
            .with_sync_hook(Arc::clone(&hook) as Arc<dyn SyncHook>);
        (col, hook)
    }

    // ── insert ───────────────────────────────────────────────────────────────

    #[test]
    fn insert_fires_insert_event() {
        let db = db();
        let (col, hook) = hooked(&db, "items");
        let id = col
            .insert(vec![("name".into(), Value::Str("Alice".into()))])
            .unwrap();
        let events = hook.take();
        assert_eq!(events.len(), 1);
        match &events[0] {
            SyncEvent::Insert {
                collection,
                id: eid,
                document,
            } => {
                assert_eq!(collection, "items");
                assert_eq!(eid, &id.to_string());
                assert_eq!(document.get("name"), Some(&Value::Str("Alice".into())));
            }
            other => panic!("expected Insert, got {other:?}"),
        }
    }

    #[test]
    fn no_hook_insert_no_panic() {
        let db = db();
        let col = db.collection("items").unwrap();
        assert!(col.insert(vec![("x".into(), Value::Int(1))]).is_ok());
    }

    // ── insert_many ──────────────────────────────────────────────────────────

    #[test]
    fn insert_many_fires_one_event_per_doc() {
        let db = db();
        let (col, hook) = hooked(&db, "items");
        let ids = col
            .insert_many(vec![
                vec![("n".into(), Value::Int(1))],
                vec![("n".into(), Value::Int(2))],
                vec![("n".into(), Value::Int(3))],
            ])
            .unwrap();
        let events = hook.take();
        assert_eq!(events.len(), 3);
        for (i, event) in events.iter().enumerate() {
            match event {
                SyncEvent::Insert { id, .. } => assert_eq!(id, &ids[i].to_string()),
                other => panic!("expected Insert, got {other:?}"),
            }
        }
    }

    // ── update_one ───────────────────────────────────────────────────────────

    #[test]
    fn update_one_fires_update_event_with_delta() {
        let db = db();
        let (col, hook) = hooked(&db, "items");
        col.insert(vec![
            ("name".into(), Value::Str("Alice".into())),
            ("score".into(), Value::Int(10)),
        ])
        .unwrap();
        hook.take(); // discard Insert event

        col.update_one(
            Filter::Eq("name".into(), Value::Str("Alice".into())),
            Update::Set(vec![("score".into(), Value::Int(20))]),
        )
        .unwrap();

        let events = hook.take();
        assert_eq!(events.len(), 1);
        match &events[0] {
            SyncEvent::Update {
                collection,
                changes,
                ..
            } => {
                assert_eq!(collection, "items");
                // Only the changed field
                assert_eq!(changes.get("score"), Some(&Value::Int(20)));
                // Unchanged field not present
                assert!(!changes.contains_key("name"));
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    #[test]
    fn update_one_no_match_fires_no_event() {
        let db = db();
        let (col, hook) = hooked(&db, "items");
        col.update_one(
            Filter::Eq("missing".into(), Value::Bool(true)),
            Update::Set(vec![("x".into(), Value::Int(1))]),
        )
        .unwrap();
        assert_eq!(hook.len(), 0);
    }

    #[test]
    fn update_diff_includes_removed_field_as_null() {
        let db = db();
        let (col, hook) = hooked(&db, "items");
        col.insert(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(2)),
        ])
        .unwrap();
        hook.take();

        col.update_one(
            Filter::Eq("a".into(), Value::Int(1)),
            Update::Unset(vec!["b".into()]),
        )
        .unwrap();

        let events = hook.take();
        match &events[0] {
            SyncEvent::Update { changes, .. } => {
                assert_eq!(changes.get("b"), Some(&Value::Null));
                assert!(!changes.contains_key("a"));
            }
            other => panic!("expected Update, got {other:?}"),
        }
    }

    // ── update_many ──────────────────────────────────────────────────────────

    #[test]
    fn update_many_fires_one_event_per_doc() {
        let db = db();
        let (col, hook) = hooked(&db, "items");
        col.insert_many(vec![
            vec![
                ("active".into(), Value::Bool(true)),
                ("v".into(), Value::Int(1)),
            ],
            vec![
                ("active".into(), Value::Bool(true)),
                ("v".into(), Value::Int(2)),
            ],
        ])
        .unwrap();
        hook.take();

        let n = col
            .update_many(
                Filter::Eq("active".into(), Value::Bool(true)),
                Update::Set(vec![("active".into(), Value::Bool(false))]),
            )
            .unwrap();
        assert_eq!(n, 2);

        let events = hook.take();
        assert_eq!(events.len(), 2);
        for event in &events {
            match event {
                SyncEvent::Update { changes, .. } => {
                    assert_eq!(changes.get("active"), Some(&Value::Bool(false)));
                    assert!(!changes.contains_key("v"));
                }
                other => panic!("expected Update, got {other:?}"),
            }
        }
    }

    // ── delete_one ───────────────────────────────────────────────────────────

    #[test]
    fn delete_one_fires_delete_event() {
        let db = db();
        let (col, hook) = hooked(&db, "items");
        let id = col.insert(vec![("x".into(), Value::Int(1))]).unwrap();
        hook.take();

        col.delete_one(Filter::Eq("x".into(), Value::Int(1)))
            .unwrap();

        let events = hook.take();
        assert_eq!(events.len(), 1);
        match &events[0] {
            SyncEvent::Delete {
                collection,
                id: eid,
            } => {
                assert_eq!(collection, "items");
                assert_eq!(eid, &id.to_string());
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn delete_one_no_match_fires_no_event() {
        let db = db();
        let (col, hook) = hooked(&db, "items");
        col.delete_one(Filter::Eq("x".into(), Value::Int(999)))
            .unwrap();
        assert_eq!(hook.len(), 0);
    }

    // ── delete_many ──────────────────────────────────────────────────────────

    #[test]
    fn delete_many_fires_one_event_per_doc() {
        let db = db();
        let (col, hook) = hooked(&db, "items");
        col.insert_many(vec![
            vec![("tag".into(), Value::Str("old".into()))],
            vec![("tag".into(), Value::Str("old".into()))],
            vec![("tag".into(), Value::Str("new".into()))],
        ])
        .unwrap();
        hook.take();

        let n = col
            .delete_many(Filter::Eq("tag".into(), Value::Str("old".into())))
            .unwrap();
        assert_eq!(n, 2);

        let events = hook.take();
        assert_eq!(events.len(), 2);
        for event in &events {
            assert!(matches!(event, SyncEvent::Delete { .. }));
        }
    }

    // ── delete_by_id ─────────────────────────────────────────────────────────

    #[test]
    fn delete_by_id_fires_delete_event() {
        let db = db();
        let (col, hook) = hooked(&db, "items");
        let id = col.insert(vec![("x".into(), Value::Int(1))]).unwrap();
        hook.take();

        col.delete_by_id(id).unwrap();

        let events = hook.take();
        assert_eq!(events.len(), 1);
        match &events[0] {
            SyncEvent::Delete { id: eid, .. } => assert_eq!(eid, &id.to_string()),
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    // ── diff_documents ───────────────────────────────────────────────────────

    #[test]
    fn diff_unchanged_doc_is_empty() {
        let doc = Document::new(vec![("a".into(), Value::Int(1))]);
        let (diff, removed) = diff_documents(&doc, &doc);
        assert!(diff.is_empty());
        assert!(removed.is_empty());
    }

    #[test]
    fn diff_new_field_included() {
        let old = Document::new(vec![("a".into(), Value::Int(1))]);
        let new = Document::new(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(2)),
        ]);
        let (diff, removed) = diff_documents(&old, &new);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff.get("b"), Some(&Value::Int(2)));
        assert!(removed.is_empty());
    }

    #[test]
    fn diff_removed_field_is_null_tombstone() {
        let old = Document::new(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(2)),
        ]);
        let new = Document::new(vec![("a".into(), Value::Int(1))]);
        let (diff, removed) = diff_documents(&old, &new);
        assert_eq!(diff.get("b"), Some(&Value::Null));
        assert!(!diff.contains_key("a"));
        assert_eq!(removed, vec!["b".to_string()]);
    }
}
