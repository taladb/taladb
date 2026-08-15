//! WorkerDB — the WASM database handle that runs inside the SharedWorker.
//!
//! The SharedWorker (taladb.worker.js) loads the WASM module, calls
//! `WorkerDB::open_with_opfs(handle)` once, then dispatches every operation
//! message to the synchronous methods below.
//!
//! All methods accept/return JSON strings so the JS worker script only needs
//! `JSON.stringify` / `JSON.parse` — no complex serialisation on the JS side.

use wasm_bindgen::prelude::*;
use web_sys::FileSystemSyncAccessHandle;

use taladb_core::engine::RedbBackend;
use taladb_core::{Database, Document, Filter, HnswOptions, Update, Value, VectorMetric};
// Encryption is only wired on the wasm OPFS open path — gate the imports the
// same way as `open_with_config_and_opfs` so native workspace builds (CI's
// cargo check/clippy) don't see them as unused.
#[cfg(target_arch = "wasm32")]
use std::sync::Arc;
#[cfg(target_arch = "wasm32")]
use taladb_core::{EncryptedBackend, MIN_PBKDF2_ITERATIONS, StorageBackend, derive_key};

use crate::doc_to_json;
use crate::storage::opfs_backend::OpfsBackend;

// ---------------------------------------------------------------------------
// WorkerDB
// ---------------------------------------------------------------------------

#[wasm_bindgen]
pub struct WorkerDB {
    db: Database,
}

#[wasm_bindgen]
impl WorkerDB {
    // ------------------------------------------------------------------
    // Constructors
    // ------------------------------------------------------------------

