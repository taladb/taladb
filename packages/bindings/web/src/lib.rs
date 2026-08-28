mod storage;
mod worker_db;

pub use worker_db::WorkerDB;

use std::sync::Arc;

use serde::Serialize;
use serde_wasm_bindgen::{from_value, to_value};
use taladb_core::{
    Collection, Database, Filter, HnswOptions, TalaDbError, Update, Value, VectorMetric,
};
use wasm_bindgen::prelude::*;

/// Serialize a query result into a JS value as **plain objects**.
///
/// `serde_wasm_bindgen`'s default emits JS `Map`s for maps/structs, so
/// `result.document` would be `undefined` and callers would need `.get(...)`.
/// The `json_compatible` serializer emits ordinary objects, matching both the
/// WorkerDB path (which round-trips through JSON) and what the TypeScript
/// client expects.
fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

pub use storage::opfs::{is_opfs_available, opfs_delete_snapshot, opfs_load_snapshot};

fn err_to_js(e: TalaDbError) -> JsValue {
    let msg = e.to_string();
    let code = e.code();
    let obj = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&obj, &"error".into(), &JsValue::from_str(&msg));
    let _ = js_sys::Reflect::set(&obj, &"code".into(), &JsValue::from_str(code));
    obj.into()
}

/// Initialize panic hook for better error messages in the browser console.
#[wasm_bindgen(start)]
pub fn init() {
    console_error_panic_hook::set_once();
}

// ---------------------------------------------------------------------------
// TalaDBWasm — the top-level database handle exposed to JavaScript
// ---------------------------------------------------------------------------

#[wasm_bindgen]
pub struct TalaDBWasm {
    inner: Arc<Database>,
}

#[wasm_bindgen]
impl TalaDBWasm {
    /// Open an in-memory database (suitable for tests and environments without OPFS).
    #[wasm_bindgen(js_name = openInMemory)]
    pub fn open_in_memory() -> Result<Self, JsValue> {
        let db = Database::open_in_memory().map_err(err_to_js)?;
        Ok(Self {
            inner: Arc::new(db),
        })
    }

    /// Open a database, restoring from a previously exported snapshot if provided.
    ///
    /// Pass the bytes returned by `opfs_load_snapshot` (or `null`/`undefined` for
    /// a fresh empty database).  After each write, call `exportSnapshot()` and
    /// pass the bytes to `opfs_flush_snapshot` to persist across page reloads.
    ///
    /// ```js
    /// const bytes = await opfs_load_snapshot('myapp.db');   // null on first open
    /// const db = TalaDBWasm.openWithSnapshot(bytes);
    /// // ... mutations ...
    /// await opfs_flush_snapshot('myapp.db', db.exportSnapshot());
    /// ```
    #[wasm_bindgen(js_name = openWithSnapshot)]
    pub fn open_with_snapshot(snapshot: Option<Vec<u8>>) -> Result<Self, JsValue> {
        let db = match snapshot {
            Some(ref data) if !data.is_empty() => {
                Database::restore_from_snapshot(data).map_err(err_to_js)?
            }
            _ => Database::open_in_memory().map_err(err_to_js)?,
        };
        Ok(Self {
            inner: Arc::new(db),
        })
    }

    /// Serialize the entire in-memory database to bytes.
    ///
    /// Pass the returned `Uint8Array` to `opfs_flush_snapshot` to persist, or
    /// store it yourself.  On the next page load, pass the same bytes to
    /// `openWithSnapshot` to restore all data.
    #[wasm_bindgen(js_name = exportSnapshot)]
    pub fn export_snapshot(&self) -> Result<Vec<u8>, JsValue> {
        self.inner.export_snapshot().map_err(err_to_js)
    }

    /// Get a collection handle by name.
    pub fn collection(&self, name: &str) -> Result<CollectionWasm, JsValue> {
        let col = self.inner.collection(name).map_err(err_to_js)?;
        Ok(CollectionWasm { inner: col })
    }

