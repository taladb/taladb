//! WorkerDB — the WASM database handle that runs inside the SharedWorker.
//!
//! The SharedWorker (taladb.worker.js) loads the WASM module, calls
//! `WorkerDB::open_with_opfs(db_name, sync_handle)` once, then dispatches
//! every operation message to the synchronous methods below.
//!
//! All methods accept/return JSON strings so the JS worker script only needs
//! `JSON.stringify` / `JSON.parse` — no complex serialisation on the JS side.

use wasm_bindgen::prelude::*;

#[cfg(not(feature = "cf-workers"))]
use web_sys::FileSystemSyncAccessHandle;

#[cfg(not(feature = "cf-workers"))]
use taladb_core::engine::RedbBackend;
use taladb_core::{
    Changeset, Database, Document, Filter, HnswOptions, LastWriteWins, SchemaValidator,
    SyncAdapter, Update, Value, VectorMetric, WriteOrigin,
};
// Encryption is only wired on the wasm OPFS open path — gate the imports the
// same way as `open_with_config_and_opfs` so native workspace builds (CI's
// cargo check/clippy) don't see them as unused.
#[cfg(all(target_arch = "wasm32", not(feature = "cf-workers")))]
use taladb_core::{EncryptedBackend, MIN_PBKDF2_ITERATIONS, StorageBackend, derive_key};

#[cfg(not(feature = "cf-workers"))]
use crate::storage::opfs_backend::OpfsBackend;
use crate::{build_schemas, doc_to_json};

#[cfg(target_arch = "wasm32")]
use serde_json::{Map, Value as JsonValue, json};
#[cfg(target_arch = "wasm32")]
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(target_arch = "wasm32")]
use taladb_core::config::SyncConfig;
#[cfg(target_arch = "wasm32")]
use taladb_core::{SyncEvent, SyncHook, TalaDbConfig};

// ---------------------------------------------------------------------------
// WasmSyncHook — HTTP push sync for the browser (WASM) platform
//
// `on_event` is synchronous (required by SyncHook). Events are appended to a
// FIFO queue and a single drain task POSTs them strictly in order — one
// `spawn_local` fetch per event would let retries and slow responses reorder
// deliveries (an update could reach the endpoint before its insert).
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
struct QueuedSyncTask {
    endpoint: String,
    headers: HashMap<String, String>,
    payload: JsonValue,
}

#[derive(Default)]
struct SyncHealth {
    pending: std::sync::atomic::AtomicU64,
    dropped: std::sync::atomic::AtomicU64,
    failed: std::sync::atomic::AtomicU64,
}

#[cfg(target_arch = "wasm32")]
struct WasmSyncHook {
    config: Arc<SyncConfig>,
    queue: Arc<std::sync::Mutex<std::collections::VecDeque<QueuedSyncTask>>>,
    draining: Arc<std::sync::atomic::AtomicBool>,
    health: Arc<SyncHealth>,
}

#[cfg(target_arch = "wasm32")]
impl WasmSyncHook {
    fn new(config: SyncConfig, health: Arc<SyncHealth>) -> Self {
        WasmSyncHook {
            config: Arc::new(config),
            queue: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            draining: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            health,
        }
    }

