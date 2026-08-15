use napi::bindgen_prelude::{AsyncTask, Float32Array};
use napi::{Env, Task};
use napi_derive::napi;
use serde_json::Value as JsonValue;
use std::sync::Arc;
use taladb_core::{
    Collection, Database, Filter, HnswOptions, TalaDbConfig, TalaDbError, Update, Value,
    VectorMetric, VectorSearchResult,
};

fn err_to_napi(e: TalaDbError) -> napi::Error {
    napi::Error::from_reason(format!("{}: {}", e.code(), e))
}

// ---------------------------------------------------------------------------
// Helpers: JSON ↔ taladb_core::Value
// ---------------------------------------------------------------------------

fn json_to_value(j: JsonValue) -> Value {
    match j {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Bool(b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        JsonValue::String(s) => Value::Str(s),
        JsonValue::Array(arr) => Value::Array(arr.into_iter().map(json_to_value).collect()),
        JsonValue::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, json_to_value(v)))
                .collect(),
        ),
    }
}

fn value_to_json(v: &Value) -> JsonValue {
    match v {
        Value::Null => JsonValue::Null,
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::Int(n) => JsonValue::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        Value::Str(s) => JsonValue::String(s.clone()),
        Value::Bytes(b) => JsonValue::String(format!("<bytes:{}>", b.len())),
        Value::Array(arr) => JsonValue::Array(arr.iter().map(value_to_json).collect()),
        Value::Object(obj) => JsonValue::Object(
            obj.iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
    }
}

fn doc_to_json(doc: &taladb_core::Document) -> JsonValue {
    let mut map = serde_json::Map::new();
    map.insert("_id".into(), JsonValue::String(doc.id.to_string()));
    for (k, v) in &doc.fields {
        map.insert(k.clone(), value_to_json(v));
    }
    JsonValue::Object(map)
}

fn obj_to_fields(json: JsonValue) -> napi::Result<Vec<(String, Value)>> {
    match json {
        JsonValue::Object(map) => Ok(map
            .into_iter()
            .map(|(k, v)| (k, json_to_value(v)))
            .collect()),
        _ => Err(napi::Error::from_reason("document must be a plain object")),
    }
}

fn json_to_filter(json: &JsonValue) -> napi::Result<Filter> {
    if json.is_null() {
        return Ok(Filter::All);
    }
    let obj = json
        .as_object()
        .ok_or_else(|| napi::Error::from_reason("filter must be an object"))?;

    let mut filters = Vec::new();
    for (field, expr) in obj {
        let f = match field.as_str() {
            "$and" => Filter::And(
                expr.as_array()
                    .ok_or_else(|| napi::Error::from_reason("$and must be an array"))?
                    .iter()
                    .map(json_to_filter)
                    .collect::<napi::Result<_>>()?,
            ),
            "$or" => Filter::Or(
                expr.as_array()
                    .ok_or_else(|| napi::Error::from_reason("$or must be an array"))?
                    .iter()
                    .map(json_to_filter)
                    .collect::<napi::Result<_>>()?,
            ),
            "$not" => Filter::Not(Box::new(json_to_filter(expr)?)),
            op if op.starts_with('$') => {
                return Err(napi::Error::from_reason(format!(
                    "unknown logical operator: {op}"
                )));
            }
            _ => parse_field_filter(field, expr)?,
        };
        filters.push(f);
    }

    match filters.len() {
        0 => Ok(Filter::All),
        1 => Ok(filters.remove(0)),
        _ => Ok(Filter::And(filters)),
    }
}

fn parse_field_filter(field: &str, expr: &JsonValue) -> napi::Result<Filter> {
    if !expr.is_object() {
        return Ok(Filter::Eq(field.to_string(), json_to_value(expr.clone())));
    }
    let ops = expr.as_object().unwrap();
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
                    .as_array()
                    .ok_or_else(|| napi::Error::from_reason("$in must be array"))?;
                Filter::In(
                    field.to_string(),
                    arr.iter().map(|v| json_to_value(v.clone())).collect(),
                )
            }
            "$nin" => {
                let arr = val
                    .as_array()
                    .ok_or_else(|| napi::Error::from_reason("$nin must be array"))?;
                Filter::Nin(
                    field.to_string(),
                    arr.iter().map(|v| json_to_value(v.clone())).collect(),
                )
            }
            "$exists" => Filter::Exists(
                field.to_string(),
                val.as_bool()
                    .ok_or_else(|| napi::Error::from_reason("$exists must be a boolean"))?,
            ),
            "$contains" => Filter::Contains(
                field.to_string(),
                val.as_str()
                    .ok_or_else(|| napi::Error::from_reason("$contains must be a string"))?
                    .to_string(),
            ),
            "$regex" => Filter::Regex(
                field.to_string(),
                val.as_str()
                    .ok_or_else(|| napi::Error::from_reason("$regex must be a string"))?
                    .to_string(),
            ),
            _ => {
                return Err(napi::Error::from_reason(format!("unknown operator: {op}")));
            }
        };
        filters.push(f);
    }
    match filters.len() {
        0 => Err(napi::Error::from_reason("empty field filter")),
        1 => Ok(filters.remove(0)),
        _ => Ok(Filter::And(filters)),
    }
}