    /// Open an in-memory database (for tests and OPFS-unavailable fallback).
    #[wasm_bindgen(js_name = openInMemory)]
    pub fn open_in_memory() -> Result<Self, JsValue> {
        let db = Database::open_in_memory().map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Self { db })
    }

    /// Open a database, restoring from a previously exported snapshot if provided.
    ///
    /// Pass the bytes returned by `WorkerDB.exportSnapshot()` (or `null`/`undefined`
    /// for a fresh empty database). Used by the IndexedDB fallback path.
    ///
    /// ```js
    /// const bytes = await idbLoadSnapshot(dbName);   // null on first open
    /// const workerDb = WorkerDB.openWithSnapshot(bytes);
    /// ```
    #[wasm_bindgen(js_name = openWithSnapshot)]
    pub fn open_with_snapshot(data: Option<Vec<u8>>) -> Result<Self, JsValue> {
        let db = match data {
            Some(ref bytes) if !bytes.is_empty() => Database::restore_from_snapshot(bytes)
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            _ => Database::open_in_memory().map_err(|e| JsValue::from_str(&e.to_string()))?,
        };
        Ok(Self { db })
    }

    /// Open a database from an optional snapshot with HTTP push sync config.
    ///
    /// `config_json` - JSON-serialised `TalaDbConfig`, or `null` to open without sync.
    ///
    /// ```js
    /// const db = WorkerDB.openWithConfigAndSnapshot(snapshot, JSON.stringify(config));
    /// ```
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen(js_name = openWithConfigAndSnapshot)]
    pub fn open_with_config_and_snapshot(
        data: Option<Vec<u8>>,
        config_json: Option<String>,
    ) -> Result<WorkerDB, JsValue> {
        let db = match data {
            Some(ref bytes) if !bytes.is_empty() => Database::restore_from_snapshot(bytes)
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            _ => Database::open_in_memory().map_err(|e| JsValue::from_str(&e.to_string()))?,
        };
        let _ = config_json;
        Ok(WorkerDB { db })
    }

    /// Serialize the entire in-memory database to bytes for persistence.
    ///
    /// Pass the returned bytes to `idbSaveSnapshot` to persist across page reloads.
    /// On next open, pass the same bytes to `openWithSnapshot` to restore all data.
    #[wasm_bindgen(js_name = exportSnapshot)]
    pub fn export_snapshot(&self) -> Result<Vec<u8>, JsValue> {
        self.db
            .export_snapshot()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Set write durability: `eventual = true` batches OPFS fsyncs for
    /// throughput (call `flush()` to force), `false` (default) fsyncs each
    /// commit. Derived from `durability.flush_every_write` by the worker.
    #[wasm_bindgen(js_name = setDurability)]
    pub fn set_durability(&self, eventual: bool) {
        self.db.set_durability(eventual);
    }

    /// Force batched (eventual) OPFS writes to durable storage. No-op under the
    /// default immediate durability. Backs `db.flush()`.
    #[wasm_bindgen(js_name = flush)]
    pub fn flush(&self) -> Result<(), JsValue> {
        self.db
            .flush()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Open a database backed by an OPFS `FileSystemSyncAccessHandle`.
    ///
    /// Call sequence in the SharedWorker:
    /// ```js
    /// const handle = await file_handle.createSyncAccessHandle();
    /// const workerDb = WorkerDB.openWithOpfs(handle);
    /// ```
    #[wasm_bindgen(js_name = openWithOpfs)]
    pub fn open_with_opfs(sync_handle: FileSystemSyncAccessHandle) -> Result<Self, JsValue> {
        let opfs = OpfsBackend::from_handle(sync_handle);
        let redb_backend = RedbBackend::open_with_redb_backend(opfs)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let db = Database::open_with_backend(Box::new(redb_backend))
            .map_err(|e: taladb_core::TalaDbError| JsValue::from_str(&e.to_string()))?;
        Ok(Self { db })
    }

    /// Open a database backed by OPFS with HTTP push sync config.
    ///
    /// `config_json` - JSON-serialised `TalaDbConfig`, or `null` to open without sync.
    ///
    /// ```js
    /// const handle = await file_handle.createSyncAccessHandle();
    /// const db = WorkerDB.openWithConfigAndOpfs(handle, JSON.stringify(config));
    /// ```
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen(js_name = openWithConfigAndOpfs)]
    pub fn open_with_config_and_opfs(
        sync_handle: FileSystemSyncAccessHandle,
        config_json: Option<String>,
        passphrase: Option<String>,
        salt: Option<Vec<u8>>,
    ) -> Result<WorkerDB, JsValue> {
        let opfs = OpfsBackend::from_handle(sync_handle);
        let redb_backend = RedbBackend::open_with_redb_backend(opfs)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        // When a passphrase is supplied, wrap the OPFS-backed storage in the
        // AES-GCM-256 EncryptedBackend so every value is ciphertext at rest —
        // the same construction as the native `open_encrypted`. The 16-byte
        // salt is generated and persisted by the JS worker (an OPFS sidecar)
        // and passed in, so key derivation is deterministic across opens.
        let backend: Box<dyn StorageBackend> = match passphrase {
            Some(passphrase) => {
                if passphrase.is_empty() {
                    return Err(JsValue::from_str("encryption passphrase must not be empty"));
                }
                let salt = salt.ok_or_else(|| {
                    JsValue::from_str("encryption salt is required when a passphrase is given")
                })?;
                if salt.len() != 16 {
                    return Err(JsValue::from_str("encryption salt must be 16 bytes"));
                }
                let key = derive_key(&passphrase, &salt, MIN_PBKDF2_ITERATIONS)
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                let raw: Arc<dyn StorageBackend> = Arc::new(redb_backend);
                Box::new(EncryptedBackend::new(raw, key))
            }
            None => Box::new(redb_backend),
        };

        let db = Database::open_with_backend(backend)
            .map_err(|e: taladb_core::TalaDbError| JsValue::from_str(&e.to_string()))?;
        let _ = config_json;
        Ok(WorkerDB { db })
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Get a collection handle.
    fn get_collection(
        &self,
        name: &str,
    ) -> Result<taladb_core::Collection, taladb_core::TalaDbError> {
        self.db.collection(name)
    }

    // ------------------------------------------------------------------
    // CRUD — all synchronous, accept/return JSON strings
    // ------------------------------------------------------------------

    /// Insert a document. Returns the new ULID as a string.
    pub fn insert(&self, collection: &str, doc_json: &str) -> Result<String, JsValue> {
        let v: serde_json::Value =
            serde_json::from_str(doc_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let fields = json_obj_to_fields(&v)?;
        let id = self
            .get_collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .insert(fields)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(id.to_string())
    }

    /// Insert many documents. Returns a JSON array of ULID strings.
    #[wasm_bindgen(js_name = insertMany)]
    pub fn insert_many(&self, collection: &str, docs_json: &str) -> Result<String, JsValue> {
        let arr: Vec<serde_json::Value> =
            serde_json::from_str(docs_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let items: Result<Vec<_>, _> = arr.iter().map(json_obj_to_fields).collect();
        let ids = self
            .get_collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .insert_many(items?)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let id_strs: Vec<String> = ids.iter().map(taladb_core::Ulid::to_string).collect();
        serde_json::to_string(&id_strs).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Upsert documents **by their own `_id`**, in one commit per document.
    ///
    /// Backs cross-tab write propagation: a tab that cannot hold the OPFS lock
    /// writes to an in-memory database, then forwards the committed documents
    /// here so the tab holding the lock applies them to the durable file. The
    /// `_id`s travel with the documents, so an id an application already holds
    /// stays valid after the hand-off — which a plain re-`insert` would break by
    /// minting a new ULID.
    ///
    /// Unlike `insert_many`, a caller-supplied `_id` is required, not ignored.
    #[wasm_bindgen(js_name = upsertManyWithIds)]
    pub fn upsert_many_with_ids(
        &self,
        collection: &str,
        docs_json: &str,
    ) -> Result<String, JsValue> {
        let arr: Vec<serde_json::Value> =
            serde_json::from_str(docs_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let col = self
            .get_collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let mut ids = Vec::with_capacity(arr.len());
        for v in &arr {
            let doc = json_obj_to_doc(v)?;
            let id = col
                .replace_with_id(doc)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            ids.push(id.to_string());
        }
        serde_json::to_string(&ids).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Delete documents by id. Returns how many were present and removed.
    /// The delete half of cross-tab write propagation.
    #[wasm_bindgen(js_name = deleteManyWithIds)]
    pub fn delete_many_with_ids(&self, collection: &str, ids_json: &str) -> Result<u32, JsValue> {
        let ids: Vec<String> =
            serde_json::from_str(ids_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let col = self
            .get_collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let mut n = 0u32;
        for raw in &ids {
            let id = taladb_core::Ulid::from_string(raw)
                .map_err(|_| JsValue::from_str(&format!("\"{raw}\" is not a valid ULID")))?;
            if col
                .delete_by_id(id)
                .map_err(|e| JsValue::from_str(&e.to_string()))?
            {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Find documents. Returns a JSON array of document objects.
    pub fn find(&self, collection: &str, filter_json: &str) -> Result<String, JsValue> {
        let filter = parse_filter(filter_json)?;
        let docs = self
            .db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .find(filter)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let json: Vec<serde_json::Value> = docs.iter().map(doc_to_json).collect();
        serde_json::to_string(&json).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Find one document. Returns a JSON object or `"null"`.
    #[wasm_bindgen(js_name = findOne)]
    pub fn find_one(&self, collection: &str, filter_json: &str) -> Result<String, JsValue> {
        let filter = parse_filter(filter_json)?;
        let doc = self
            .db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .find_one(filter)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let json = doc.as_ref().map(doc_to_json);
        serde_json::to_string(&json).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Update the first matching document. Returns `true` / `false`.
    #[wasm_bindgen(js_name = updateOne)]
    pub fn update_one(
        &self,
        collection: &str,
        filter_json: &str,
        update_json: &str,
    ) -> Result<bool, JsValue> {
        let filter = parse_filter(filter_json)?;
        let update = parse_update(update_json)?;
        self.get_collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .update_one(filter, update)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Update all matching documents. Returns the count updated.
    #[wasm_bindgen(js_name = updateMany)]
    pub fn update_many(
        &self,
        collection: &str,
        filter_json: &str,
        update_json: &str,
    ) -> Result<u32, JsValue> {
        let filter = parse_filter(filter_json)?;
        let update = parse_update(update_json)?;
        self.get_collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .update_many(filter, update)
            .map(|n| n as u32)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Delete the first matching document. Returns `true` / `false`.
    #[wasm_bindgen(js_name = deleteOne)]
    pub fn delete_one(&self, collection: &str, filter_json: &str) -> Result<bool, JsValue> {
        let filter = parse_filter(filter_json)?;
        self.get_collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .delete_one(filter)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Delete all matching documents. Returns the count deleted.
    #[wasm_bindgen(js_name = deleteMany)]
    pub fn delete_many(&self, collection: &str, filter_json: &str) -> Result<u32, JsValue> {
        let filter = parse_filter(filter_json)?;
        self.get_collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .delete_many(filter)
            .map(|n| n as u32)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Count matching documents.
    pub fn count(&self, collection: &str, filter_json: &str) -> Result<u32, JsValue> {
        let filter = parse_filter(filter_json)?;
        self.db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .count(filter)
            .map(|n| n as u32)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// This collection's write generation — a counter bumped once per committed
    /// mutation.
    ///
    /// Backs `subscribe()`: a live query can compare one integer per tick
    /// instead of re-running the query and `JSON.stringify`-ing the whole
    /// result set to see whether anything moved.
    #[wasm_bindgen(js_name = writeGeneration)]
    pub fn write_generation(&self, collection: &str) -> Result<f64, JsValue> {
        Ok(self
            .db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .write_generation() as f64)
    }

    /// Run an aggregation pipeline. Returns a JSON array of result documents.
    pub fn aggregate(&self, collection: &str, pipeline_json: &str) -> Result<String, JsValue> {
        let value: serde_json::Value =
            serde_json::from_str(pipeline_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let pipeline = taladb_core::aggregate::parse_pipeline(&value, &|v| {
            json_to_filter_val(v).ok_or_else(|| "invalid filter in $match".to_string())
        })
        .map_err(|e| JsValue::from_str(&e))?;
        let docs = self
            .db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .aggregate(pipeline)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let json: Vec<serde_json::Value> = docs.iter().map(doc_to_json).collect();
        serde_json::to_string(&json).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    // ------------------------------------------------------------------
    // Index management
    // ------------------------------------------------------------------

    #[wasm_bindgen(js_name = createIndex)]
    pub fn create_index(&self, collection: &str, field: &str) -> Result<(), JsValue> {
        self.db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .create_index(field)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = dropIndex)]
    pub fn drop_index(&self, collection: &str, field: &str) -> Result<(), JsValue> {
        self.db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .drop_index(field)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Create a compound index. `fields_json` is a JSON array of field names.
    #[wasm_bindgen(js_name = createCompoundIndex)]
    pub fn create_compound_index(
        &self,
        collection: &str,
        fields_json: &str,
    ) -> Result<(), JsValue> {
        let fields: Vec<String> =
            serde_json::from_str(fields_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        self.db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .create_compound_index(&refs)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Drop a compound index by its ordered field list (`fields_json`).
    #[wasm_bindgen(js_name = dropCompoundIndex)]
    pub fn drop_compound_index(&self, collection: &str, fields_json: &str) -> Result<(), JsValue> {
        let fields: Vec<String> =
            serde_json::from_str(fields_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        self.db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .drop_compound_index(&refs)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = createFtsIndex)]
    pub fn create_fts_index(&self, collection: &str, field: &str) -> Result<(), JsValue> {
        self.db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .create_fts_index(field)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = dropFtsIndex)]
    pub fn drop_fts_index(&self, collection: &str, field: &str) -> Result<(), JsValue> {
        self.db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .drop_fts_index(field)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Rank documents against a free-text query using BM25 (OR semantics).
    ///
    /// Returns a JSON array of `{ document, score }`.
    #[wasm_bindgen(js_name = searchText)]
    pub fn search_text(
        &self,
        collection: &str,
        field: &str,
        query: &str,
        top_k: u32,
        filter_json: &str,
        options_json: &str,
    ) -> Result<String, JsValue> {
        let pre_filter = parse_optional_filter(filter_json)?;
        let options = parse_options(options_json)?;
        let bm25 = bm25_from_options(options.as_ref());

        let results = self
            .db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .search_text_with(field, query, top_k as usize, &bm25, pre_filter)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let json: Vec<serde_json::Value> = results
            .iter()
            .map(|r| serde_json::json!({ "document": doc_to_json(&r.document), "score": r.score }))
            .collect();
        serde_json::to_string(&json).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Hybrid retrieval — BM25 and vector similarity fused with reciprocal
    /// rank fusion.
    ///
    /// `options_json` accepts `{ rrfK, textWeight, vectorWeight, candidates, k1, b }`.
    /// Returns a JSON array of `{ document, score, textRank, vectorRank }`.
    #[allow(clippy::too_many_arguments)]
    #[wasm_bindgen(js_name = hybridSearch)]
    pub fn hybrid_search(
        &self,
        collection: &str,
        text_field: &str,
        text: &str,
        vector_field: &str,
        vector_json: &str,
        top_k: u32,
        filter_json: &str,
        options_json: &str,
    ) -> Result<String, JsValue> {
        let vector: Vec<f32> =
            serde_json::from_str(vector_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let pre_filter = parse_optional_filter(filter_json)?;
        let options = parse_options(options_json)?;

        let mut query = taladb_core::fts::HybridQuery::new(
            text_field,
            text,
            vector_field,
            &vector,
            top_k as usize,
        );
        query.filter = pre_filter;
        query.bm25 = bm25_from_options(options.as_ref());
        query.rrf = rrf_from_options(options.as_ref());
        query.candidates = options
            .as_ref()
            .and_then(|o| o.get("candidates"))
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as usize);

        let results = self
            .db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .hybrid_search(query)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let json: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "document": doc_to_json(&r.document),
                    "score": r.score,
                    "textRank": r.text_rank,
                    "vectorRank": r.vector_rank,
                })
            })
            .collect();
        serde_json::to_string(&json).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Create a vector index.
    ///
    /// - `metric_str`: `"cosine"` (default) | `"dot"` | `"euclidean"`
    /// - `index_type`: `"flat"` (default) | `"hnsw"`
    /// - `hnsw_m`: HNSW connectivity (default 16, only used when `index_type = "hnsw"`)
    /// - `hnsw_ef_construction`: build-time quality (default 200, only used when `index_type = "hnsw"`)
    #[allow(clippy::too_many_arguments)]
    #[wasm_bindgen(js_name = createVectorIndex)]
    pub fn create_vector_index(
        &self,
        collection: &str,
        field: &str,
        dimensions: u32,
        metric_str: Option<String>,
        index_type: Option<String>,
        hnsw_m: Option<u32>,
        hnsw_ef_construction: Option<u32>,
    ) -> Result<(), JsValue> {
        let metric = parse_metric(metric_str)?;
        let hnsw = parse_hnsw_opts(index_type, hnsw_m, hnsw_ef_construction);
        self.db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .create_vector_index(field, dimensions as usize, metric, hnsw)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Drop a vector index (and its HNSW graph if present).
    #[wasm_bindgen(js_name = dropVectorIndex)]
    pub fn drop_vector_index(&self, collection: &str, field: &str) -> Result<(), JsValue> {
        self.db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .drop_vector_index(field)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Rebuild the HNSW graph for a vector index from the current flat vector
    /// table.  Use after bulk inserts or when ANN recall has degraded.
    ///
    /// No-op when the `vector-hnsw` feature is disabled or the index is flat-only.
    #[wasm_bindgen(js_name = upgradeVectorIndex)]
    pub fn upgrade_vector_index(&self, collection: &str, field: &str) -> Result<(), JsValue> {
        self.db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .upgrade_vector_index(field)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Returns a JSON string `{ btree: string[], fts: string[], vector: string[] }`
    /// listing all indexes on the given collection.
    #[wasm_bindgen(js_name = listIndexes)]
    pub fn list_indexes(&self, collection: &str) -> Result<String, JsValue> {
        let info = self
            .db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .list_indexes()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let json = serde_json::json!({
            "btree": info.btree,
            "fts": info.fts,
            "vector": info.vector,
        });
        Ok(json.to_string())
    }

    /// Find nearest neighbours. Returns a JSON string of `[{ document, score }]`.
    #[wasm_bindgen(js_name = findNearest)]
    pub fn find_nearest(
        &self,
        collection: &str,
        field: &str,
        query_json: &str,
        top_k: u32,
        filter_json: &str,
    ) -> Result<String, JsValue> {
        let query: Vec<f32> =
            serde_json::from_str(query_json).map_err(|e| JsValue::from_str(&e.to_string()))?;

        let pre_filter = if filter_json == "null" {
            None
        } else {
            let v: serde_json::Value =
                serde_json::from_str(filter_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
            Some(json_to_filter_val(&v).ok_or_else(|| JsValue::from_str("invalid filter"))?)
        };

        let results = self
            .db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .find_nearest(field, &query, top_k as usize, pre_filter)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let json: Vec<serde_json::Value> = results
            .iter()
            .map(|r| serde_json::json!({ "document": doc_to_json(&r.document), "score": r.score }))
            .collect();

        serde_json::to_string(&json).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    // ------------------------------------------------------------------
    // Storage compaction
    // ------------------------------------------------------------------

    /// Compact the underlying OPFS / redb storage file, reclaiming space freed
    /// by deletes and updates.
    ///
    /// Call this during idle periods (e.g. once on app startup after tombstone
    /// compaction). No-op on in-memory (IDB-fallback) databases.
    ///
    /// ```js
    /// db.compact();
    /// ```
    pub fn compact(&self) -> Result<(), JsValue> {
        self.db
            .compact()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    // ------------------------------------------------------------------
    // Collection introspection
    // ------------------------------------------------------------------

    /// Returns a JSON array of all collection names in the database.
    #[wasm_bindgen(js_name = listCollections)]
    pub fn list_collections(&self) -> Result<String, JsValue> {
        let names = self
            .db
            .list_collection_names()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        serde_json::to_string(&names).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Read the current application migration version (0 if never set). Backs
    /// the `openDB({ migrations })` runner, which advances it per migration.
    #[wasm_bindgen(js_name = userVersion)]
    pub fn user_version(&self) -> Result<u32, JsValue> {
        self.db
            .user_version()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Persist the application migration version. Called after each migration's
    /// body succeeds so a crash mid-run resumes from the last applied version.
    #[wasm_bindgen(js_name = setUserVersion)]
    pub fn set_user_version(&self, version: u32) -> Result<(), JsValue> {
        self.db
            .set_user_version(version)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Vector helpers
// ---------------------------------------------------------------------------

fn parse_metric(metric: Option<String>) -> Result<Option<VectorMetric>, JsValue> {
    match metric.as_deref() {
        None | Some("cosine") => Ok(Some(VectorMetric::Cosine)),
        Some("dot") => Ok(Some(VectorMetric::Dot)),
        Some("euclidean") => Ok(Some(VectorMetric::Euclidean)),
        Some(other) => Err(JsValue::from_str(&format!(
            "unknown metric \"{other}\": expected \"cosine\", \"dot\", or \"euclidean\""
        ))),
    }
}

/// Parse optional HNSW parameters. Returns `None` for flat indexes.
fn parse_hnsw_opts(
    index_type: Option<String>,
    m: Option<u32>,
    ef_construction: Option<u32>,
) -> Option<HnswOptions> {
    match index_type.as_deref() {
        Some("hnsw") => Some(HnswOptions {
            m: m.unwrap_or(16),
            ef_construction: ef_construction.unwrap_or(200),
        }),
        _ => None, // "flat" or absent → flat index
    }
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

/// Parse a JSON object into insert fields, `_id` included.
///
/// `_id` used to be filtered out here, which is why supplying one was a silent
/// no-op in the browser. The engine now owns that decision: it takes the id if
/// one is present, rejects a non-ULID with an actionable message, and refuses to
/// overwrite an existing document. Filtering here would put the browser back on
/// different semantics from Node and React Native.
fn json_obj_to_fields(v: &serde_json::Value) -> Result<Vec<(String, Value)>, JsValue> {
    match v {
        serde_json::Value::Object(map) => Ok(map
            .iter()
            .map(|(k, v)| (k.clone(), json_to_core_value(v)))
            .collect()),
        _ => Err(JsValue::from_str("document must be a JSON object")),
    }
}

/// Parse a JSON object into a [`Document`], **honouring `_id`**.
///
/// The counterpart `json_obj_to_fields` deliberately drops `_id` — the engine
/// mints its own ULID on insert. Cross-tab propagation needs the opposite: the
/// id is the whole point, because it is what keeps a document the same document
/// after it is handed to the tab holding the OPFS lock. So a missing or
/// malformed `_id` is a hard error here, not a silently-generated new row.
fn json_obj_to_doc(v: &serde_json::Value) -> Result<Document, JsValue> {
    let serde_json::Value::Object(map) = v else {
        return Err(JsValue::from_str("document must be a JSON object"));
    };
    let Some(serde_json::Value::String(raw)) = map.get("_id") else {
        return Err(JsValue::from_str(
            "upsertManyWithIds requires a string `_id` on every document",
        ));
    };
    let id = taladb_core::Ulid::from_string(raw)
        .map_err(|_| JsValue::from_str(&format!("\"{raw}\" is not a valid ULID")))?;
    let fields = map
        .iter()
        .filter(|(k, _)| k.as_str() != "_id")
        .map(|(k, v)| (k.clone(), json_to_core_value(v)))
        .collect();
    Ok(Document::with_id(id, fields))
}

/// Parse a filter argument that may legitimately be absent.
///
/// The worker protocol sends `"null"` (and, defensively, an empty string) for
/// "no filter" rather than omitting the argument.
fn parse_optional_filter(json: &str) -> Result<Option<Filter>, JsValue> {
    if json.is_empty() || json == "null" {
        return Ok(None);
    }
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
    if v.is_null() {
        return Ok(None);
    }
    Ok(Some(
        json_to_filter_val(&v).ok_or_else(|| JsValue::from_str("invalid filter"))?,
    ))
}

/// Parse the shared options bag, treating absent/blank/`null` as "defaults".
fn parse_options(json: &str) -> Result<Option<serde_json::Value>, JsValue> {
    if json.is_empty() || json == "null" {
        return Ok(None);
    }
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
    Ok(if v.is_null() { None } else { Some(v) })
}

fn bm25_from_options(options: Option<&serde_json::Value>) -> taladb_core::bm25::Bm25Params {
    let mut params = taladb_core::bm25::Bm25Params::default();
    if let Some(o) = options {
        if let Some(v) = o.get("k1").and_then(serde_json::Value::as_f64) {
            params.k1 = v as f32;
        }
        if let Some(v) = o.get("b").and_then(serde_json::Value::as_f64) {
            params.b = v as f32;
        }
    }
    params
}

fn rrf_from_options(options: Option<&serde_json::Value>) -> taladb_core::bm25::RrfParams {
    let mut params = taladb_core::bm25::RrfParams::default();
    if let Some(o) = options {
        if let Some(v) = o.get("rrfK").and_then(serde_json::Value::as_f64) {
            params.k = v as f32;
        }
        if let Some(v) = o.get("textWeight").and_then(serde_json::Value::as_f64) {
            params.text_weight = v as f32;
        }
        if let Some(v) = o.get("vectorWeight").and_then(serde_json::Value::as_f64) {
            params.vector_weight = v as f32;
        }
    }
    params
}

fn parse_filter(json: &str) -> Result<Filter, JsValue> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
    if v.is_null() {
        return Ok(Filter::All);
    }
    json_to_filter_val(&v).ok_or_else(|| JsValue::from_str("invalid filter"))
}

fn parse_update(json: &str) -> Result<Update, JsValue> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| JsValue::from_str(&e.to_string()))?;
    json_to_update_val(&v)
}

fn json_to_core_value(j: &serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(arr) => Value::Array(arr.iter().map(json_to_core_value).collect()),
        serde_json::Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_core_value(v)))
                .collect(),
        ),
    }
}

fn json_to_filter_val(v: &serde_json::Value) -> Option<Filter> {
    let obj = v.as_object()?;

    let mut filters: Vec<Filter> = Vec::new();
    for (field, expr) in obj {
        if field.starts_with('$') {
            let logical = match field.as_str() {
                "$and" => Filter::And(
                    expr.as_array()?
                        .iter()
                        .map(json_to_filter_val)
                        .collect::<Option<_>>()?,
                ),
                "$or" => Filter::Or(
                    expr.as_array()?
                        .iter()
                        .map(json_to_filter_val)
                        .collect::<Option<_>>()?,
                ),
                "$not" => Filter::Not(Box::new(json_to_filter_val(expr)?)),
                _ => return None,
            };
            filters.push(logical);
            continue;
        }
        if !expr.is_object() {
            filters.push(Filter::Eq(field.clone(), json_to_core_value(expr)));
            continue;
        }
        let ops = expr.as_object()?;
        if ops.is_empty() {
            // `{field: {}}` is ambiguous (error on node, match-all
            // historically here) — rejected on every platform as of 0.8.1.
            return None;
        }
        for (op, val) in ops {
            let v = json_to_core_value(val);
            let f = match op.as_str() {
                "$eq" => Filter::Eq(field.clone(), v),
                "$ne" => Filter::Ne(field.clone(), v),
                "$gt" => Filter::Gt(field.clone(), v),
                "$gte" => Filter::Gte(field.clone(), v),
                "$lt" => Filter::Lt(field.clone(), v),
                "$lte" => Filter::Lte(field.clone(), v),
                "$in" => Filter::In(
                    field.clone(),
                    val.as_array()?.iter().map(json_to_core_value).collect(),
                ),
                "$nin" => Filter::Nin(
                    field.clone(),
                    val.as_array()?.iter().map(json_to_core_value).collect(),
                ),
                "$exists" => Filter::Exists(field.clone(), val.as_bool()?),
                "$contains" => Filter::Contains(field.clone(), val.as_str()?.to_string()),
                "$regex" => Filter::Regex(field.clone(), val.as_str()?.to_string()),
                _ => return None,
            };
            filters.push(f);
        }
    }
    match filters.len() {
        0 => Some(Filter::All),
        1 => Some(filters.remove(0)),
        _ => Some(Filter::And(filters)),
    }
}

fn json_to_update_val(v: &serde_json::Value) -> Result<Update, JsValue> {
    let obj = v
        .as_object()
        .ok_or_else(|| JsValue::from_str("update must be an object"))?;

    let mut updates = Vec::new();
    if let Some(set) = obj.get("$set") {
        let pairs = set
            .as_object()
            .ok_or_else(|| JsValue::from_str("$set must be object"))?
            .iter()
            .map(|(k, v)| (k.clone(), json_to_core_value(v)))
            .collect();
        updates.push(Update::Set(pairs));
    }
    if let Some(unset) = obj.get("$unset") {
        let keys = unset
            .as_object()
            .ok_or_else(|| JsValue::from_str("$unset must be object"))?
            .keys()
            .cloned()
            .collect();
        updates.push(Update::Unset(keys));
    }
    if let Some(inc) = obj.get("$inc") {
        let pairs = inc
            .as_object()
            .ok_or_else(|| JsValue::from_str("$inc must be object"))?
            .iter()
            .map(|(k, v)| (k.clone(), json_to_core_value(v)))
            .collect();
        updates.push(Update::Inc(pairs));
    }
    if let Some(push) = obj.get("$push") {
        let map = push
            .as_object()
            .ok_or_else(|| JsValue::from_str("$push must be object"))?;
        updates.extend(
            map.iter()
                .map(|(k, v)| Update::Push(k.clone(), json_to_core_value(v))),
        );
    }
    if let Some(pull) = obj.get("$pull") {
        let map = pull
            .as_object()
            .ok_or_else(|| JsValue::from_str("$pull must be object"))?;
        updates.extend(
            map.iter()
                .map(|(k, v)| Update::Pull(k.clone(), json_to_core_value(v))),
        );
    }
    if obj
        .keys()
        .any(|k| !matches!(k.as_str(), "$set" | "$unset" | "$inc" | "$push" | "$pull"))
    {
        return Err(JsValue::from_str("unsupported update operator"));
    }
    match updates.len() {
        0 => Err(JsValue::from_str("update must contain an operator")),
        1 => Ok(updates.remove(0)),
        _ => Ok(Update::Many(updates)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Host-side tests for the pure JSON→Filter parsing (no browser needed).
/// `parse_filter` itself wraps errors in `JsValue` (wasm-only), so these
/// exercise `json_to_filter_val`, where the parsing decisions live.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod filter_parse_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_operator_object_is_invalid() {
        // Aligned with the node binding as of 0.8.1: `{a: {}}` is rejected
        // instead of silently matching every document.
        assert!(json_to_filter_val(&json!({"a": {}})).is_none());
    }

    #[test]
    fn empty_filter_object_still_matches_all() {
        assert!(matches!(json_to_filter_val(&json!({})), Some(Filter::All)));
    }

    #[test]
    fn unknown_operator_is_invalid() {
        assert!(json_to_filter_val(&json!({"a": {"$qe": 1}})).is_none());
    }

    #[test]
    fn regex_operator_parses() {
        assert!(matches!(
            json_to_filter_val(&json!({"email": {"$regex": r"@example\.com$"}})),
            Some(Filter::Regex(..))
        ));
    }

    #[test]
    fn logical_operator_keeps_sibling_predicates() {
        let filter = json_to_filter_val(&json!({"tenant":"a","$or":[{"x":1},{"x":2}]})).unwrap();
        assert!(matches!(filter, Filter::And(parts) if parts.len() == 2));
    }

    #[test]
    fn strict_operator_operands() {
        assert!(json_to_filter_val(&json!({"a":{"$exists":"yes"}})).is_none());
        assert!(json_to_filter_val(&json!({"a":{"$contains":1}})).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    // ------------------------------------------------------------------
    // WorkerDB::open_with_snapshot
    // ------------------------------------------------------------------

    #[wasm_bindgen_test]
    fn open_with_snapshot_none_produces_empty_db() {
        let db = WorkerDB::open_with_snapshot(None).unwrap();
        // A collection that was never written returns an empty JSON array.
        let result = db.find("items", "null").unwrap();
        assert_eq!(result, "[]");
    }

    #[wasm_bindgen_test]
    fn open_with_snapshot_empty_vec_treated_as_fresh_db() {
        // An empty Vec<u8> is equivalent to None — open fresh in-memory DB.
        let db = WorkerDB::open_with_snapshot(Some(vec![])).unwrap();
        let result = db.find("items", "null").unwrap();
        assert_eq!(result, "[]");
    }

    #[wasm_bindgen_test]
    fn open_with_snapshot_restores_documents() {
        // Build a DB, export its snapshot, restore into a new WorkerDB.
        let original = WorkerDB::open_with_snapshot(None).unwrap();
        original.insert("items", r#"{"name":"Alice"}"#).unwrap();
        original.insert("items", r#"{"name":"Bob"}"#).unwrap();

        let snapshot = original.export_snapshot().unwrap();

        let restored = WorkerDB::open_with_snapshot(Some(snapshot)).unwrap();
        let json = restored.find("items", "null").unwrap();
        let docs: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(docs.len(), 2);
        let names: Vec<&str> = docs.iter().filter_map(|d| d["name"].as_str()).collect();
        assert!(names.contains(&"Alice"));
        assert!(names.contains(&"Bob"));
    }

    #[wasm_bindgen_test]
    fn open_with_snapshot_restores_multiple_collections() {
        let original = WorkerDB::open_with_snapshot(None).unwrap();
        original.insert("users", r#"{"name":"Alice"}"#).unwrap();
        original.insert("posts", r#"{"title":"Hello"}"#).unwrap();
        original.insert("posts", r#"{"title":"World"}"#).unwrap();

        let snapshot = original.export_snapshot().unwrap();
        let restored = WorkerDB::open_with_snapshot(Some(snapshot)).unwrap();

        let users: Vec<serde_json::Value> =
            serde_json::from_str(&restored.find("users", "null").unwrap()).unwrap();
        let posts: Vec<serde_json::Value> =
            serde_json::from_str(&restored.find("posts", "null").unwrap()).unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(posts.len(), 2);
    }

    #[wasm_bindgen_test]
    fn open_with_snapshot_preserves_document_ids() {
        let original = WorkerDB::open_with_snapshot(None).unwrap();
        let id = original.insert("col", r#"{"x":42}"#).unwrap();

        let snapshot = original.export_snapshot().unwrap();
        let restored = WorkerDB::open_with_snapshot(Some(snapshot)).unwrap();

        let json = restored.find("col", "null").unwrap();
        let docs: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0]["_id"].as_str().unwrap(), id);
    }

    // ------------------------------------------------------------------
    // WorkerDB::export_snapshot
    // ------------------------------------------------------------------

    #[wasm_bindgen_test]
    fn export_snapshot_of_empty_db_starts_with_magic_bytes() {
        let db = WorkerDB::open_with_snapshot(None).unwrap();
        let bytes = db.export_snapshot().unwrap();
        // TalaDB snapshot magic: "TDBS"
        assert!(bytes.len() >= 4, "snapshot must be at least 4 bytes");
        assert_eq!(&bytes[..4], b"TDBS");
    }

    #[wasm_bindgen_test]
    fn export_snapshot_grows_after_inserts() {
        let db = WorkerDB::open_with_snapshot(None).unwrap();
        let empty_size = db.export_snapshot().unwrap().len();

        db.insert("col", r#"{"payload":"aaaaaaaaaa"}"#).unwrap();
        let after_insert = db.export_snapshot().unwrap().len();

        assert!(
            after_insert > empty_size,
            "snapshot must grow after inserting a document"
        );
    }

    #[wasm_bindgen_test]
    fn export_snapshot_twice_produces_equal_bytes_if_no_writes_in_between() {
        let db = WorkerDB::open_with_snapshot(None).unwrap();
        db.insert("col", r#"{"k":"v"}"#).unwrap();

        let snap1 = db.export_snapshot().unwrap();
        let snap2 = db.export_snapshot().unwrap();
        assert_eq!(snap1, snap2);
    }

    // ------------------------------------------------------------------
    // Round-trip — snapshot → restore → mutate → snapshot again
    // ------------------------------------------------------------------

    #[wasm_bindgen_test]
    fn snapshot_round_trip_then_further_inserts_are_visible() {
        // First generation
        let db1 = WorkerDB::open_with_snapshot(None).unwrap();
        db1.insert("items", r#"{"gen":1}"#).unwrap();
        let snap1 = db1.export_snapshot().unwrap();

        // Second generation — restore from snap1, add more data
        let db2 = WorkerDB::open_with_snapshot(Some(snap1)).unwrap();
        db2.insert("items", r#"{"gen":2}"#).unwrap();
        let snap2 = db2.export_snapshot().unwrap();

        // Third generation — must see both gen:1 and gen:2
        let db3 = WorkerDB::open_with_snapshot(Some(snap2)).unwrap();
        let json = db3.find("items", "null").unwrap();
        let docs: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(docs.len(), 2);
        let gens: Vec<i64> = docs.iter().filter_map(|d| d["gen"].as_i64()).collect();
        assert!(gens.contains(&1));
        assert!(gens.contains(&2));
    }

    #[wasm_bindgen_test]
    fn snapshot_round_trip_preserves_secondary_index() {
        let db = WorkerDB::open_with_snapshot(None).unwrap();
        db.create_index("users", "age").unwrap();
        db.insert("users", r#"{"age":30,"name":"Alice"}"#).unwrap();
        db.insert("users", r#"{"age":25,"name":"Bob"}"#).unwrap();

        let snap = db.export_snapshot().unwrap();
        let restored = WorkerDB::open_with_snapshot(Some(snap)).unwrap();

        // Filter via the indexed field
        let json = restored.find("users", r#"{"age":{"$eq":30}}"#).unwrap();
        let docs: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0]["name"].as_str().unwrap(), "Alice");
    }

    #[wasm_bindgen_test]
    fn snapshot_round_trip_update_delete_then_restore() {
        let db = WorkerDB::open_with_snapshot(None).unwrap();
        db.insert("col", r#"{"k":"keep"}"#).unwrap();
        db.insert("col", r#"{"k":"remove"}"#).unwrap();

        db.delete_one("col", r#"{"k":"remove"}"#).unwrap();
        db.update_one("col", r#"{"k":"keep"}"#, r#"{"$set":{"k":"updated"}}"#)
            .unwrap();

        let snap = db.export_snapshot().unwrap();
        let restored = WorkerDB::open_with_snapshot(Some(snap)).unwrap();

        let json = restored.find("col", "null").unwrap();
        let docs: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(
            docs.len(),
            1,
            "deleted document must not appear in snapshot"
        );
        assert_eq!(docs[0]["k"].as_str().unwrap(), "updated");
    }
}