    /// User collection names (reserved `_`-prefixed collections excluded).
    #[wasm_bindgen(js_name = listCollectionNames)]
    pub fn list_collection_names(&self) -> Result<Vec<String>, JsValue> {
        self.inner.list_collection_names().map_err(err_to_js)
    }

    /// Read the current application migration version (0 if never set). Backs
    /// the `openDB({ migrations })` runner, which advances it per migration.
    #[wasm_bindgen(js_name = userVersion)]
    pub fn user_version(&self) -> Result<u32, JsValue> {
        self.inner.user_version().map_err(err_to_js)
    }

    /// Persist the application migration version. Called after each migration's
    /// body succeeds so a crash mid-run resumes from the last applied version.
    #[wasm_bindgen(js_name = setUserVersion)]
    pub fn set_user_version(&self, version: u32) -> Result<(), JsValue> {
        self.inner.set_user_version(version).map_err(err_to_js)
    }
}

// ---------------------------------------------------------------------------
// CollectionWasm — exposes insert/find/update/delete to JavaScript
// ---------------------------------------------------------------------------

#[wasm_bindgen]
pub struct CollectionWasm {
    inner: Collection,
}

#[wasm_bindgen]
impl CollectionWasm {
    /// Insert a document. Accepts a plain JS object, returns the ULID string id.
    pub fn insert(&self, doc: JsValue) -> Result<String, JsValue> {
        let fields = js_object_to_fields(doc)?;
        let id = self.inner.insert(fields).map_err(err_to_js)?;
        Ok(id.to_string())
    }