fn json_to_update(json: JsonValue) -> napi::Result<Update> {
    let obj = json
        .as_object()
        .ok_or_else(|| napi::Error::from_reason("update must be an object"))?;

    let mut updates = Vec::new();
    if let Some(set_obj) = obj.get("$set") {
        let pairs = set_obj
            .as_object()
            .ok_or_else(|| napi::Error::from_reason("$set must be an object"))?
            .iter()
            .map(|(k, v)| (k.clone(), json_to_value(v.clone())))
            .collect();
        updates.push(Update::Set(pairs));
    }
    if let Some(unset_obj) = obj.get("$unset") {
        let keys = unset_obj
            .as_object()
            .ok_or_else(|| napi::Error::from_reason("$unset must be an object"))?
            .keys()
            .cloned()
            .collect();
        updates.push(Update::Unset(keys));
    }
    if let Some(inc_obj) = obj.get("$inc") {
        let pairs = inc_obj
            .as_object()
            .ok_or_else(|| napi::Error::from_reason("$inc must be an object"))?
            .iter()
            .map(|(k, v)| (k.clone(), json_to_value(v.clone())))
            .collect();
        updates.push(Update::Inc(pairs));
    }
    if let Some(push_obj) = obj.get("$push") {
        let map = push_obj
            .as_object()
            .ok_or_else(|| napi::Error::from_reason("$push must be an object"))?;
        updates.extend(
            map.iter()
                .map(|(k, v)| Update::Push(k.clone(), json_to_value(v.clone()))),
        );
    }
    if let Some(pull_obj) = obj.get("$pull") {
        let map = pull_obj
            .as_object()
            .ok_or_else(|| napi::Error::from_reason("$pull must be an object"))?;
        updates.extend(
            map.iter()
                .map(|(k, v)| Update::Pull(k.clone(), json_to_value(v.clone()))),
        );
    }
    if obj
        .keys()
        .any(|k| !matches!(k.as_str(), "$set" | "$unset" | "$inc" | "$push" | "$pull"))
    {
        return Err(napi::Error::from_reason("unsupported update operator"));
    }
    match updates.len() {
        0 => Err(napi::Error::from_reason("update must contain an operator")),
        1 => Ok(updates.remove(0)),
        _ => Ok(Update::Many(updates)),
    }
}

// ---------------------------------------------------------------------------
// Vector helpers
// ---------------------------------------------------------------------------