    fn endpoint_for(&self, event: &SyncEvent) -> Option<String> {
        let cfg = &*self.config;
        match event {
            SyncEvent::Insert { .. } => {
                cfg.insert_endpoint.clone().or_else(|| cfg.endpoint.clone())
            }
            SyncEvent::Update { .. } => {
                cfg.update_endpoint.clone().or_else(|| cfg.endpoint.clone())
            }
            SyncEvent::Delete { .. } => {
                cfg.delete_endpoint.clone().or_else(|| cfg.endpoint.clone())
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl SyncHook for WasmSyncHook {
    fn on_event(&self, event: SyncEvent) {
        use std::sync::atomic::Ordering;

        if !self.config.enabled {
            return;
        }
        let Some(endpoint) = self.endpoint_for(&event) else {
            return;
        };
        let payload = build_wasm_payload(event, &self.config.exclude_fields);
        let headers = self.config.headers.clone();

        let mut queue = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        if queue.len() >= 256 {
            self.health.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        queue.push_back(QueuedSyncTask {
            endpoint,
            headers,
            payload,
        });
        self.health.pending.fetch_add(1, Ordering::Relaxed);
        drop(queue);

        // Start the drain task if one is not already running. WASM is
        // single-threaded, so there is no interleaving between the swap and
        // the spawn; the drain loop only yields at awaits, where new events
        // can safely be appended.
        if !self.draining.swap(true, Ordering::AcqRel) {
            let queue = Arc::clone(&self.queue);
            let draining = Arc::clone(&self.draining);
            let health = Arc::clone(&self.health);
            wasm_bindgen_futures::spawn_local(async move {
                loop {
                    let task = queue.lock().unwrap_or_else(|p| p.into_inner()).pop_front();
                    match task {
                        Some(t) => {
                            if !fire_wasm_with_retry(&t.endpoint, &t.headers, &t.payload).await {
                                health.failed.fetch_add(1, Ordering::Relaxed);
                            }
                            health.pending.fetch_sub(1, Ordering::Relaxed);
                        }
                        None => break,
                    }
                }
                draining.store(false, Ordering::Release);
            });
        }
    }
}

/// Build the JSON payload for a sync event (WASM variant - uses js_sys::Date for timestamp).
#[cfg(target_arch = "wasm32")]
fn build_wasm_payload(event: SyncEvent, exclude: &[String]) -> JsonValue {
    let ts = js_sys::Date::now() as u64;
    match event {
        SyncEvent::Insert {
            collection,
            id,
            document,
        } => {
            let doc_obj = doc_fields_to_json(&document, exclude);
            json!({
                "_taladb_event": "insert",
                "collection": collection,
                "id": id,
                "document": doc_obj,
                "timestamp": ts,
            })
        }
        SyncEvent::Update {
            collection,
            id,
            changes,
            removed,
        } => {
            let mut changes_obj = Map::new();
            for (k, v) in &changes {
                if !exclude.contains(k) {
                    changes_obj.insert(k.clone(), wasm_value_to_json(v));
                }
            }
            let removed_fields: Vec<&String> =
                removed.iter().filter(|f| !exclude.contains(*f)).collect();
            json!({
                "_taladb_event": "update",
                "collection": collection,
                "id": id,
                "changes": JsonValue::Object(changes_obj),
                "removed_fields": removed_fields,
                "timestamp": ts,
            })
        }
        SyncEvent::Delete { collection, id } => {
            json!({
                "_taladb_event": "delete",
                "collection": collection,
                "id": id,
                "timestamp": ts,
            })
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn doc_fields_to_json(doc: &Document, exclude: &[String]) -> JsonValue {
    let mut obj = Map::new();
    for (k, v) in &doc.fields {
        if !exclude.contains(k) {
            obj.insert(k.clone(), wasm_value_to_json(v));
        }
    }
    JsonValue::Object(obj)
}

#[cfg(target_arch = "wasm32")]
fn wasm_value_to_json(v: &Value) -> JsonValue {
    use crate::value_to_json;
    value_to_json(v)
}

/// Simple sleep using globalThis.setTimeout - works in both window and worker contexts.
#[cfg(target_arch = "wasm32")]
async fn sleep_ms_wasm(ms: u32) {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let promise = js_sys::Promise::new(&mut move |resolve, _| {
        let global = js_sys::global();
        let set_timeout = js_sys::Reflect::get(&global, &JsValue::from_str("setTimeout"))
            .expect("setTimeout not found on globalThis");
        let _ = js_sys::Reflect::apply(
            set_timeout.unchecked_ref::<js_sys::Function>(),
            &JsValue::undefined(),
            &js_sys::Array::of2(&resolve, &JsValue::from_f64(ms as f64)),
        );
    });
    let _ = JsFuture::from(promise).await;
}

/// POST `payload` once, returning the HTTP status (or `None` on a network
/// error / abort).
///
/// Uses the platform `fetch` directly rather than an HTTP client crate: this is
/// a single JSON POST inside a worker that already has `fetch`, and pulling in
/// `reqwest` for it cost the browser bundle `http`, `url`, `idna`,
/// `form_urlencoded`, `serde_urlencoded` and the `futures` facade — all of it
/// downloaded, decoded and compiled on every cold start.
///
/// The timeout is an `AbortSignal`, which also cancels the in-flight request
/// instead of merely abandoning the future.
#[cfg(target_arch = "wasm32")]
async fn fetch_post(
    endpoint: &str,
    headers: &HashMap<String, String>,
    body: &str,
    timeout_ms: u32,
) -> Option<u16> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let global = js_sys::global();
    let fetch: js_sys::Function = js_sys::Reflect::get(&global, &JsValue::from_str("fetch"))
        .ok()?
        .dyn_into()
        .ok()?;

    let header_obj = js_sys::Object::new();
    for (k, v) in headers {
        js_sys::Reflect::set(
            &header_obj,
            &JsValue::from_str(k),
            &JsValue::from_str(v),
        )
        .ok()?;
    }
    js_sys::Reflect::set(
        &header_obj,
        &JsValue::from_str("content-type"),
        &JsValue::from_str("application/json"),
    )
    .ok()?;

    let init = js_sys::Object::new();
    js_sys::Reflect::set(&init, &JsValue::from_str("method"), &JsValue::from_str("POST")).ok()?;
    js_sys::Reflect::set(&init, &JsValue::from_str("headers"), &header_obj).ok()?;
    js_sys::Reflect::set(&init, &JsValue::from_str("body"), &JsValue::from_str(body)).ok()?;

    // AbortSignal.timeout(ms) where available; harmless to omit if not.
    if let Ok(ctor) = js_sys::Reflect::get(&global, &JsValue::from_str("AbortSignal"))
        && let Ok(timeout_fn) = js_sys::Reflect::get(&ctor, &JsValue::from_str("timeout"))
        && let Some(timeout_fn) = timeout_fn.dyn_ref::<js_sys::Function>()
        && let Ok(signal) = timeout_fn.call1(&ctor, &JsValue::from_f64(timeout_ms as f64))
    {
        js_sys::Reflect::set(&init, &JsValue::from_str("signal"), &signal).ok()?;
    }

    let promise: js_sys::Promise = fetch
        .call2(&JsValue::UNDEFINED, &JsValue::from_str(endpoint), &init)
        .ok()?
        .dyn_into()
        .ok()?;
    let response = JsFuture::from(promise).await.ok()?;
    js_sys::Reflect::get(&response, &JsValue::from_str("status"))
        .ok()?
        .as_f64()
        .map(|s| s as u16)
}

/// POST `payload` with exponential-backoff retry (4 total attempts).
#[cfg(target_arch = "wasm32")]
async fn fire_wasm_with_retry(
    endpoint: &str,
    headers: &HashMap<String, String>,
    payload: &JsonValue,
) -> bool {
    const BACKOFFS_MS: &[u32] = &[200, 400, 800];
    let max_attempts = BACKOFFS_MS.len() + 1;
    let Ok(body) = serde_json::to_string(payload) else {
        return false;
    };

    for attempt in 0..max_attempts {
        if attempt > 0 {
            sleep_ms_wasm(BACKOFFS_MS[attempt - 1]).await;
        }
        match fetch_post(endpoint, headers, &body, 10_000).await {
            Some(status) if (200..300).contains(&status) => return true,
            Some(status) if status >= 500 => continue, // transient — retry
            Some(_) => return false,                   // 4xx — permanent
            None => continue,                          // network error / timeout
        }
    }
    false
}

/// Build a WASM sync hook from an optional JSON config string.
#[cfg(target_arch = "wasm32")]
fn build_wasm_sync_hook(
    config_json: Option<String>,
    health: Arc<SyncHealth>,
) -> Result<Option<Arc<dyn SyncHook>>, JsValue> {
    if let Some(json) = config_json {
        let config: TalaDbConfig =
            serde_json::from_str(&json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        config
            .validate()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        if config.sync.enabled {
            return Ok(Some(
                Arc::new(WasmSyncHook::new(config.sync, health)) as Arc<dyn SyncHook>
            ));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// WorkerDB
// ---------------------------------------------------------------------------

#[wasm_bindgen]
pub struct WorkerDB {
    db: Database,
    #[cfg(target_arch = "wasm32")]
    sync_hook: Option<Arc<dyn SyncHook>>,
    sync_health: Arc<SyncHealth>,
}

#[wasm_bindgen]
impl WorkerDB {
    // ------------------------------------------------------------------
    // Constructors
    // ------------------------------------------------------------------

    /// Open an in-memory database (for tests and OPFS-unavailable fallback).
    #[wasm_bindgen(js_name = openInMemory)]
    pub fn open_in_memory() -> Result<WorkerDB, JsValue> {
        let db = Database::open_in_memory().map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(WorkerDB {
            db,
            #[cfg(target_arch = "wasm32")]
            sync_hook: None,
            sync_health: Arc::new(SyncHealth::default()),
        })
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
    pub fn open_with_snapshot(data: Option<Vec<u8>>) -> Result<WorkerDB, JsValue> {
        let db = match data {
            Some(ref bytes) if !bytes.is_empty() => Database::restore_from_snapshot(bytes)
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            _ => Database::open_in_memory().map_err(|e| JsValue::from_str(&e.to_string()))?,
        };
        Ok(WorkerDB {
            db,
            #[cfg(target_arch = "wasm32")]
            sync_hook: None,
            sync_health: Arc::new(SyncHealth::default()),
        })
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
        let sync_health = Arc::new(SyncHealth::default());
        let sync_hook = build_wasm_sync_hook(config_json, Arc::clone(&sync_health))?;
        Ok(WorkerDB {
            db,
            sync_hook,
            sync_health,
        })
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

    #[wasm_bindgen(js_name = syncStatus)]
    pub fn sync_status(&self) -> String {
        use std::sync::atomic::Ordering;
        serde_json::json!({
            "pending": self.sync_health.pending.load(Ordering::Relaxed),
            "dropped": self.sync_health.dropped.load(Ordering::Relaxed),
            "failed": self.sync_health.failed.load(Ordering::Relaxed),
        })
        .to_string()
    }

    #[wasm_bindgen(js_name = syncPending)]
    pub fn sync_pending(&self) -> u64 {
        use std::sync::atomic::Ordering;
        self.sync_health.pending.load(Ordering::Relaxed)
    }

    /// Open a database backed by an OPFS `FileSystemSyncAccessHandle`.
    ///
    /// Not available when compiled with the `cf-workers` feature.
    ///
    /// Call sequence in the SharedWorker:
    /// ```js
    /// const handle = await file_handle.createSyncAccessHandle();
    /// const workerDb = WorkerDB.openWithOpfs(handle);
    /// ```
    #[cfg(not(feature = "cf-workers"))]
    #[wasm_bindgen(js_name = openWithOpfs)]
    pub fn open_with_opfs(sync_handle: FileSystemSyncAccessHandle) -> Result<WorkerDB, JsValue> {
        let opfs = OpfsBackend::from_handle(sync_handle);
        let redb_backend = RedbBackend::open_with_redb_backend(opfs)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let db = Database::open_with_backend(Box::new(redb_backend))
            .map_err(|e: taladb_core::TalaDbError| JsValue::from_str(&e.to_string()))?;
        Ok(WorkerDB {
            db,
            #[cfg(target_arch = "wasm32")]
            sync_hook: None,
            sync_health: Arc::new(SyncHealth::default()),
        })
    }

    /// Open a database backed by OPFS with HTTP push sync config.
    ///
    /// Not available when compiled with the `cf-workers` feature.
    ///
    /// `config_json` - JSON-serialised `TalaDbConfig`, or `null` to open without sync.
    ///
    /// ```js
    /// const handle = await file_handle.createSyncAccessHandle();
    /// const db = WorkerDB.openWithConfigAndOpfs(handle, JSON.stringify(config));
    /// ```
    #[cfg(all(target_arch = "wasm32", not(feature = "cf-workers")))]
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
        let sync_health = Arc::new(SyncHealth::default());
        let sync_hook = build_wasm_sync_hook(config_json, Arc::clone(&sync_health))?;
        Ok(WorkerDB {
            db,
            sync_hook,
            sync_health,
        })
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Get a collection handle, attaching the sync hook on write paths.
    #[cfg(target_arch = "wasm32")]
    fn get_collection(
        &self,
        name: &str,
    ) -> Result<taladb_core::Collection, taladb_core::TalaDbError> {
        let col = self.db.collection(name)?;
        if let Some(hook) = &self.sync_hook {
            Ok(col.with_sync_hook(Arc::clone(hook)))
        } else {
            Ok(col)
        }
    }

    // When not targeting WASM (e.g. cargo check on host), fall back to no-hook.
    #[cfg(not(target_arch = "wasm32"))]
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
        let id_strs: Vec<String> = ids.iter().map(|u| u.to_string()).collect();
        serde_json::to_string(&id_strs).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Upsert many documents **by caller-supplied `_id`**, in one commit.
    ///
    /// Unlike `insert_many` — which discards `_id` and mints a fresh ULID — this
    /// honours the id in each document. That is what lets the replication
    /// coordinator address a remote row by a *derived* id (see `deriveDocId`) and
    /// have repeated fetches converge on one document instead of duplicating it.
    ///
    /// `origin` is `"remote"` for authoritative rows replicated in from an origin,
    /// or `"local"` for ordinary user writes. Remote rows are marked so they can
    /// never replicate back out — see `Collection::replace_many_with_ids`.
    #[wasm_bindgen(js_name = replaceManyWithIds)]
    pub fn replace_many_with_ids(
        &self,
        collection: &str,
        docs_json: &str,
        origin: &str,
    ) -> Result<String, JsValue> {
        let arr: Vec<serde_json::Value> =
            serde_json::from_str(docs_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let docs: Result<Vec<Document>, JsValue> = arr.iter().map(json_obj_to_doc).collect();
        let ids = self
            .get_collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .replace_many_with_ids(docs?, parse_write_origin(origin)?)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let id_strs: Vec<String> = ids.iter().map(|u| u.to_string()).collect();
        serde_json::to_string(&id_strs).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Delete many documents by id, in one commit. Returns the number removed.
    #[wasm_bindgen(js_name = deleteManyWithIds)]
    pub fn delete_many_with_ids(
        &self,
        collection: &str,
        ids_json: &str,
        origin: &str,
    ) -> Result<usize, JsValue> {
        let ids: Vec<String> =
            serde_json::from_str(ids_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let ids: Result<Vec<taladb_core::Ulid>, JsValue> =
            ids.iter().map(|s| parse_ulid(s)).collect();
        self.get_collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .delete_many_with_ids(&ids?, parse_write_origin(origin)?)
            .map_err(|e| JsValue::from_str(&e.to_string()))
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
            .and_then(|v| v.as_u64())
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
    // Tombstone compaction
    // ------------------------------------------------------------------

    /// Remove tombstones older than `before_ms` from the given collection.
    ///
    /// Call periodically (e.g. on app startup) after your sync retention window
    /// has elapsed so deleted document IDs no longer accumulate indefinitely.
    /// Returns the number of tombstones removed.
    ///
    /// ```js
    /// // Prune tombstones older than 30 days
    /// const cutoff = Date.now() - 30 * 24 * 60 * 60 * 1000;
    /// const pruned = db.compactTombstones('users', cutoff);
    /// ```
    #[wasm_bindgen(js_name = compactTombstones)]
    pub fn compact_tombstones(&self, collection: &str, before_ms: f64) -> Result<u32, JsValue> {
        self.db
            .collection(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .compact_tombstones(before_ms as u64)
            .map(|n| n as u32)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    // ------------------------------------------------------------------
    // Collection introspection
    // ------------------------------------------------------------------

    /// Returns a JSON array of all collection names in the database.
    /// Used by the Worker to build the collections list for exportChangeset.
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

    // ------------------------------------------------------------------
    // Bidirectional sync — changeset export / import
    // ------------------------------------------------------------------

    /// Export a changeset for the given collections since `since_ms`.
    ///
    /// Returns a JSON string representing `Vec<Change>` that can be sent
    /// to a remote peer via fetch, WebSocket, or SSE.
    ///
    /// ```js
    /// const json = db.exportChangeset(JSON.stringify(['users', 'posts']), 0);
    /// await fetch('/sync', { method: 'POST', body: json });
    /// ```
    #[wasm_bindgen(js_name = exportChangeset)]
    pub fn export_changeset(
        &self,
        collections_json: &str,
        since_ms: f64,
    ) -> Result<String, JsValue> {
        let collections: Vec<String> = serde_json::from_str(collections_json)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let col_refs: Vec<&str> = collections.iter().map(|s| s.as_str()).collect();
        let changeset = LastWriteWins::new()
            .export_changes(&self.db, &col_refs, since_ms as u64)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        serde_json::to_string(&changeset).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Import a remote changeset and merge it into the local database using
    /// Last-Write-Wins conflict resolution.
    ///
    /// Returns the number of documents actually changed.
    ///
    /// ```js
    /// const resp = await fetch('/sync?since=' + lastSync);
    /// const applied = db.importChangeset(await resp.text());
    /// if (applied > 0) { rerender(); }
    /// ```
    #[wasm_bindgen(js_name = importChangeset)]
    pub fn import_changeset(&self, changeset_json: &str) -> Result<u32, JsValue> {
        let changeset: Changeset =
            serde_json::from_str(changeset_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let applied = LastWriteWins::new()
            .import_changes(&self.db, changeset)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(applied as u32)
    }

    /// Import a remote changeset through a tolerant structural validator built
    /// from `schemas_json` (`{ "<collection>": { version, required, types,
    /// defaults } }`). Returns a JSON `{ applied, skipped, quarantined }`.
    /// Rejected documents are set aside (see `quarantined`), never dropped.
    #[wasm_bindgen(js_name = importChangesetValidated)]
    pub fn import_changeset_validated(
        &self,
        changeset_json: &str,
        schemas_json: &str,
    ) -> Result<String, JsValue> {
        let changeset: Changeset =
            serde_json::from_str(changeset_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let schemas = build_schemas(schemas_json)?;
        let validator =
            SchemaValidator::try_new(schemas).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let report = self
            .db
            .import_changes_validated(changeset, Arc::new(validator))
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        serde_json::to_string(&serde_json::json!({
            "applied": report.applied,
            "skipped": report.skipped,
            "quarantined": report.quarantined,
        }))
        .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Documents set aside in `collection`'s quarantine table, as a JSON array
    /// of `{ document, reason, changedAt }`.
    #[wasm_bindgen(js_name = quarantined)]
    pub fn quarantined(&self, collection: &str) -> Result<String, JsValue> {
        let recs = self
            .db
            .quarantined(collection)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        let arr: Vec<_> = recs
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "document": doc_to_json(&r.document),
                    "reason": r.reason,
                    "changedAt": r.changed_at,
                })
            })
            .collect();
        serde_json::to_string(&arr).map_err(|e| JsValue::from_str(&e.to_string()))
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

fn json_obj_to_fields(v: &serde_json::Value) -> Result<Vec<(String, Value)>, JsValue> {
    match v {
        serde_json::Value::Object(map) => Ok(map
            .iter()
            .filter(|(k, _)| k.as_str() != "_id")
            .map(|(k, v)| (k.clone(), json_to_core_value(v)))
            .collect()),
        _ => Err(JsValue::from_str("document must be a JSON object")),
    }
}

fn parse_ulid(s: &str) -> Result<taladb_core::Ulid, JsValue> {
    taladb_core::Ulid::from_string(s).map_err(|_| {
        JsValue::from_str(&format!(
            "\"{s}\" is not a valid ULID. Ids for replicated rows come from \
             deriveDocId(collection, remoteKey)."
        ))
    })
}

fn parse_write_origin(origin: &str) -> Result<WriteOrigin, JsValue> {
    match origin {
        "local" => Ok(WriteOrigin::Local),
        "remote" => Ok(WriteOrigin::AuthoritativeRemote),
        other => Err(JsValue::from_str(&format!(
            "unknown write origin \"{other}\"; expected \"local\" or \"remote\""
        ))),
    }
}

/// Parse a JSON object into a [`Document`], **honouring `_id`**.
///
/// The counterpart [`json_obj_to_fields`] deliberately *drops* `_id` — the engine
/// mints its own ULID on insert. Replication needs the opposite: the id is the
/// whole point, because it is what makes an upsert idempotent across a bootstrap
/// walk and an on-demand fetch. So `_id` is required here, and a missing or
/// malformed one is a hard error rather than a silently-generated new row.
fn json_obj_to_doc(v: &serde_json::Value) -> Result<Document, JsValue> {
    let serde_json::Value::Object(map) = v else {
        return Err(JsValue::from_str("document must be a JSON object"));
    };
    let id = match map.get("_id") {
        Some(serde_json::Value::String(s)) => parse_ulid(s)?,
        _ => {
            return Err(JsValue::from_str(
                "replaceManyWithIds requires a string `_id` on every document",
            ));
        }
    };
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
        if let Some(v) = o.get("k1").and_then(|v| v.as_f64()) {
            params.k1 = v as f32;
        }
        if let Some(v) = o.get("b").and_then(|v| v.as_f64()) {
            params.b = v as f32;
        }
    }
    params
}

fn rrf_from_options(options: Option<&serde_json::Value>) -> taladb_core::bm25::RrfParams {
    let mut params = taladb_core::bm25::RrfParams::default();
    if let Some(o) = options {
        if let Some(v) = o.get("rrfK").and_then(|v| v.as_f64()) {
            params.k = v as f32;
        }
        if let Some(v) = o.get("textWeight").and_then(|v| v.as_f64()) {
            params.text_weight = v as f32;
        }
        if let Some(v) = o.get("vectorWeight").and_then(|v| v.as_f64()) {
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