    /// Insert multiple documents. Returns an array of ULID string ids.
    #[wasm_bindgen(js_name = insertMany)]
    pub fn insert_many(&self, docs: JsValue) -> Result<JsValue, JsValue> {
        let arr = js_sys::Array::from(&docs);
        let items: Result<Vec<Vec<(String, Value)>>, JsValue> =
            arr.iter().map(js_object_to_fields).collect();
        let ids = self.inner.insert_many(items?).map_err(err_to_js)?;
        let id_strings: Vec<String> = ids.iter().map(taladb_core::Ulid::to_string).collect();
        to_value(&id_strings).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Find documents matching the filter. Returns a JS array of plain objects.
    pub fn find(&self, filter: JsValue) -> Result<JsValue, JsValue> {
        let f = js_to_filter(filter)?;
        let docs = self.inner.find(f).map_err(err_to_js)?;
        let result: Vec<serde_json::Value> = docs.iter().map(doc_to_json).collect();
        to_js(&result)
    }

    /// Find a single document. Returns the document or null.
    #[wasm_bindgen(js_name = findOne)]
    pub fn find_one(&self, filter: JsValue) -> Result<JsValue, JsValue> {
        let f = js_to_filter(filter)?;
        match self.inner.find_one(f).map_err(err_to_js)? {
            Some(doc) => {
                let json = doc_to_json(&doc);
                to_js(&json)
            }
            None => Ok(JsValue::NULL),
        }
    }

    /// Update the first matching document. Returns true if a document was updated.
    #[wasm_bindgen(js_name = updateOne)]
    pub fn update_one(&self, filter: JsValue, update: JsValue) -> Result<bool, JsValue> {
        let f = js_to_filter(filter)?;
        let u = js_to_update(update)?;
        self.inner.update_one(f, u).map_err(err_to_js)
    }

    /// Update all matching documents. Returns the count updated.
    #[wasm_bindgen(js_name = updateMany)]
    pub fn update_many(&self, filter: JsValue, update: JsValue) -> Result<u32, JsValue> {
        let f = js_to_filter(filter)?;
        let u = js_to_update(update)?;
        let n = self.inner.update_many(f, u).map_err(err_to_js)?;
        Ok(n as u32)
    }

    /// Delete the first matching document. Returns true if deleted.
    #[wasm_bindgen(js_name = deleteOne)]
    pub fn delete_one(&self, filter: JsValue) -> Result<bool, JsValue> {
        let f = js_to_filter(filter)?;
        self.inner.delete_one(f).map_err(err_to_js)
    }

    /// Delete all matching documents. Returns the count deleted.
    #[wasm_bindgen(js_name = deleteMany)]
    pub fn delete_many(&self, filter: JsValue) -> Result<u32, JsValue> {
        let f = js_to_filter(filter)?;
        let n = self.inner.delete_many(f).map_err(err_to_js)?;
        Ok(n as u32)
    }

    /// Count documents matching the filter.
    pub fn count(&self, filter: JsValue) -> Result<u32, JsValue> {
        let f = js_to_filter(filter)?;
        let n = self.inner.count(f).map_err(err_to_js)?;
        Ok(n as u32)
    }

    /// Run a MongoDB-style aggregation pipeline (`$match`, `$group`, `$sort`,
    /// `$skip`, `$limit`, `$project`). Returns the resulting documents.
    pub fn aggregate(&self, pipeline: JsValue) -> Result<JsValue, JsValue> {
        let json = js_to_json(pipeline)?;
        let pl = taladb_core::aggregate::parse_pipeline(&json, &|v| {
            json_to_filter(v).ok_or_else(|| "invalid $match filter".to_string())
        })
        .map_err(|e| JsValue::from_str(&e))?;
        let docs = self.inner.aggregate(pl).map_err(err_to_js)?;
        let result: Vec<serde_json::Value> = docs.iter().map(doc_to_json).collect();
        to_js(&result)
    }

    /// Create a secondary index on a field.
    #[wasm_bindgen(js_name = createIndex)]
    pub fn create_index(&self, field: &str) -> Result<(), JsValue> {
        self.inner.create_index(field).map_err(err_to_js)
    }

    /// Drop a secondary index.
    #[wasm_bindgen(js_name = dropIndex)]
    pub fn drop_index(&self, field: &str) -> Result<(), JsValue> {
        self.inner.drop_index(field).map_err(err_to_js)
    }

    /// Create a compound index. `fields_json` is a JSON array of field names.
    #[wasm_bindgen(js_name = createCompoundIndex)]
    pub fn create_compound_index(&self, fields_json: &str) -> Result<(), JsValue> {
        let fields: Vec<String> =
            serde_json::from_str(fields_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        self.inner.create_compound_index(&refs).map_err(err_to_js)
    }

    /// Drop a compound index by its ordered field list (`fields_json`).
    #[wasm_bindgen(js_name = dropCompoundIndex)]
    pub fn drop_compound_index(&self, fields_json: &str) -> Result<(), JsValue> {
        let fields: Vec<String> =
            serde_json::from_str(fields_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        self.inner.drop_compound_index(&refs).map_err(err_to_js)
    }

    /// Create a vector index on `field`.
    ///
    /// `dimensions`           - expected vector length.
    /// `metric`               - optional: `"cosine"` (default), `"dot"`, or `"euclidean"`.
    /// `index_type`           - optional: `"flat"` (default) or `"hnsw"`.
    /// `hnsw_m`               - HNSW connectivity (default 16).
    /// `hnsw_ef_construction` - build quality (default 200).
    #[wasm_bindgen(js_name = createVectorIndex)]
    pub fn create_vector_index(
        &self,
        field: &str,
        dimensions: u32,
        metric: Option<String>,
        index_type: Option<String>,
        hnsw_m: Option<u32>,
        hnsw_ef_construction: Option<u32>,
    ) -> Result<(), JsValue> {
        let m = parse_metric(metric)?;
        let hnsw = parse_hnsw_opts(index_type, hnsw_m, hnsw_ef_construction);
        self.inner
            .create_vector_index(field, dimensions as usize, m, hnsw)
            .map_err(err_to_js)
    }

    /// Drop a vector index (and its HNSW graph if present).
    #[wasm_bindgen(js_name = dropVectorIndex)]
    pub fn drop_vector_index(&self, field: &str) -> Result<(), JsValue> {
        self.inner.drop_vector_index(field).map_err(err_to_js)
    }

    /// Rebuild the HNSW graph from the current flat vector table.
    #[wasm_bindgen(js_name = upgradeVectorIndex)]
    pub fn upgrade_vector_index(&self, field: &str) -> Result<(), JsValue> {
        self.inner.upgrade_vector_index(field).map_err(err_to_js)
    }

    /// Create a full-text search index on a string field.
    #[wasm_bindgen(js_name = createFtsIndex)]
    pub fn create_fts_index(&self, field: &str) -> Result<(), JsValue> {
        self.inner.create_fts_index(field).map_err(err_to_js)
    }

    /// Drop a full-text search index and everything it stores.
    #[wasm_bindgen(js_name = dropFtsIndex)]
    pub fn drop_fts_index(&self, field: &str) -> Result<(), JsValue> {
        self.inner.drop_fts_index(field).map_err(err_to_js)
    }

    /// Rank documents against a free-text query using BM25 (OR semantics).
    ///
    /// Returns a JSON array of `{ document, score }`.
    #[wasm_bindgen(js_name = searchText)]
    pub fn search_text(
        &self,
        field: &str,
        query: &str,
        top_k: u32,
        filter: JsValue,
        options: JsValue,
    ) -> Result<JsValue, JsValue> {
        let pre_filter = if filter.is_null() || filter.is_undefined() {
            None
        } else {
            Some(js_to_filter(filter)?)
        };
        let opts = js_options_to_json(options);
        let bm25 = bm25_from_options(opts.as_ref());

        let results = self
            .inner
            .search_text_with(field, query, top_k as usize, &bm25, pre_filter)
            .map_err(err_to_js)?;

        let json: Vec<serde_json::Value> = results
            .iter()
            .map(|r| serde_json::json!({ "document": doc_to_json(&r.document), "score": r.score }))
            .collect();
        to_js(&json)
    }

    /// Hybrid retrieval — BM25 and vector similarity fused with reciprocal
    /// rank fusion.
    ///
    /// `options` accepts `{ rrfK, textWeight, vectorWeight, candidates, k1, b }`.
    /// Returns a JSON array of `{ document, score, textRank, vectorRank }`.
    #[allow(clippy::too_many_arguments)]
    #[wasm_bindgen(js_name = hybridSearch)]
    pub fn hybrid_search(
        &self,
        text_field: &str,
        text: &str,
        vector_field: &str,
        vector: Vec<f32>,
        top_k: u32,
        filter: JsValue,
        options: JsValue,
    ) -> Result<JsValue, JsValue> {
        let pre_filter = if filter.is_null() || filter.is_undefined() {
            None
        } else {
            Some(js_to_filter(filter)?)
        };
        let opts = js_options_to_json(options);

        let mut query = taladb_core::fts::HybridQuery::new(
            text_field,
            text,
            vector_field,
            &vector,
            top_k as usize,
        );
        query.filter = pre_filter;
        query.bm25 = bm25_from_options(opts.as_ref());
        query.rrf = rrf_from_options(opts.as_ref());
        query.candidates = opts
            .as_ref()
            .and_then(|o| o.get("candidates"))
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as usize);

        let results = self.inner.hybrid_search(query).map_err(err_to_js)?;

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
        to_js(&json)
    }

    /// Find the `top_k` nearest documents to `query` on a vector index.
    ///
    /// `filter` - optional pre-filter (same format as `find`). Pass `null` to
    ///            search across all documents that have the vector field.
    ///
    /// Returns a JSON array of `{ document: {...}, score: number }` objects.
    #[wasm_bindgen(js_name = findNearest)]
    pub fn find_nearest(
        &self,
        field: &str,
        query: Vec<f32>,
        top_k: u32,
        filter: JsValue,
    ) -> Result<JsValue, JsValue> {
        let pre_filter = if filter.is_null() || filter.is_undefined() {
            None
        } else {
            Some(js_to_filter(filter)?)
        };
        let results = self
            .inner
            .find_nearest(field, &query, top_k as usize, pre_filter)
            .map_err(err_to_js)?;

        let json: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "document": doc_to_json(&r.document),
                    "score": r.score,
                })
            })
            .collect();
        to_js(&json)
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers: JS ↔ Rust
// ---------------------------------------------------------------------------