fn parse_metric(metric: Option<String>) -> napi::Result<Option<VectorMetric>> {
    match metric.as_deref() {
        None | Some("cosine") => Ok(Some(VectorMetric::Cosine)),
        Some("dot") => Ok(Some(VectorMetric::Dot)),
        Some("euclidean") => Ok(Some(VectorMetric::Euclidean)),
        Some(other) => Err(napi::Error::from_reason(format!(
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
// napi bindings
// ---------------------------------------------------------------------------

#[napi]
pub struct TalaDBNode {
    /// `None` after `close()` — methods then return an error instead of
    /// touching freed state.
    inner: Option<Database>,
}

#[napi]
impl TalaDBNode {
    /// Open an in-memory database (useful for testing).
    #[napi(factory)]
    pub fn open_in_memory() -> napi::Result<Self> {
        let db = Database::open_in_memory().map_err(err_to_napi)?;
        Ok(Self { inner: Some(db) })
    }

    /// Open a file-backed database at the given path.
    ///
    /// `config_json` is an optional JSON-serialised `TalaDbConfig`; only its
    /// `durability` block is read here. Change webhooks are delivered by the
    /// `taladb` TypeScript client, not by this binding.
    #[napi(factory)]
    pub fn open(
        path: String,
        config_json: Option<String>,
        passphrase: Option<String>,
    ) -> napi::Result<Self> {
        let db = match passphrase {
            Some(passphrase) => Database::open_encrypted(std::path::Path::new(&path), &passphrase),
            None => Database::open(std::path::Path::new(&path)),
        }
        .map_err(err_to_napi)?;
        // Apply durability from config (default: flush every write / immediate).
        if let Some(json) = config_json.as_deref()
            && let Ok(cfg) = serde_json::from_str::<TalaDbConfig>(json)
        {
            db.set_durability(!cfg.durability.flush_every_write);
        }
        Ok(Self { inner: Some(db) })
    }

    /// Force any batched (eventual-durability) writes to disk. No-op under the
    /// default immediate durability. Backs `db.flush()`.
    #[napi]
    pub fn flush(&self) -> napi::Result<()> {
        self.db()?.flush().map_err(err_to_napi)
    }

    /// Compact the underlying storage file, reclaiming space freed by deletes
    /// and updates. Call during idle periods after large bulk deletes.
    /// Returns the number of bytes reclaimed (may be 0).
    #[napi]
    pub fn compact(&self) -> napi::Result<()> {
        self.db()?.compact().map_err(err_to_napi)
    }

    /// Close the database, releasing the file handle and its lock.
    ///
    /// Subsequent calls on this object return an error. Collections obtained
    /// before `close()` keep the underlying storage alive until they are
    /// garbage-collected — drop them too to fully release the file.
    #[napi]
    pub fn close(&mut self) -> napi::Result<()> {
        self.inner = None;
        Ok(())
    }

    fn db(&self) -> napi::Result<&Database> {
        self.inner
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("database is closed"))
    }

    /// List user collection names (excludes reserved `_`-prefixed collections).
    #[napi(js_name = "listCollectionNames")]
    pub fn list_collection_names(&self) -> napi::Result<Vec<String>> {
        self.db()?.list_collection_names().map_err(err_to_napi)
    }

    /// Read the current application migration version (0 if never set). Backs
    /// the `openDB({ migrations })` runner, which advances it per migration.
    #[napi(js_name = "userVersion")]
    pub fn user_version(&self) -> napi::Result<u32> {
        self.db()?.user_version().map_err(err_to_napi)
    }

    /// Persist the application migration version. Called after each migration's
    /// body succeeds so a crash mid-run resumes from the last applied version.
    #[napi(js_name = "setUserVersion")]
    pub fn set_user_version(&self, version: u32) -> napi::Result<()> {
        self.db()?.set_user_version(version).map_err(err_to_napi)
    }

    /// Get a collection by name.
    #[napi]
    pub fn collection(&self, name: String) -> napi::Result<CollectionNode> {
        let col = self.db()?.collection(&name).map_err(err_to_napi)?;
        Ok(CollectionNode {
            inner: Arc::new(col),
        })
    }
}

// ---------------------------------------------------------------------------
// CollectionNode
// ---------------------------------------------------------------------------

#[napi]
pub struct CollectionNode {
    inner: Arc<Collection>,
}

#[napi]
impl CollectionNode {
    /// Insert a document. Returns the ULID string id.
    #[napi]
    pub fn insert(&self, doc: JsonValue) -> napi::Result<String> {
        let fields = obj_to_fields(doc)?;
        let id = self.inner.insert(fields).map_err(err_to_napi)?;
        Ok(id.to_string())
    }

    /// Insert multiple documents.
    #[napi(js_name = "insertMany")]
    pub fn insert_many(&self, docs: Vec<JsonValue>) -> napi::Result<Vec<String>> {
        let items: napi::Result<Vec<Vec<(String, Value)>>> =
            docs.into_iter().map(obj_to_fields).collect();
        let ids = self.inner.insert_many(items?).map_err(err_to_napi)?;
        Ok(ids.iter().map(taladb_core::Ulid::to_string).collect())
    }

    /// Find documents matching the filter.
    #[napi]
    pub fn find(&self, filter: JsonValue) -> napi::Result<Vec<JsonValue>> {
        let f = json_to_filter(&filter)?;
        let docs = self.inner.find(f).map_err(err_to_napi)?;
        Ok(docs.iter().map(doc_to_json).collect())
    }

    /// Find a single document or return null.
    #[napi(js_name = "findOne")]
    pub fn find_one(&self, filter: JsonValue) -> napi::Result<Option<JsonValue>> {
        let f = json_to_filter(&filter)?;
        let doc = self.inner.find_one(f).map_err(err_to_napi)?;
        Ok(doc.map(|d| doc_to_json(&d)))
    }

    /// Run a MongoDB-style aggregation pipeline. `pipeline` is a JSON array of
    /// stage objects (`$match`, `$group`, `$sort`, `$skip`, `$limit`,
    /// `$project`). Returns the resulting documents.
    #[napi]
    pub fn aggregate(&self, pipeline: JsonValue) -> napi::Result<Vec<JsonValue>> {
        let pl = taladb_core::aggregate::parse_pipeline(&pipeline, &|v| {
            json_to_filter(v).map_err(|e| e.to_string())
        })
        .map_err(napi::Error::from_reason)?;
        let docs = self.inner.aggregate(pl).map_err(err_to_napi)?;
        Ok(docs.iter().map(doc_to_json).collect())
    }

    /// Update the first matching document.
    #[napi(js_name = "updateOne")]
    pub fn update_one(&self, filter: JsonValue, update: JsonValue) -> napi::Result<bool> {
        let f = json_to_filter(&filter)?;
        let u = json_to_update(update)?;
        self.inner.update_one(f, u).map_err(err_to_napi)
    }

    /// Update all matching documents.
    #[napi(js_name = "updateMany")]
    pub fn update_many(&self, filter: JsonValue, update: JsonValue) -> napi::Result<u32> {
        let f = json_to_filter(&filter)?;
        let u = json_to_update(update)?;
        let n = self.inner.update_many(f, u).map_err(err_to_napi)?;
        Ok(n as u32)
    }

    /// Delete the first matching document.
    #[napi(js_name = "deleteOne")]
    pub fn delete_one(&self, filter: JsonValue) -> napi::Result<bool> {
        let f = json_to_filter(&filter)?;
        self.inner.delete_one(f).map_err(err_to_napi)
    }

    /// Delete all matching documents.
    #[napi(js_name = "deleteMany")]
    pub fn delete_many(&self, filter: JsonValue) -> napi::Result<u32> {
        let f = json_to_filter(&filter)?;
        let n = self.inner.delete_many(f).map_err(err_to_napi)?;
        Ok(n as u32)
    }

    /// Count documents matching the filter.
    #[napi]
    pub fn count(&self, filter: JsonValue) -> napi::Result<u32> {
        let f = json_to_filter(&filter)?;
        let n = self.inner.count(f).map_err(err_to_napi)?;
        Ok(n as u32)
    }

    /// Create a secondary index on a field.
    #[napi(js_name = "createIndex")]
    pub fn create_index(&self, field: String) -> napi::Result<()> {
        self.inner.create_index(&field).map_err(err_to_napi)
    }

    /// Drop a secondary index.
    #[napi(js_name = "dropIndex")]
    pub fn drop_index(&self, field: String) -> napi::Result<()> {
        self.inner.drop_index(&field).map_err(err_to_napi)
    }

    /// Create a compound (multi-field) index over an ordered list of fields.
    #[napi(js_name = "createCompoundIndex")]
    pub fn create_compound_index(&self, fields: Vec<String>) -> napi::Result<()> {
        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        self.inner.create_compound_index(&refs).map_err(err_to_napi)
    }

    /// Drop a compound index by its ordered field list.
    #[napi(js_name = "dropCompoundIndex")]
    pub fn drop_compound_index(&self, fields: Vec<String>) -> napi::Result<()> {
        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        self.inner.drop_compound_index(&refs).map_err(err_to_napi)
    }

    /// Create a vector index on `field`.
    ///
    /// - `metric` — optional: `"cosine"` (default), `"dot"`, or `"euclidean"`.
    /// - `index_type` — optional: `"flat"` (default) or `"hnsw"`.
    /// - `hnsw_m` — HNSW connectivity (default 16).
    /// - `hnsw_ef_construction` — build quality (default 200).
    #[napi(js_name = "createVectorIndex")]
    pub fn create_vector_index(
        &self,
        field: String,
        dimensions: u32,
        metric: Option<String>,
        index_type: Option<String>,
        hnsw_m: Option<u32>,
        hnsw_ef_construction: Option<u32>,
    ) -> napi::Result<()> {
        let m = parse_metric(metric)?;
        let hnsw = parse_hnsw_opts(index_type, hnsw_m, hnsw_ef_construction);
        self.inner
            .create_vector_index(&field, dimensions as usize, m, hnsw)
            .map_err(err_to_napi)
    }

    /// Drop a vector index (and its HNSW graph if present).
    #[napi(js_name = "dropVectorIndex")]
    pub fn drop_vector_index(&self, field: String) -> napi::Result<()> {
        self.inner.drop_vector_index(&field).map_err(err_to_napi)
    }

    /// Rebuild the HNSW graph for a vector index from the current flat data.
    /// No-op when the feature is disabled or the index is flat-only.
    #[napi(js_name = "upgradeVectorIndex")]
    pub fn upgrade_vector_index(&self, field: String) -> napi::Result<()> {
        self.inner.upgrade_vector_index(&field).map_err(err_to_napi)
    }

    /// Return the indexes on this collection as a JSON string
    /// `{ btree: string[], fts: string[], vector: string[] }`.
    #[napi(js_name = "listIndexes")]
    pub fn list_indexes(&self) -> napi::Result<String> {
        let info = self.inner.list_indexes().map_err(err_to_napi)?;
        serde_json::to_string(&serde_json::json!({
            "btree": info.btree,
            "fts": info.fts,
            "vector": info.vector,
        }))
        .map_err(|e| napi::Error::from_reason(e.to_string()))
    }

    /// Create a full-text search index on a string field.
    #[napi(js_name = "createFtsIndex")]
    pub fn create_fts_index(&self, field: String) -> napi::Result<()> {
        self.inner.create_fts_index(&field).map_err(err_to_napi)
    }

    /// Drop a full-text search index and everything it stores.
    #[napi(js_name = "dropFtsIndex")]
    pub fn drop_fts_index(&self, field: String) -> napi::Result<()> {
        self.inner.drop_fts_index(&field).map_err(err_to_napi)
    }

    /// Rank documents against a free-text query using BM25.
    ///
    /// Unlike the `$contains` filter, which requires every token, this uses
    /// OR semantics — matching more query terms simply scores higher.
    ///
    /// Returns an array of `{ document: {...}, score: number }` objects.
    #[napi(js_name = "searchText")]
    pub fn search_text(
        &self,
        field: String,
        query: String,
        top_k: u32,
        filter: Option<JsonValue>,
        options: Option<JsonValue>,
    ) -> napi::Result<Vec<JsonValue>> {
        let pre_filter = match filter {
            Some(ref v) if !v.is_null() => Some(json_to_filter(v)?),
            _ => None,
        };
        let bm25 = parse_bm25_options(options.as_ref());

        let results = self
            .inner
            .search_text_with(&field, &query, top_k as usize, &bm25, pre_filter)
            .map_err(err_to_napi)?;

        Ok(results
            .iter()
            .map(|r| serde_json::json!({ "document": doc_to_json(&r.document), "score": r.score }))
            .collect())
    }

    /// Hybrid retrieval — BM25 and vector similarity fused with reciprocal
    /// rank fusion.
    ///
    /// `options` accepts `{ rrfK, textWeight, vectorWeight, candidates, k1, b }`.
    /// Returns `{ document, score, textRank, vectorRank }`, where the ranks are
    /// zero-based positions in each retriever's list or `null` if that
    /// retriever did not return the document.
    #[napi(js_name = "hybridSearch")]
    #[allow(clippy::too_many_arguments)]
    pub fn hybrid_search(
        &self,
        text_field: String,
        text: String,
        vector_field: String,
        vector: Vec<f64>,
        top_k: u32,
        filter: Option<JsonValue>,
        options: Option<JsonValue>,
    ) -> napi::Result<Vec<JsonValue>> {
        let vector_f32: Vec<f32> = vector.iter().map(|&f| f as f32).collect();
        let pre_filter = match filter {
            Some(ref v) if !v.is_null() => Some(json_to_filter(v)?),
            _ => None,
        };

        let mut query = taladb_core::fts::HybridQuery::new(
            &text_field,
            &text,
            &vector_field,
            &vector_f32,
            top_k as usize,
        );
        query.filter = pre_filter;
        query.bm25 = parse_bm25_options(options.as_ref());
        query.rrf = parse_rrf_options(options.as_ref());
        query.candidates = options
            .as_ref()
            .and_then(|o| o.get("candidates"))
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as usize);

        let results = self.inner.hybrid_search(query).map_err(err_to_napi)?;

        Ok(results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "document": doc_to_json(&r.document),
                    "score": r.score,
                    "textRank": r.text_rank,
                    "vectorRank": r.vector_rank,
                })
            })
            .collect())
    }

    /// Find the `top_k` nearest documents to `query`.
    ///
    /// `filter` — optional pre-filter in the same JSON object format as `find`.
    ///
    /// Returns an array of `{ document: {...}, score: number }` objects.
    #[napi(js_name = "findNearest")]
    pub fn find_nearest(
        &self,
        field: String,
        query: Vec<f64>,
        top_k: u32,
        filter: Option<JsonValue>,
    ) -> napi::Result<Vec<JsonValue>> {
        let query_f32: Vec<f32> = query.iter().map(|&f| f as f32).collect();

        let pre_filter = match filter {
            Some(ref v) if !v.is_null() => Some(json_to_filter(v)?),
            _ => None,
        };

        let results = self
            .inner
            .find_nearest(&field, &query_f32, top_k as usize, pre_filter)
            .map_err(err_to_napi)?;

        Ok(format_nearest(&results))
    }

    /// Zero-copy Float32Array fast path. Avoids the f64→f32 conversion loop
    /// used by `findNearest(number[])`, which matters for large embeddings
    /// (768/1024/1536 dims) called on the hot path.
    #[napi(js_name = "findNearestF32")]
    pub fn find_nearest_f32(
        &self,
        field: String,
        query: Float32Array,
        top_k: u32,
        filter: Option<JsonValue>,
    ) -> napi::Result<Vec<JsonValue>> {
        let pre_filter = match filter {
            Some(ref v) if !v.is_null() => Some(json_to_filter(v)?),
            _ => None,
        };

        let results = self
            .inner
            .find_nearest(&field, query.as_ref(), top_k as usize, pre_filter)
            .map_err(err_to_napi)?;

        Ok(format_nearest(&results))
    }

    /// Async variant of `findNearest` — runs on the libuv thread pool so large
    /// scans or unfiltered HNSW queries don't block the JS thread. Accepts a
    /// `Float32Array` for zero-copy embedding transfer.
    #[napi(js_name = "findNearestAsync", ts_return_type = "Promise<Array<any>>")]
    pub fn find_nearest_async(
        &self,
        field: String,
        query: Float32Array,
        top_k: u32,
        filter: Option<JsonValue>,
    ) -> napi::Result<AsyncTask<FindNearestTask>> {
        let pre_filter = match filter {
            Some(ref v) if !v.is_null() => Some(json_to_filter(v)?),
            _ => None,
        };
        Ok(AsyncTask::new(FindNearestTask {
            collection: Arc::clone(&self.inner),
            field,
            query: query.as_ref().to_vec(),
            top_k: top_k as usize,
            filter: pre_filter,
        }))
    }

    /// Async variant of `find` — runs on the libuv thread pool so large
    /// collection scans don't block the JS thread.
    #[napi(js_name = "findAsync", ts_return_type = "Promise<Array<any>>")]
    pub fn find_async(&self, filter: JsonValue) -> napi::Result<AsyncTask<FindTask>> {
        let f = json_to_filter(&filter)?;
        Ok(AsyncTask::new(FindTask {
            collection: Arc::clone(&self.inner),
            filter: f,
        }))
    }

    /// Async variant of `insert` — the write runs on the libuv thread pool
    /// instead of blocking the JS thread.
    #[napi(js_name = "insertAsync", ts_return_type = "Promise<string>")]
    pub fn insert_async(&self, doc: JsonValue) -> napi::Result<AsyncTask<InsertTask>> {
        let fields = obj_to_fields(doc)?;
        Ok(AsyncTask::new(InsertTask {
            collection: Arc::clone(&self.inner),
            fields: Some(fields),
        }))
    }

    /// Async variant of `insertMany`.
    #[napi(js_name = "insertManyAsync", ts_return_type = "Promise<Array<string>>")]
    pub fn insert_many_async(
        &self,
        docs: Vec<JsonValue>,
    ) -> napi::Result<AsyncTask<InsertManyTask>> {
        let items: napi::Result<Vec<Vec<(String, Value)>>> =
            docs.into_iter().map(obj_to_fields).collect();
        Ok(AsyncTask::new(InsertManyTask {
            collection: Arc::clone(&self.inner),
            items: Some(items?),
        }))
    }

    /// Async variant of `updateOne`.
    #[napi(js_name = "updateOneAsync", ts_return_type = "Promise<boolean>")]
    pub fn update_one_async(
        &self,
        filter: JsonValue,
        update: JsonValue,
    ) -> napi::Result<AsyncTask<UpdateOneTask>> {
        Ok(AsyncTask::new(UpdateOneTask {
            collection: Arc::clone(&self.inner),
            filter: json_to_filter(&filter)?,
            update: json_to_update(update)?,
        }))
    }

    /// Async variant of `updateMany`.
    #[napi(js_name = "updateManyAsync", ts_return_type = "Promise<number>")]
    pub fn update_many_async(
        &self,
        filter: JsonValue,
        update: JsonValue,
    ) -> napi::Result<AsyncTask<UpdateManyTask>> {
        Ok(AsyncTask::new(UpdateManyTask {
            collection: Arc::clone(&self.inner),
            filter: json_to_filter(&filter)?,
            update: json_to_update(update)?,
        }))
    }

    /// Async variant of `deleteOne`.
    #[napi(js_name = "deleteOneAsync", ts_return_type = "Promise<boolean>")]
    pub fn delete_one_async(&self, filter: JsonValue) -> napi::Result<AsyncTask<DeleteOneTask>> {
        Ok(AsyncTask::new(DeleteOneTask {
            collection: Arc::clone(&self.inner),
            filter: json_to_filter(&filter)?,
        }))
    }

    /// Async variant of `deleteMany`.
    #[napi(js_name = "deleteManyAsync", ts_return_type = "Promise<number>")]
    pub fn delete_many_async(&self, filter: JsonValue) -> napi::Result<AsyncTask<DeleteManyTask>> {
        Ok(AsyncTask::new(DeleteManyTask {
            collection: Arc::clone(&self.inner),
            filter: json_to_filter(&filter)?,
        }))
    }
}

