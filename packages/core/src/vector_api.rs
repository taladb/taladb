//! Vector lifecycle and query API; all operations share the collection backend.
use super::*;
use crate::engine::{KvOp, ScanFlow, WriteTxn};
use crate::vector_graph::{self as graph, GraphOptions, Header, invalid};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

#[derive(Deserialize)]
#[serde(
    tag = "op",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Command {
    Create {
        field: String,
        dimensions: usize,
        #[serde(default)]
        metric: Option<String>,
        options: Option<GraphOptions>,
        /// Validate graph options now, but let the caller build in batches.
        #[serde(default)]
        defer_build: bool,
    },
    Status {
        field: String,
    },
    BeginBuild {
        field: String,
        options: Option<GraphOptions>,
    },
    StepBuild {
        field: String,
        id: String,
        batch_size: usize,
    },
    CancelBuild {
        field: String,
        id: String,
    },
    Search {
        field: String,
        query: Vec<f32>,
        top_k: usize,
        filter: Option<Json>,
        #[serde(default)]
        options: VectorQueryOptions,
    },
    Recall {
        field: String,
        queries: Vec<Vec<f32>>,
        top_k: usize,
        filter: Option<Json>,
        #[serde(default)]
        options: VectorQueryOptions,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VectorSearchMode {
    #[default]
    Auto,
    Exact,
    Ann,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct VectorQueryOptions {
    pub mode: VectorSearchMode,
    pub ef_search: Option<usize>,
    pub score_threshold: Option<f32>,
    pub offset: usize,
    pub group_by: Option<String>,
    /// Hits retained per group; default 1.
    pub group_size: Option<usize>,
    /// ANN candidate multiplier before exact rescoring; default 4.
    pub oversampling: Option<usize>,
}

pub struct VectorQueryResult {
    pub hits: Vec<VectorSearchResult>,
    pub execution: VectorExecution,
    pub next_offset: Option<usize>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorExecution {
    pub path: String,
    pub reason: String,
    pub revision: u64,
    pub ef_search: Option<usize>,
    pub distance_computations: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorBuildProgress {
    pub id: String,
    pub state: String,
    pub processed: u64,
    pub total: u64,
    pub revision: u64,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Build {
    progress: VectorBuildProgress,
    header: Header,
    last: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorIndexStatus {
    pub field: String,
    pub state: String,
    pub persistent: bool,
    pub indexed_vectors: u64,
    pub total_vectors: u64,
    pub deleted_nodes: u64,
    pub revision: u64,
    pub index_revision: Option<u64>,
    pub options: Option<GraphOptions>,
    pub build: Option<VectorBuildProgress>,
}

impl Collection {
    /// Shared binding protocol. Platform adapters supply their existing filter
    /// and document converters so JSON semantics stay identical to other queries.
    pub fn vector_command(
        &self,
        request: Json,
        parse_filter: &dyn Fn(&Json) -> Result<Filter, TalaDbError>,
        document_json: &dyn Fn(&Document) -> Json,
    ) -> Result<Json, TalaDbError> {
        crate::json_depth::check_json_depth(&request).map_err(|e| invalid(&e.to_string()))?;
        let command: Command =
            serde_json::from_value(request).map_err(|e| invalid(&e.to_string()))?;
        let filter = |f: Option<Json>| {
            f.filter(|v| !v.is_null())
                .map(|f| parse_filter(&f))
                .transpose()
        };
        Ok(match command {
            Command::Create {
                field,
                dimensions,
                metric,
                options,
                defer_build,
            } => {
                let metric = match metric.as_deref() {
                    None | Some("cosine") => VectorMetric::Cosine,
                    Some("dot") => VectorMetric::Dot,
                    Some("euclidean") => VectorMetric::Euclidean,
                    _ => return Err(invalid("invalid vector metric")),
                };
                if defer_build {
                    if let Some(options) = &options {
                        options.validate(metric)?;
                    }
                    self.create_vector_index_with_options(&field, dimensions, Some(metric), None)?;
                } else {
                    self.create_vector_index_with_options(
                        &field,
                        dimensions,
                        Some(metric),
                        options,
                    )?;
                }
                Json::Null
            }
            Command::Status { field } => json!(self.vector_index_status(&field)?),
            Command::BeginBuild { field, options } => {
                json!(self.begin_vector_build(&field, options)?)
            }
            Command::StepBuild {
                field,
                id,
                batch_size,
            } => json!(self.step_vector_build(&field, &id, batch_size)?),
            Command::CancelBuild { field, id } => json!(self.cancel_vector_build(&field, &id)?),
            Command::Search {
                field,
                query,
                top_k,
                filter: f,
                options,
            } => {
                let result = self.search_vectors(&field, &query, top_k, filter(f)?, &options)?;
                let hits: Vec<_> = result
                    .hits
                    .iter()
                    .map(|r| json!({"document": document_json(&r.document), "score": r.score}))
                    .collect();
                json!({ "hits": hits, "execution": result.execution, "nextOffset": result.next_offset })
            }
            Command::Recall {
                field,
                queries,
                top_k,
                filter: f,
                mut options,
            } => {
                if queries.is_empty()
                    || queries.len() > 1000
                    || top_k == 0
                    || options.offset != 0
                    || options.group_by.is_some()
                    || options.score_threshold.is_some()
                {
                    return Err(invalid(
                        "recall requires 1..1000 queries, positive topK, and no grouping, offset or threshold",
                    ));
                }
                let filter = filter(f)?;
                let txn = self.backend.begin_read()?;
                let mut recall = 0.0;
                let mut exact_ms = 0.0;
                let mut ann_ms = 0.0;
                for query in &queries {
                    let start = web_time::Instant::now();
                    let exact = self.search_vectors_in(
                        txn.as_ref(),
                        &field,
                        query,
                        top_k,
                        filter.clone(),
                        &VectorQueryOptions {
                            mode: VectorSearchMode::Exact,
                            ..Default::default()
                        },
                    )?;
                    exact_ms += start.elapsed().as_secs_f64() * 1000.0;
                    options.mode = VectorSearchMode::Ann;
                    let start = web_time::Instant::now();
                    let ann = self.search_vectors_in(
                        txn.as_ref(),
                        &field,
                        query,
                        top_k,
                        filter.clone(),
                        &options,
                    )?;
                    ann_ms += start.elapsed().as_secs_f64() * 1000.0;
                    let truth: HashSet<_> = exact.hits.iter().map(|h| h.document.id).collect();
                    recall += if truth.is_empty() {
                        1.0
                    } else {
                        ann.hits
                            .iter()
                            .filter(|h| truth.contains(&h.document.id))
                            .count() as f64
                            / truth.len() as f64
                    };
                }
                json!({"recallAtK": recall / queries.len() as f64, "queries": queries.len(), "topK": top_k, "exactMs": exact_ms, "annMs": ann_ms})
            }
        })
    }
    fn vector_def_in(&self, txn: &dyn ReadTxn, field: &str) -> Result<VectorDef, TalaDbError> {
        let bytes = txn
            .get(
                META_VECTOR_TABLE,
                vec_meta_key(&self.name, field).as_bytes(),
            )?
            .ok_or_else(|| TalaDbError::VectorIndexNotFound(vec_meta_key(&self.name, field)))?;
        Ok(postcard::from_bytes(&bytes)?)
    }
    pub(super) fn drop_graph_in(
        &self,
        txn: &mut dyn WriteTxn,
        field: &str,
    ) -> Result<(), TalaDbError> {
        let key = vec_meta_key(&self.name, field);
        if let Some(h) = graph::header(&WriteView(txn), &key)? {
            txn.delete_table(&h.table)?;
        }
        if let Some(bytes) = txn.get(graph::BUILDS, key.as_bytes())? {
            let build: Build = postcard::from_bytes(&bytes)?;
            if build.progress.state == "building" {
                txn.delete_table(&build.header.table)?;
            }
        }
        txn.delete(graph::META, key.as_bytes())?;
        txn.delete(graph::BUILDS, key.as_bytes())?;
        Ok(())
    }
    pub(super) fn build_graph_in(
        &self,
        txn: &mut dyn WriteTxn,
        def: &VectorDef,
        options: GraphOptions,
    ) -> Result<(), TalaDbError> {
        options.validate(def.metric)?;
        let key = vec_meta_key(&self.name, &def.field);
        let table = vec_table_name(&self.name, &def.field);
        let rev = revision(&WriteView(txn), &table)?;
        self.drop_graph_in(txn, &def.field)?;
        let mut h = Header::new(
            format!("hnsw::{key}::a"),
            rev,
            options,
            def.dimensions,
            def.metric,
        );
        for (id, bytes) in txn.range(
            &table,
            std::ops::Bound::Unbounded,
            std::ops::Bound::Unbounded,
        )? {
            let id = <[u8; 16]>::try_from(id.as_slice())
                .map_err(|_| invalid("invalid stored vector ID"))?;
            let v = decode_f32_vec(&bytes).ok_or_else(|| invalid("invalid stored vector bytes"))?;
            graph::insert(txn, &mut h, id, &v)?;
        }
        self.publish_graph_in(txn, &key, &h)
    }
    fn publish_graph_in(
        &self,
        txn: &mut dyn WriteTxn,
        key: &str,
        h: &Header,
    ) -> Result<(), TalaDbError> {
        graph::save_header(txn, key, h)?;
        txn.put(
            META_HNSW_TABLE,
            key.as_bytes(),
            &postcard::to_allocvec(&HnswOptions {
                m: h.options.m,
                ef_construction: h.options.ef_construction,
            })?,
        )?;
        Ok(())
    }
    pub(super) fn maintain_graph_in(
        &self,
        txn: &mut dyn WriteTxn,
        vtable: &str,
        generation: u64,
        ops: &[KvOp<'_>],
    ) -> Result<(), TalaDbError> {
        let key = vtable
            .strip_prefix("vec::")
            .ok_or_else(|| invalid("invalid vector table"))?;
        let Some(mut h) = graph::header(&WriteView(txn), key)? else {
            return Ok(());
        };
        // A graph from a legacy/corrupt revision is explicitly reported stale;
        // never stamp an incomplete graph with the current revision.
        if h.revision.checked_add(1) != Some(generation) {
            return Ok(());
        }
        for op in ops {
            match op {
                KvOp::Delete(id) => graph::remove(
                    txn,
                    &mut h,
                    &<[u8; 16]>::try_from(*id).map_err(|_| invalid("invalid vector ID"))?,
                )?,
                KvOp::Put(id, bytes) => {
                    let values =
                        decode_f32_vec(bytes).ok_or_else(|| invalid("invalid vector bytes"))?;
                    graph::insert(
                        txn,
                        &mut h,
                        <[u8; 16]>::try_from(*id).map_err(|_| invalid("invalid vector ID"))?,
                        &values,
                    )?;
                }
            }
        }
        h.revision = generation;
        graph::save_header(txn, key, &h)
    }
    /// Synchronous promotion/rebuild. For large indexes use the resumable batch API.
    pub fn rebuild_vector_index(
        &self,
        field: &str,
        options: Option<GraphOptions>,
    ) -> Result<(), TalaDbError> {
        let mut txn = self.backend.begin_write()?;
        let def = self.vector_def_in(&WriteView(txn.as_ref()), field)?;
        let key = vec_meta_key(&self.name, field);
        let options = match options {
            Some(o) => o,
            None => self.existing_graph_options(&WriteView(txn.as_ref()), &key)?,
        };
        self.build_graph_in(txn.as_mut(), &def, options)?;
        txn.commit()
    }
    fn existing_graph_options(
        &self,
        txn: &dyn ReadTxn,
        key: &str,
    ) -> Result<GraphOptions, TalaDbError> {
        if let Some(h) = graph::header(txn, key)? {
            return Ok(h.options);
        }
        if let Some(bytes) = txn.get(META_HNSW_TABLE, key.as_bytes())? {
            let old: HnswOptions = postcard::from_bytes(&bytes)?;
            return Ok(GraphOptions {
                m: old.m,
                ef_construction: old.ef_construction,
                ..Default::default()
            });
        }
        Ok(GraphOptions::default())
    }
    pub fn vector_index_status(&self, field: &str) -> Result<VectorIndexStatus, TalaDbError> {
        let txn = self.backend.begin_read()?;
        self.vector_def_in(txn.as_ref(), field)?;
        let key = vec_meta_key(&self.name, field);
        let table = vec_table_name(&self.name, field);
        let rev = revision(txn.as_ref(), &table)?;
        let h = graph::header(txn.as_ref(), &key)?;
        let legacy = txn.get(META_HNSW_TABLE, key.as_bytes())?.is_some();
        let build: Option<Build> = txn
            .get(graph::BUILDS, key.as_bytes())?
            .map(|b| postcard::from_bytes(&b))
            .transpose()?;
        Ok(VectorIndexStatus {
            field: field.into(),
            state: match &h {
                Some(h) if h.revision == rev => "ready",
                Some(_) => "stale",
                None if legacy => "rebuildRequired",
                None => "flat",
            }
            .into(),
            persistent: h.is_some(),
            indexed_vectors: h.as_ref().map_or(0, |h| h.live),
            total_vectors: txn.count_entries(&table)?,
            deleted_nodes: h.as_ref().map_or(0, |h| h.deleted),
            revision: rev,
            index_revision: h.as_ref().map(|h| h.revision),
            options: h.map(|h| h.options),
            build: build.map(|b| b.progress),
        })
    }
    /// Start a durable staged build. The current index continues serving queries.
    pub fn begin_vector_build(
        &self,
        field: &str,
        options: Option<GraphOptions>,
    ) -> Result<VectorBuildProgress, TalaDbError> {
        let mut txn = self.backend.begin_write()?;
        let view = WriteView(txn.as_ref());
        let def = self.vector_def_in(&view, field)?;
        let key = vec_meta_key(&self.name, field);
        if let Some(bytes) = view.get(graph::BUILDS, key.as_bytes())? {
            let old: Build = postcard::from_bytes(&bytes)?;
            if old.progress.state == "building" {
                return Err(invalid(
                    "a vector build is already running; resume or cancel it",
                ));
            }
        }
        let options = options
            .map(Ok)
            .unwrap_or_else(|| self.existing_graph_options(&view, &key))?;
        options.validate(def.metric)?;
        let vtable = vec_table_name(&self.name, field);
        let rev = revision(&view, &vtable)?;
        let id = Ulid::new().to_string();
        let active = graph::header(&view, &key)?;
        let slot = if active.is_some_and(|h| h.table.ends_with("::a")) {
            "b"
        } else {
            "a"
        };
        let build = Build {
            progress: VectorBuildProgress {
                id,
                state: "building".into(),
                processed: 0,
                total: view.count_entries(&vtable)?,
                revision: rev,
                error: None,
            },
            header: Header::new(
                format!("hnsw::{key}::{slot}"),
                rev,
                options,
                def.dimensions,
                def.metric,
            ),
            last: None,
        };
        txn.put(
            graph::BUILDS,
            key.as_bytes(),
            &postcard::to_allocvec(&build)?,
        )?;
        txn.commit()?;
        Ok(build.progress)
    }
    /// Do at most `batch_size` insertions. Each completed batch is resumable
    /// after a process restart. Concurrent embedding writes abort publication.
    pub fn step_vector_build(
        &self,
        field: &str,
        id: &str,
        batch_size: usize,
    ) -> Result<VectorBuildProgress, TalaDbError> {
        if !(1..=1024).contains(&batch_size) {
            return Err(invalid("build batchSize must be between 1 and 1024"));
        }
        let key = vec_meta_key(&self.name, field);
        let vtable = vec_table_name(&self.name, field);
        let read = self.backend.begin_read()?;
        let bytes = read
            .get(graph::BUILDS, key.as_bytes())?
            .ok_or_else(|| invalid("no vector build exists"))?;
        let mut build: Build = postcard::from_bytes(&bytes)?;
        if build.progress.id != id {
            return Err(invalid("vector build ID does not match"));
        }
        if build.progress.state != "building" {
            return Ok(build.progress);
        }
        let start = build
            .last
            .as_deref()
            .map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
        let mut batch = Vec::with_capacity(batch_size);
        read.scan(&vtable, start, std::ops::Bound::Unbounded, &mut |k, v| {
            batch.push((k.to_vec(), v.to_vec()));
            Ok(if batch.len() == batch_size {
                ScanFlow::Stop
            } else {
                ScanFlow::Continue
            })
        })?;
        drop(read);
        let mut txn = self.backend.begin_write()?;
        // Prevent two handles from advancing/cancelling the same batch concurrently.
        if txn.get(graph::BUILDS, key.as_bytes())?.as_ref() != Some(&bytes) {
            return Err(invalid(
                "vector build changed concurrently; read its status before resuming",
            ));
        }
        if revision(&WriteView(txn.as_ref()), &vtable)? != build.progress.revision {
            build.progress.state = "failed".into();
            build.progress.error =
                Some("vectors changed during the build; restart the build".into());
            txn.delete_table(&build.header.table)?;
        } else {
            for (key, bytes) in batch {
                let vector =
                    decode_f32_vec(&bytes).ok_or_else(|| invalid("invalid stored vector"))?;
                graph::insert(
                    txn.as_mut(),
                    &mut build.header,
                    <[u8; 16]>::try_from(key.as_slice())
                        .map_err(|_| invalid("invalid stored vector ID"))?,
                    &vector,
                )?;
                build.last = Some(key);
                build.progress.processed += 1;
            }
            if build.progress.processed == build.progress.total {
                if let Some(old) = graph::header(&WriteView(txn.as_ref()), &key)? {
                    txn.delete_table(&old.table)?;
                }
                self.publish_graph_in(txn.as_mut(), &key, &build.header)?;
                build.progress.state = "ready".into();
            }
        }
        txn.put(
            graph::BUILDS,
            key.as_bytes(),
            &postcard::to_allocvec(&build)?,
        )?;
        txn.commit()?;
        Ok(build.progress)
    }
    pub fn cancel_vector_build(
        &self,
        field: &str,
        id: &str,
    ) -> Result<VectorBuildProgress, TalaDbError> {
        let key = vec_meta_key(&self.name, field);
        let mut txn = self.backend.begin_write()?;
        let bytes = txn
            .get(graph::BUILDS, key.as_bytes())?
            .ok_or_else(|| invalid("no vector build exists"))?;
        let mut build: Build = postcard::from_bytes(&bytes)?;
        if build.progress.id != id {
            return Err(invalid("vector build ID does not match"));
        }
        if build.progress.state == "building" {
            txn.delete_table(&build.header.table)?;
            build.progress.state = "cancelled".into();
            txn.put(
                graph::BUILDS,
                key.as_bytes(),
                &postcard::to_allocvec(&build)?,
            )?;
        }
        txn.commit()?;
        Ok(build.progress)
    }
    pub fn search_vectors(
        &self,
        field: &str,
        query: &[f32],
        top_k: usize,
        filter: Option<Filter>,
        options: &VectorQueryOptions,
    ) -> Result<VectorQueryResult, TalaDbError> {
        let txn = self.backend.begin_read()?;
        self.search_vectors_in(txn.as_ref(), field, query, top_k, filter, options)
    }
    pub(super) fn search_vectors_in(
        &self,
        txn: &dyn ReadTxn,
        field: &str,
        query: &[f32],
        top_k: usize,
        filter: Option<Filter>,
        options: &VectorQueryOptions,
    ) -> Result<VectorQueryResult, TalaDbError> {
        let def = self.vector_def_in(txn, field)?;
        if query.len() != def.dimensions {
            return Err(TalaDbError::VectorDimensionMismatch {
                expected: def.dimensions,
                got: query.len(),
            });
        }
        if def.dimensions == 0 || !query.iter().all(|x| x.is_finite()) {
            return Err(invalid(
                "query vector must have finite components and positive dimensions",
            ));
        }
        if options.ef_search == Some(0)
            || options.group_size == Some(0)
            || options.oversampling.is_some_and(|n| n == 0 || n > 100)
            || options.score_threshold.is_some_and(|n| !n.is_finite())
            || options.group_by.as_ref().is_some_and(String::is_empty)
        {
            return Err(invalid("invalid vector search options"));
        }
        let key = vec_meta_key(&self.name, field);
        let table = vec_table_name(&self.name, field);
        let rev = revision(txn, &table)?;
        let h = graph::header(txn, &key)?;
        let ready = h.as_ref().is_some_and(|h| h.revision == rev);
        let ann = options.mode == VectorSearchMode::Ann
            || (options.mode == VectorSearchMode::Auto && filter.is_none() && ready);
        if ann && !ready {
            return Err(invalid(
                "ANN index is unavailable or stale; rebuild it or request exact search",
            ));
        }
        let reason = if ann {
            "indexReady"
        } else if options.mode == VectorSearchMode::Exact {
            "requestedExact"
        } else if filter.is_some() {
            "filteredExactDefault"
        } else if h.is_some() {
            "staleIndex"
        } else if txn.get(META_HNSW_TABLE, key.as_bytes())?.is_some() {
            "legacyIndexRequiresRebuild"
        } else {
            "flatIndex"
        };
        let mut execution = VectorExecution {
            path: if ann { "hnsw" } else { "exact" }.into(),
            reason: reason.into(),
            revision: rev,
            ef_search: None,
            distance_computations: 0,
        };
        if top_k == 0 {
            return Ok(VectorQueryResult {
                hits: vec![],
                execution,
                next_offset: None,
            });
        }
        let count = usize::try_from(txn.count_entries(&table)?).unwrap_or(usize::MAX);
        let wanted = top_k.saturating_add(options.offset).min(count);
        let allowed = if ann {
            filter
                .as_ref()
                .map(|f| self.matching_ids_in(txn, f))
                .transpose()?
        } else {
            None
        };
        let mut rows;
        if ann {
            let h = h.as_ref().unwrap();
            let mut ef = options
                .ef_search
                .unwrap_or(100)
                .max(wanted.saturating_mul(options.oversampling.unwrap_or(4)))
                .min(count)
                .max(1);
            loop {
                let (ids, distances) = graph::search(txn, h, query, ef, allowed.as_ref())?;
                execution.distance_computations += distances;
                execution.ef_search = Some(ef);
                // Exact rescoring always reads the original f32 vector from the
                // same snapshot as the graph, filter, and returned document.
                let mut ranked = Vec::with_capacity(ids.len());
                let query_norm = crate::vector::l2_norm(query);
                for id in ids {
                    if let Some(bytes) = txn.get(&table, &id)?
                        && let Some(v) = decode_f32_vec(&bytes)
                        && v.len() == query.len()
                    {
                        let score = crate::vector::score_with_query_norm(
                            &def.metric,
                            query,
                            query_norm,
                            &v,
                        );
                        if score.is_finite() {
                            ranked.push((Ulid::from_bytes(id), score));
                        }
                    }
                }
                ranked.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
                rows = self.load_results_in(txn, ranked)?;
                Self::reduce_vector_rows(&mut rows, options)?;
                if rows.len() >= wanted || ef >= count {
                    break;
                }
                ef = ef.saturating_mul(2).min(count);
            }
        } else {
            // Grouping must see all candidates before truncation: simply
            // grouping a top-k list loses less frequent parent entities.
            let pool = if options.group_by.is_some() {
                count
            } else {
                wanted.saturating_add(1).min(count)
            };
            (rows, execution.distance_computations) =
                self.find_nearest_in(txn, field, query, pool, filter)?;
            Self::reduce_vector_rows(&mut rows, options)?;
        }
        let next_offset = (rows.len() > wanted).then_some(options.offset.saturating_add(top_k));
        let hits = rows.into_iter().skip(options.offset).take(top_k).collect();
        Ok(VectorQueryResult {
            hits,
            execution,
            next_offset,
        })
    }
    fn reduce_vector_rows(
        rows: &mut Vec<VectorSearchResult>,
        options: &VectorQueryOptions,
    ) -> Result<(), TalaDbError> {
        if let Some(threshold) = options.score_threshold {
            rows.retain(|r| r.score >= threshold);
        }
        if let Some(field) = &options.group_by {
            let mut counts: HashMap<Vec<u8>, usize> = HashMap::new();
            let mut reduced = Vec::new();
            for row in rows.drain(..) {
                let key = postcard::to_allocvec(row.document.get(field).unwrap_or(&Value::Null))?;
                let count = counts.entry(key).or_default();
                if *count < options.group_size.unwrap_or(1) {
                    *count += 1;
                    reduced.push(row);
                }
            }
            *rows = reduced;
        }
        Ok(())
    }
}