/// `serde_wasm_bindgen::from_value` plus a nesting check.
///
/// Every JS value that goes on to a recursive converter comes through here.
/// `from_value` imposes no depth limit of its own — unlike `serde_json`'s string
/// parser, which stops at 128 — so without this a caller-supplied object graph
/// sets the recursion depth of `json_to_filter`, `json_to_value` and friends.
///
/// One function rather than a check at each call site: a new `from_value` call
/// added later gets the guard by using this, and the reviewer's question becomes
/// "why is this one not using `js_to_json`?" instead of "is a check missing?".
fn js_to_json(val: JsValue) -> Result<serde_json::Value, JsValue> {
    // Measured on the *JavaScript* value, before anything deserialises it.
    //
    // Checking the `serde_json::Value` afterwards — which is what this used to do
    // — bounds what reaches the filter and update parsers, but leaves two
    // recursions on either side of the guard, neither of them ours:
    //
    //   building   `serde_wasm_bindgen::from_value` walks a nested JS object by
    //              recursing into it, and imposes no depth limit, so a deep
    //              enough input overflows before the check is ever reached.
    //   dropping   `serde_json::Value` has a recursive `Drop`, so *returning the
    //              error* could overflow instead. Measured on an 8 MiB stack: a
    //              30,000-deep value builds, checks and drops cleanly; 50,000
    //              survives construction and the check, then dies in the drop.
    //
    // Rejecting here closes both. `from_value` is only ever handed something
    // within the ceiling, so its recursion is bounded at 64 frames, and the value
    // it produces is shallow enough to drop safely.
    check_js_depth(&val)?;

    let json: serde_json::Value = from_value(val).map_err(|e| JsValue::from_str(&e.to_string()))?;
    // Still checked on the Rust side: `check_js_depth` and the deserialiser could
    // in principle disagree about what counts as a level, and this is the cheap
    // assertion that they did not.
    taladb_core::json_depth::check_json_depth(&json).map_err(err_to_js)?;
    Ok(json)
}