/// Read BM25 tuning out of the shared options bag, falling back to defaults
/// for anything absent or malformed.
fn parse_bm25_options(options: Option<&JsonValue>) -> taladb_core::bm25::Bm25Params {
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

/// Read fusion tuning out of the shared options bag.
fn parse_rrf_options(options: Option<&JsonValue>) -> taladb_core::bm25::RrfParams {
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

fn format_nearest(results: &[VectorSearchResult]) -> Vec<JsonValue> {
    results
        .iter()
        .map(|r| serde_json::json!({ "document": doc_to_json(&r.document), "score": r.score }))
        .collect()
}

// ---------------------------------------------------------------------------
// AsyncTask impls — background work on the libuv thread pool
// ---------------------------------------------------------------------------

pub struct FindNearestTask {
    collection: Arc<Collection>,
    field: String,
    query: Vec<f32>,
    top_k: usize,
    filter: Option<Filter>,
}

impl Task for FindNearestTask {
    type Output = Vec<VectorSearchResult>;
    type JsValue = Vec<JsonValue>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.collection
            .find_nearest(&self.field, &self.query, self.top_k, self.filter.clone())
            .map_err(err_to_napi)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(format_nearest(&output))
    }
}

pub struct FindTask {
    collection: Arc<Collection>,
    filter: Filter,
}

impl Task for FindTask {
    type Output = Vec<taladb_core::Document>;
    type JsValue = Vec<JsonValue>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.collection
            .find(self.filter.clone())
            .map_err(err_to_napi)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output.iter().map(doc_to_json).collect())
    }
}