/// Reject a JS value nested deeper than the engine's ceiling, without recursing.
///
/// An explicit stack, for the same reason the core's checker uses one: a
/// recursive walker would overflow on exactly the input it exists to reject.
///
/// Cycles are handled by the depth limit rather than by tracking visited nodes —
/// a cycle necessarily revisits a node at a greater depth on every lap, so it
/// trips the ceiling within 64 of them.
fn check_js_depth(root: &JsValue) -> Result<(), JsValue> {
    use taladb_core::json_depth::MAX_JSON_DEPTH;

    // (value, depth-of-that-value). Depth 1 is the root.
    let mut stack: Vec<(JsValue, usize)> = vec![(root.clone(), 1)];

    while let Some((value, depth)) = stack.pop() {
        if depth > MAX_JSON_DEPTH {
            return Err(JsValue::from_str(&format!(
                "JSON nested deeper than {MAX_JSON_DEPTH} levels"
            )));
        }
        // `typeof null` is "object", so this has to come first.
        if value.is_null() || value.is_undefined() {
            continue;
        }
        if js_sys::Array::is_array(&value) {
            let array = js_sys::Array::from(&value);
            for i in 0..array.length() {
                stack.push((array.get(i), depth + 1));
            }
        } else if value.is_object() {
            // Own enumerable values only — the same set `from_value` will walk.
            let values = js_sys::Object::values(value.unchecked_ref::<js_sys::Object>());
            for i in 0..values.length() {
                stack.push((values.get(i), depth + 1));
            }
        }
        // Anything else is a scalar and ends the branch.
    }
    Ok(())
}