pub struct InsertTask {
    collection: Arc<Collection>,
    /// Taken in `compute` — napi may call compute only once per task.
    fields: Option<Vec<(String, Value)>>,
}

impl Task for InsertTask {
    type Output = String;
    type JsValue = String;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let fields = self
            .fields
            .take()
            .ok_or_else(|| napi::Error::from_reason("insert task already consumed"))?;
        let id = self.collection.insert(fields).map_err(err_to_napi)?;
        Ok(id.to_string())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct InsertManyTask {
    collection: Arc<Collection>,
    items: Option<Vec<Vec<(String, Value)>>>,
}

impl Task for InsertManyTask {
    type Output = Vec<String>;
    type JsValue = Vec<String>;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        let items = self
            .items
            .take()
            .ok_or_else(|| napi::Error::from_reason("insertMany task already consumed"))?;
        let ids = self.collection.insert_many(items).map_err(err_to_napi)?;
        Ok(ids.iter().map(taladb_core::Ulid::to_string).collect())
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct UpdateOneTask {
    collection: Arc<Collection>,
    filter: Filter,
    update: Update,
}

impl Task for UpdateOneTask {
    type Output = bool;
    type JsValue = bool;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.collection
            .update_one(self.filter.clone(), self.update.clone())
            .map_err(err_to_napi)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct UpdateManyTask {
    collection: Arc<Collection>,
    filter: Filter,
    update: Update,
}

impl Task for UpdateManyTask {
    type Output = u64;
    type JsValue = u32;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.collection
            .update_many(self.filter.clone(), self.update.clone())
            .map_err(err_to_napi)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output as u32)
    }
}

pub struct DeleteOneTask {
    collection: Arc<Collection>,
    filter: Filter,
}

impl Task for DeleteOneTask {
    type Output = bool;
    type JsValue = bool;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.collection
            .delete_one(self.filter.clone())
            .map_err(err_to_napi)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct DeleteManyTask {
    collection: Arc<Collection>,
    filter: Filter,
}

impl Task for DeleteManyTask {
    type Output = u64;
    type JsValue = u32;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        self.collection
            .delete_many(self.filter.clone())
            .map_err(err_to_napi)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> napi::Result<Self::JsValue> {
        Ok(output as u32)
    }
}