/// Convert a JS object like { name: "Alice", age: 30 } to Vec<(String, Value)>.
fn js_object_to_fields(val: JsValue) -> Result<Vec<(String, Value)>, JsValue> {
    let json = js_to_json(val)?;
    match json {
        serde_json::Value::Object(map) => Ok(map
            .into_iter()
            .map(|(k, v)| (k, json_to_value(v)))
            .collect()),
        _ => Err(JsValue::from_str("document must be a plain object")),
    }
}

fn json_to_value(j: serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::Str(s),
        serde_json::Value::Array(arr) => Value::Array(arr.into_iter().map(json_to_value).collect()),
        serde_json::Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, json_to_value(v)))
                .collect(),
        ),
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Bytes(b) => serde_json::Value::String(format!("<bytes:{}>", b.len())),
        Value::Array(arr) => serde_json::Value::Array(arr.iter().map(value_to_json).collect()),
        Value::Object(obj) => serde_json::Value::Object(
            obj.iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
    }
}

pub(crate) fn doc_to_json(doc: &taladb_core::Document) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("_id".into(), serde_json::Value::String(doc.id.to_string()));
    for (k, v) in &doc.fields {
        map.insert(k.clone(), value_to_json(v));
    }
    serde_json::Value::Object(map)
}

/// Convert a JS filter object to a Rust Filter.
/// Supports: { field: value }  →  Eq
///           { field: { $gt, $gte, $lt, $lte, $ne, $in, $nin, $exists } }
///           { $and: [...] }, { $or: [...] }, { $not: { ... } }
/// Convert an optional JS options object into plain JSON, treating
/// `null`/`undefined`/unparseable as "use defaults" rather than an error —
/// tuning knobs should never be the thing that fails a query.
fn js_options_to_json(options: JsValue) -> Option<serde_json::Value> {
    if options.is_null() || options.is_undefined() {
        return None;
    }
    serde_wasm_bindgen::from_value::<serde_json::Value>(options)
        .ok()
        .filter(|v| !v.is_null())
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

fn js_to_filter(val: JsValue) -> Result<Filter, JsValue> {
    if val.is_null() || val.is_undefined() {
        return Ok(Filter::All);
    }
    let json = js_to_json(val)?;
    json_to_filter(&json).ok_or_else(|| JsValue::from_str("invalid filter"))
}

fn json_to_filter(json: &serde_json::Value) -> Option<Filter> {
    let obj = json.as_object()?;

    // Field-level operators
    let mut filters: Vec<Filter> = Vec::new();
    for (field, expr) in obj {
        let f = match field.as_str() {
            "$and" => Filter::And(
                expr.as_array()?
                    .iter()
                    .map(json_to_filter)
                    .collect::<Option<_>>()?,
            ),
            "$or" => Filter::Or(
                expr.as_array()?
                    .iter()
                    .map(json_to_filter)
                    .collect::<Option<_>>()?,
            ),
            "$not" => Filter::Not(Box::new(json_to_filter(expr)?)),
            op if op.starts_with('$') => return None,
            _ => parse_field_filter(field, expr)?,
        };
        filters.push(f);
    }

    match filters.len() {
        0 => Some(Filter::All),
        1 => Some(filters.remove(0)),
        _ => Some(Filter::And(filters)),
    }
}

fn parse_field_filter(field: &str, expr: &serde_json::Value) -> Option<Filter> {
    // Simple equality: { age: 30 }
    if !expr.is_object() {
        return Some(Filter::Eq(field.to_string(), json_to_value(expr.clone())));
    }

    let ops = expr.as_object()?;
    let mut filters = Vec::new();

    for (op, val) in ops {
        let v = json_to_value(val.clone());
        let f = match op.as_str() {
            "$eq" => Filter::Eq(field.to_string(), v),
            "$ne" => Filter::Ne(field.to_string(), v),
            "$gt" => Filter::Gt(field.to_string(), v),
            "$gte" => Filter::Gte(field.to_string(), v),
            "$lt" => Filter::Lt(field.to_string(), v),
            "$lte" => Filter::Lte(field.to_string(), v),
            "$in" => {
                let arr = val
                    .as_array()?
                    .iter()
                    .map(|v| json_to_value(v.clone()))
                    .collect();
                Filter::In(field.to_string(), arr)
            }
            "$nin" => {
                let arr = val
                    .as_array()?
                    .iter()
                    .map(|v| json_to_value(v.clone()))
                    .collect();
                Filter::Nin(field.to_string(), arr)
            }
            "$exists" => Filter::Exists(field.to_string(), val.as_bool()?),
            "$contains" => Filter::Contains(field.to_string(), val.as_str()?.to_string()),
            "$regex" => Filter::Regex(field.to_string(), val.as_str()?.to_string()),
            _ => return None,
        };
        filters.push(f);
    }

    match filters.len() {
        0 => None,
        1 => Some(filters.remove(0)),
        _ => Some(Filter::And(filters)),
    }
}

/// Convert a JS update object to a Rust Update.
/// Supports: { $set: {...} }, { $unset: {...} }, { $inc: {...} },
///           { $push: { field: val } }, { $pull: { field: val } }
fn js_to_update(val: JsValue) -> Result<Update, JsValue> {
    let json = js_to_json(val)?;
    let obj = json
        .as_object()
        .ok_or_else(|| JsValue::from_str("update must be an object"))?;

    let mut updates = Vec::new();
    if let Some(set_obj) = obj.get("$set") {
        let pairs = set_obj
            .as_object()
            .ok_or_else(|| JsValue::from_str("$set must be an object"))?
            .iter()
            .map(|(k, v)| (k.clone(), json_to_value(v.clone())))
            .collect();
        updates.push(Update::Set(pairs));
    }
    if let Some(unset_obj) = obj.get("$unset") {
        let keys = unset_obj
            .as_object()
            .ok_or_else(|| JsValue::from_str("$unset must be an object"))?
            .keys()
            .cloned()
            .collect();
        updates.push(Update::Unset(keys));
    }
    if let Some(inc_obj) = obj.get("$inc") {
        let pairs = inc_obj
            .as_object()
            .ok_or_else(|| JsValue::from_str("$inc must be an object"))?
            .iter()
            .map(|(k, v)| (k.clone(), json_to_value(v.clone())))
            .collect();
        updates.push(Update::Inc(pairs));
    }
    if let Some(push_obj) = obj.get("$push") {
        let map = push_obj
            .as_object()
            .ok_or_else(|| JsValue::from_str("$push must be an object"))?;
        updates.extend(
            map.iter()
                .map(|(k, v)| Update::Push(k.clone(), json_to_value(v.clone()))),
        );
    }
    if let Some(pull_obj) = obj.get("$pull") {
        let map = pull_obj
            .as_object()
            .ok_or_else(|| JsValue::from_str("$pull must be an object"))?;
        updates.extend(
            map.iter()
                .map(|(k, v)| Update::Pull(k.clone(), json_to_value(v.clone()))),
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
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// WASM tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    // Not `run_in_browser`. Every test in this crate passes under node — none of
    // them touch the DOM, OPFS or a worker — and requiring a browser meant
    // requiring a webdriver, which is why nothing in CI has ever run them. There
    // were twelve such tests sitting here looking like coverage.
    //
    // If a test is added that genuinely needs browser APIs, put it in its own
    // module with its own `wasm_bindgen_test_configure!(run_in_browser)` rather
    // than reinstating it here, so it does not take the rest with it.

    #[wasm_bindgen_test]
    fn in_memory_insert_find() {
        let db = TalaDBWasm::open_in_memory().unwrap();
        let col = db.collection("users").unwrap();

        let doc = js_sys::Object::new();
        js_sys::Reflect::set(&doc, &"name".into(), &"Alice".into()).unwrap();
        js_sys::Reflect::set(&doc, &"age".into(), &JsValue::from_f64(30.0)).unwrap();

        let id = col.insert(doc.into()).unwrap();
        assert!(!id.is_empty());

        let results = col.find(JsValue::NULL).unwrap();
        let arr = js_sys::Array::from(&results);
        assert_eq!(arr.length(), 1);
    }
}

// ---------------------------------------------------------------------------
// Depth guard on the JavaScript side
//
// Deliberately not `run_in_browser`: this walks a plain object graph and touches
// no DOM, OPFS or worker API, so it runs under node. That matters — the browser
// tests in this crate need a webdriver, and nothing in CI currently runs them, so
// a test that insists on a browser is a test that does not execute.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod js_depth_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Build exactly `depth` levels of nesting, without recursing.
    ///
    /// `1..depth`, not `0..depth`: the innermost `null` is itself a level, so
    /// `depth` levels means `depth - 1` arrays around it. Matching the core's
    /// `nest_arrays` helper exactly is the point — these two tests assert the same
    /// boundary from opposite sides of the binding, and they are only comparable
    /// if "depth" counts the same thing on both. Running the test is what caught
    /// this; it compiled happily while asserting the wrong boundary.
    fn nest(depth: usize) -> JsValue {
        let mut v = JsValue::NULL;
        for _ in 1..depth {
            let a = js_sys::Array::new();
            a.push(&v);
            v = a.into();
        }
        v
    }

    #[wasm_bindgen_test]
    fn ordinary_shapes_are_accepted() {
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &"status".into(), &"active".into()).unwrap();
        let tags = js_sys::Array::new();
        tags.push(&"a".into());
        tags.push(&"b".into());
        js_sys::Reflect::set(&obj, &"tags".into(), &tags).unwrap();

        assert!(check_js_depth(&obj.into()).is_ok());
        assert!(check_js_depth(&JsValue::NULL).is_ok());
        assert!(check_js_depth(&JsValue::UNDEFINED).is_ok());
        assert!(check_js_depth(&JsValue::from_f64(1.0)).is_ok());
    }

    #[wasm_bindgen_test]
    fn the_boundary_matches_the_rust_side() {
        use taladb_core::json_depth::MAX_JSON_DEPTH;
        assert!(check_js_depth(&nest(MAX_JSON_DEPTH)).is_ok());
        assert!(check_js_depth(&nest(MAX_JSON_DEPTH + 1)).is_err());
    }

    /// The whole point of doing this before `from_value`: a value deep enough to
    /// overflow the deserialiser must be rejected without ever reaching it, and
    /// without the check itself recursing.
    #[wasm_bindgen_test]
    fn a_payload_deep_enough_to_overflow_the_deserialiser_is_refused() {
        let bomb = nest(100_000);
        assert!(
            check_js_depth(&bomb).is_err(),
            "a 100k-deep value must be rejected by the iterative walk"
        );
    }

    /// A cycle would spin forever in a naive walker. The depth ceiling ends it,
    /// which is why no visited-set is needed.
    #[wasm_bindgen_test]
    fn a_cyclic_object_terminates() {
        let a = js_sys::Array::new();
        let b = js_sys::Array::new();
        a.push(&b);
        b.push(&a);
        assert!(
            check_js_depth(&a.into()).is_err(),
            "a cycle must be rejected by the depth ceiling, not loop"
        );
    }

    #[wasm_bindgen_test]
    fn wide_is_not_deep() {
        let wide = js_sys::Array::new();
        for i in 0..5_000 {
            wide.push(&JsValue::from_f64(f64::from(i)));
        }
        assert!(check_js_depth(&wide.into()).is_ok());
    }
}
