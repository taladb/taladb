//! ANN rescoring from a retained vector block.
//!
//! Unfiltered exact search retains the decoded vectors of a field, tagged with
//! the vector table's revision. When a block of the query snapshot's revision
//! is resident, ANN rescoring takes candidate vectors from it instead of one
//! storage read per candidate. Everything else — candidates, ranking, scores,
//! documents — must be what the point-read path returns, from one snapshot.
//!
//! A probe backend counts point reads of `vec::` tables per thread and can park
//! a reader on its first read of a chosen table, so a write can be committed at
//! a known point of a running query.

use std::cell::Cell;
use std::ops::Bound;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use taladb::engine::{KvPairs, ReadTxn, ScanFn, WriteTxn};
use taladb::vector::{
    VectorMetric, encode_f32_vec, l2_norm, score_with_query_norm, vec_table_name,
};
use taladb::{
    Collection, Database, Filter, GraphOptions, Quantization, RedbBackend, StorageBackend,
    TalaDbError, Update, Value, VectorQueryOptions, VectorQueryResult, VectorSearchMode,
};

thread_local! {
    static VECTOR_READS: Cell<u64> = const { Cell::new(0) };
}

/// Point reads of vector tables made by this thread so far.
fn vector_reads() -> u64 {
    VECTOR_READS.with(Cell::get)
}

struct Gate {
    armed: AtomicBool,
    prefix: Mutex<&'static str>,
    entered: mpsc::Sender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
}
struct ProbeBackend {
    inner: RedbBackend,
    gate: Arc<Gate>,
}
impl StorageBackend for ProbeBackend {
    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        Ok(Box::new(ProbeRead {
            inner: self.inner.begin_read()?,
            gate: &self.gate,
        }))
    }
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        self.inner.begin_write()
    }
}
struct ProbeRead<'a> {
    inner: Box<dyn ReadTxn + 'a>,
    gate: &'a Gate,
}
impl ProbeRead<'_> {
    fn observe(&self, table: &str, point_reads: u64) {
        if table.starts_with("vec::") {
            VECTOR_READS.with(|n| n.set(n.get() + point_reads));
        }
        if table.starts_with(*self.gate.prefix.lock().unwrap())
            && self.gate.armed.swap(false, Ordering::SeqCst)
        {
            self.gate.entered.send(()).unwrap();
            self.gate
                .resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(20))
                .unwrap();
        }
    }
}
impl ReadTxn for ProbeRead<'_> {
    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        self.observe(table, 1);
        self.inner.get(table, key)
    }
    fn get_many(&self, table: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        self.observe(table, keys.len() as u64);
        self.inner.get_many(table, keys)
    }
    fn range(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, TalaDbError> {
        self.observe(table, 0);
        self.inner.range(table, start, end)
    }
    fn scan(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
        f: ScanFn<'_>,
    ) -> Result<(), TalaDbError> {
        self.observe(table, 0);
        self.inner.scan(table, start, end, f)
    }
    fn scan_all(&self, table: &str) -> Result<KvPairs, TalaDbError> {
        self.observe(table, 0);
        self.inner.scan_all(table)
    }
    fn list_tables(&self) -> Result<Vec<String>, TalaDbError> {
        self.inner.list_tables()
    }
    fn count_entries(&self, table: &str) -> Result<u64, TalaDbError> {
        self.inner.count_entries(table)
    }
}

struct Harness {
    db: Database,
    gate: Arc<Gate>,
    entered: mpsc::Receiver<()>,
    resume: mpsc::Sender<()>,
}
impl Harness {
    fn new() -> Self {
        let (entered_tx, entered) = mpsc::channel();
        let (resume, resume_rx) = mpsc::channel();
        let gate = Arc::new(Gate {
            armed: AtomicBool::new(false),
            prefix: Mutex::new(""),
            entered: entered_tx,
            resume: Mutex::new(resume_rx),
        });
        let db = Database::open_with_backend(Box::new(ProbeBackend {
            inner: RedbBackend::open_in_memory().unwrap(),
            gate: gate.clone(),
        }))
        .unwrap();
        Self {
            db,
            gate,
            entered,
            resume,
        }
    }
    /// Park the next read of a table starting with `prefix` until `resume`.
    fn arm(&self, prefix: &'static str) {
        *self.gate.prefix.lock().unwrap() = prefix;
        self.gate.armed.store(true, Ordering::SeqCst);
    }
    fn wait_entered(&self) {
        self.entered.recv_timeout(Duration::from_secs(20)).unwrap();
    }
    /// Empty the search cache (blocks and graph nodes), keeping the budget.
    fn clear_cache(&self) {
        let budget = self.db.vector_cache_stats().budget_bytes;
        self.db.set_vector_cache_budget(0);
        self.db.set_vector_cache_budget(budget);
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
    fn vector(&mut self, dims: usize) -> Vec<f32> {
        (0..dims).map(|_| self.unit()).collect()
    }
}

fn value(v: &[f32]) -> Value {
    Value::Array(v.iter().map(|x| Value::Float(f64::from(*x))).collect())
}
fn body_vector(doc: &taladb::Document, field: &str) -> Vec<f32> {
    match doc.get(field) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|x| match x {
                Value::Float(f) => *f as f32,
                other => panic!("unexpected vector component {other:?}"),
            })
            .collect(),
        other => panic!("document has no vector: {other:?}"),
    }
}

/// Everything a caller can observe: hits (ID, score bits, body), next offset,
/// and the execution report.
type Fingerprint = (
    Vec<(String, u32, String)>,
    Option<usize>,
    String,
    String,
    u64,
    Option<usize>,
    usize,
);
fn fingerprint(r: &VectorQueryResult) -> Fingerprint {
    (
        r.hits
            .iter()
            .map(|h| {
                (
                    h.document.id.to_string(),
                    h.score.to_bits(),
                    format!("{:?}", h.document.fields),
                )
            })
            .collect(),
        r.next_offset,
        r.execution.path.clone(),
        r.execution.reason.clone(),
        r.execution.revision,
        r.execution.ef_search,
        r.execution.distance_computations,
    )
}

fn ann() -> VectorQueryOptions {
    VectorQueryOptions {
        mode: VectorSearchMode::Ann,
        ..Default::default()
    }
}
fn exact() -> VectorQueryOptions {
    VectorQueryOptions {
        mode: VectorSearchMode::Exact,
        ..Default::default()
    }
}

/// Retain the block of the current revision, as an unfiltered exact search does.
fn retain_block(h: &Harness, col: &Collection, field: &str, dims: usize) {
    col.search_vectors(field, &vec![0.5; dims], 1, None, &exact())
        .unwrap();
    assert_eq!(
        h.db.vector_cache_stats().vector_indexes,
        1,
        "block retained"
    );
}

/// The same ANN query without a block (point reads) and with one.
/// Returns both fingerprints and the vector point reads of each.
fn miss_and_hit(
    h: &Harness,
    col: &Collection,
    field: &str,
    query: &[f32],
    top_k: usize,
    filter: Option<Filter>,
    options: &VectorQueryOptions,
) -> (Fingerprint, Fingerprint, u64, u64) {
    h.clear_cache();
    let before = vector_reads();
    let miss = col
        .search_vectors(field, query, top_k, filter.clone(), options)
        .unwrap();
    let miss_reads = vector_reads() - before;
    retain_block(h, col, field, query.len());
    let before = vector_reads();
    let hit = col
        .search_vectors(field, query, top_k, filter, options)
        .unwrap();
    let hit_reads = vector_reads() - before;
    (fingerprint(&miss), fingerprint(&hit), miss_reads, hit_reads)
}

fn populate(col: &Collection, n: usize, dims: usize, rng: &mut Rng) -> Vec<ulid::Ulid> {
    col.insert_many(
        (0..n)
            .map(|i| {
                let v = match i {
                    0 => vec![0.0; dims],                       // zero vector
                    1 => vec![1e18; dims],                      // squares overflow f32
                    2 => vec![1e-40; dims],                     // subnormal components
                    3 => (0..dims).map(|j| j as f32).collect(), // unnormalised
                    _ => rng.vector(dims),
                };
                vec![
                    ("v".into(), value(&v)),
                    ("g".into(), Value::Int((i % 7) as i64)),
                    ("u".into(), Value::Int((i % 3) as i64)),
                    ("name".into(), Value::Str(format!("doc-{i}"))),
                ]
            })
            .collect(),
    )
    .unwrap()
}

fn graph(quantization: Quantization) -> Option<GraphOptions> {
    Some(GraphOptions {
        m: 8,
        ef_construction: 32,
        quantization,
    })
}

#[test]
fn retained_block_rescoring_is_bit_identical_to_point_reads() {
    // HNSW rejects dot product, so ANN rescoring never runs for it.
    let h = Harness::new();
    let col = h.db.collection("docs").unwrap();
    assert!(
        col.create_vector_index_with_options(
            "v",
            4,
            Some(VectorMetric::Dot),
            graph(Quantization::None)
        )
        .is_err()
    );
    let metrics = [VectorMetric::Cosine, VectorMetric::Euclidean];
    let quantizations = [
        Quantization::None,
        Quantization::Scalar,
        Quantization::Binary,
    ];
    for metric in &metrics {
        for quantization in quantizations {
            // Binary quantization is cosine-only.
            if quantization == Quantization::Binary && *metric != VectorMetric::Cosine {
                continue;
            }
            // Tails: 3, 13 and 130 are not multiples of the 8-lane kernels.
            for dims in [3usize, 8, 13, 130] {
                let h = Harness::new();
                let col = h.db.collection("docs").unwrap();
                col.create_index("g").unwrap();
                col.create_vector_index_with_options("v", dims, Some(*metric), graph(quantization))
                    .unwrap();
                let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ dims as u64);
                populate(&col, 200, dims, &mut rng);
                let cases: Vec<(usize, Option<Filter>, VectorQueryOptions)> = vec![
                    (10, None, ann()),
                    (5, None, VectorQueryOptions { offset: 5, ..ann() }),
                    (
                        10,
                        None,
                        VectorQueryOptions {
                            ef_search: Some(1),
                            oversampling: Some(1),
                            ..ann()
                        },
                    ),
                    (
                        10,
                        None,
                        VectorQueryOptions {
                            score_threshold: Some(0.25),
                            ..ann()
                        },
                    ),
                    (
                        6,
                        None,
                        VectorQueryOptions {
                            group_by: Some("g".into()),
                            group_size: Some(2),
                            ..ann()
                        },
                    ),
                    // Seven groups, tiny ef: grouping drops rows and the search retries.
                    (
                        7,
                        None,
                        VectorQueryOptions {
                            group_by: Some("g".into()),
                            ef_search: Some(1),
                            oversampling: Some(1),
                            ..ann()
                        },
                    ),
                    (5, Some(Filter::Eq("g".into(), Value::Int(3))), ann()),
                    (500, None, ann()),
                ];
                let mut queries = vec![rng.vector(dims), vec![0.0; dims]];
                queries.push((0..dims).map(|j| (j % 3) as f32 - 1.0).collect());
                for query in &queries {
                    for (top_k, filter, options) in &cases {
                        let (miss, hit, miss_reads, hit_reads) =
                            miss_and_hit(&h, &col, "v", query, *top_k, filter.clone(), options);
                        let label = format!(
                            "{metric:?} {quantization:?} dims={dims} top_k={top_k} filter={filter:?}"
                        );
                        assert_eq!(miss, hit, "{label}");
                        assert_eq!(miss.2, "hnsw", "{label}");
                        assert!(miss_reads > 0, "{label}: point-read control read nothing");
                        assert_eq!(
                            hit_reads, 0,
                            "{label}: rescoring read storage despite a block"
                        );
                    }
                }
                assert_eq!(h.db.vector_cache_stats().active_bytes, 0);
            }
        }
    }
}

/// Filters too large for an ID set walk the graph with a bitmap charged to the
/// graph loan; the block must not change what such a walk returns.
#[test]
fn large_filters_through_the_graph_bitmap_rescore_identically() {
    let h = Harness::new();
    // 256 KiB caps ANN filter ID sets at 256, so 2/3 of 400 needs the bitmap.
    h.db.set_vector_cache_budget(256 * 1024);
    let col = h.db.collection("docs").unwrap();
    col.create_vector_index_with_options(
        "v",
        8,
        Some(VectorMetric::Cosine),
        graph(Quantization::None),
    )
    .unwrap();
    let mut rng = Rng(7);
    populate(&col, 400, 8, &mut rng);
    let filter = Filter::Ne("u".into(), Value::Int(0));
    for top_k in [5, 40] {
        let query = rng.vector(8);
        let (miss, hit, miss_reads, hit_reads) =
            miss_and_hit(&h, &col, "v", &query, top_k, Some(filter.clone()), &ann());
        assert_eq!(miss, hit);
        assert_eq!(miss.2, "hnsw");
        assert!(miss_reads > 0);
        assert_eq!(hit_reads, 0);
    }
    let stats = h.db.vector_cache_stats();
    assert_eq!(stats.active_bytes, 0);
    assert!(stats.peak_bytes <= stats.memory_budget_bytes);
}

/// A candidate missing from a block of the right revision is read from
/// storage, never dropped. Normal writes cannot produce this — the block holds
/// every vector of the index's dimensions — so it is staged behind the write
/// path, which does not bump the revision: a truncated record keeps the vector
/// out of the block, then a valid record is restored.
#[test]
fn a_candidate_absent_from_the_block_falls_back_to_its_point_read() {
    let h = Harness::new();
    let col = h.db.collection("docs").unwrap();
    col.create_vector_index_with_options(
        "v",
        4,
        Some(VectorMetric::Cosine),
        graph(Quantization::None),
    )
    .unwrap();
    let mut rng = Rng(11);
    let ids = populate(&col, 40, 4, &mut rng);
    let target = ids[20];
    let table = vec_table_name("docs", "v");
    let raw_put = |v: &[f32]| {
        let mut txn = h.db.backend().begin_write().unwrap();
        txn.put(&table, &target.to_bytes(), &encode_f32_vec(v))
            .unwrap();
        txn.commit().unwrap();
    };
    let query = [0.25, -0.5, 1.0, 0.125];
    h.clear_cache();
    raw_put(&[1.0, 0.0]);
    retain_block(&h, &col, "v", 4);
    raw_put(&query);

    let before = vector_reads();
    let hit = col.search_vectors("v", &query, 40, None, &ann()).unwrap();
    assert_eq!(
        vector_reads() - before,
        1,
        "only the absent candidate is read"
    );
    assert_eq!(
        hit.hits[0].document.id, target,
        "the restored vector ranks first"
    );
    h.clear_cache();
    let control = col.search_vectors("v", &query, 40, None, &ann()).unwrap();
    assert_eq!(fingerprint(&hit), fingerprint(&control));

    // A record that stays invalid is skipped by both paths, as before.
    raw_put(&[1.0, 0.0]);
    h.clear_cache();
    let control = col.search_vectors("v", &query, 40, None, &ann()).unwrap();
    retain_block(&h, &col, "v", 4);
    let hit = col.search_vectors("v", &query, 40, None, &ann()).unwrap();
    assert!(hit.hits.iter().all(|r| r.document.id != target));
    assert_eq!(fingerprint(&hit), fingerprint(&control));
}

/// Only a block of the query's own revision, field and collection is used.
#[test]
fn a_block_of_another_revision_field_or_collection_is_never_used() {
    let h = Harness::new();
    let col = h.db.collection("docs").unwrap();
    let other = h.db.collection("other").unwrap();
    for c in [&col, &other] {
        for field in ["a", "b"] {
            c.create_vector_index_with_options(
                field,
                4,
                Some(VectorMetric::Euclidean),
                graph(Quantization::None),
            )
            .unwrap();
        }
    }
    let mut rng = Rng(23);
    // One insert per collection: all four vector tables share revision numbers.
    let mut ids = Vec::new();
    for c in [&col, &other] {
        ids = c
            .insert_many(
                (0..60)
                    .map(|_| {
                        vec![
                            ("a".into(), value(&rng.vector(4))),
                            ("b".into(), value(&rng.vector(4))),
                        ]
                    })
                    .collect(),
            )
            .unwrap();
    }
    let query = [1.0, 0.5, -0.25, 0.75];
    let ann_of =
        |c: &Collection, field: &str| c.search_vectors(field, &query, 10, None, &ann()).unwrap();

    // Field `a` of `docs` retained: `b` of `docs` and `a` of `other` read storage.
    h.clear_cache();
    let controls = [ann_of(&col, "b"), ann_of(&other, "a")].map(|r| fingerprint(&r));
    h.clear_cache();
    retain_block(&h, &col, "a", 4);
    for (i, (c, field)) in [(&col, "b"), (&other, "a")].into_iter().enumerate() {
        let before = vector_reads();
        let result = ann_of(c, field);
        assert!(vector_reads() > before, "{field}: used a foreign block");
        assert_eq!(fingerprint(&result), controls[i]);
    }

    // A write moves `other.a` to a new revision; its old block must not serve it.
    h.clear_cache();
    retain_block(&h, &other, "a", 4);
    let target = ids[0];
    other
        .update_one(
            Filter::Eq("_id".into(), Value::Str(target.to_string())),
            Update::Set(vec![("a".into(), value(&query))]),
        )
        .unwrap();
    let before = vector_reads();
    let stale = ann_of(&other, "a");
    assert!(vector_reads() > before, "rescored from a stale block");
    assert_eq!(stale.hits[0].document.id, target);
    assert_eq!(
        stale.hits[0].score.to_bits(),
        score_with_query_norm(&VectorMetric::Euclidean, &query, l2_norm(&query), &query).to_bits()
    );
    h.clear_cache();
    assert_eq!(fingerprint(&stale), fingerprint(&ann_of(&other, "a")));
}

/// Deletes, inserts and graph rebuilds: each state rescored with and without
/// its own block gives the same result.
#[test]
fn writes_and_rebuilds_invalidate_and_reuse_blocks_correctly() {
    let h = Harness::new();
    let col = h.db.collection("docs").unwrap();
    col.create_vector_index_with_options(
        "v",
        13,
        Some(VectorMetric::Cosine),
        graph(Quantization::Scalar),
    )
    .unwrap();
    let mut rng = Rng(31);
    let ids = populate(&col, 120, 13, &mut rng);
    let query = rng.vector(13);
    let check = |label: &str| {
        let (miss, hit, _, hit_reads) = miss_and_hit(&h, &col, "v", &query, 10, None, &ann());
        assert_eq!(miss, hit, "{label}");
        assert_eq!(hit_reads, 0, "{label}");
        miss
    };
    let initial = check("initial");
    // Delete the current best hit: a block retained before the delete is stale.
    retain_block(&h, &col, "v", 13);
    let best = initial.0[0].0.parse::<ulid::Ulid>().unwrap();
    assert!(col.delete_by_id(best).unwrap());
    let after_delete = col.search_vectors("v", &query, 10, None, &ann()).unwrap();
    assert!(after_delete.hits.iter().all(|r| r.document.id != best));
    assert_eq!(fingerprint(&after_delete), check("after delete"));
    // Insert a perfect match.
    retain_block(&h, &col, "v", 13);
    let id = col.insert(vec![("v".into(), value(&query))]).unwrap();
    let after_insert = col.search_vectors("v", &query, 10, None, &ann()).unwrap();
    assert_eq!(after_insert.hits[0].document.id, id);
    assert_eq!(fingerprint(&after_insert), check("after insert"));
    // A rebuild replaces the graph and evicts the field; the revision stays.
    retain_block(&h, &col, "v", 13);
    let revision = after_insert.execution.revision;
    col.rebuild_vector_index("v", graph(Quantization::Binary))
        .unwrap();
    assert_eq!(
        h.db.vector_cache_stats().vector_indexes,
        0,
        "rebuild evicts the block"
    );
    let rebuilt = check("after rebuild");
    assert_eq!(rebuilt.4, revision);
    let _ = ids;
}

/// Hit and miss agree at every budget, and loans are released. At 0 nothing
/// is retained, so both runs read storage.
#[test]
fn budgets_from_zero_to_64_mib_keep_results_and_accounting() {
    for mib in [0usize, 1, 8, 16, 64] {
        let h = Harness::new();
        h.db.set_vector_cache_budget(mib * 1024 * 1024);
        let col = h.db.collection("docs").unwrap();
        col.create_vector_index_with_options(
            "v",
            64,
            Some(VectorMetric::Cosine),
            graph(Quantization::None),
        )
        .unwrap();
        let mut rng = Rng(41 + mib as u64);
        populate(&col, 300, 64, &mut rng);
        let query = rng.vector(64);
        h.clear_cache();
        let before = vector_reads();
        let miss = col.search_vectors("v", &query, 10, None, &ann()).unwrap();
        let miss_reads = vector_reads() - before;
        col.search_vectors("v", &query, 1, None, &exact()).unwrap();
        let retained = h.db.vector_cache_stats().vector_indexes == 1;
        assert_eq!(retained, mib > 0, "{mib} MiB");
        let before = vector_reads();
        let hit = col.search_vectors("v", &query, 10, None, &ann()).unwrap();
        let hit_reads = vector_reads() - before;
        assert_eq!(fingerprint(&miss), fingerprint(&hit), "{mib} MiB");
        assert_eq!(hit_reads == 0, retained, "{mib} MiB");
        assert!(miss_reads > 0);
        let stats = h.db.vector_cache_stats();
        assert_eq!(stats.active_bytes, 0, "{mib} MiB");
        assert!(stats.peak_bytes <= stats.memory_budget_bytes, "{mib} MiB");
        assert!(stats.retained_bytes <= stats.budget_bytes, "{mib} MiB");
    }
}

/// Fixture for the interleaving tests: 64 cosine vectors; the query is the
/// target's own vector, so the target ranks first until it is rewritten.
struct Interleave {
    h: Harness,
    target: ulid::Ulid,
    query: Vec<f32>,
}
impl Interleave {
    fn new() -> Self {
        let h = Harness::new();
        let col = h.db.collection("docs").unwrap();
        col.create_vector_index_with_options(
            "v",
            4,
            Some(VectorMetric::Cosine),
            graph(Quantization::None),
        )
        .unwrap();
        let mut rng = Rng(53);
        let ids = col
            .insert_many(
                (0..64)
                    .map(|i| {
                        vec![
                            ("v".into(), value(&rng.vector(4))),
                            ("tag".into(), Value::Str(format!("old-{i}"))),
                            ("g".into(), Value::Int(i % 5)),
                        ]
                    })
                    .collect(),
            )
            .unwrap();
        let target = ids[9];
        let query = body_vector(&col.find_by_id(target).unwrap().unwrap(), "v");
        Self { h, target, query }
    }
    fn col(&self) -> Collection {
        self.h.db.collection("docs").unwrap()
    }
    /// Commit a new vector and body for the target.
    fn rewrite_target(&self) {
        self.col()
            .update_one(
                Filter::Eq("_id".into(), Value::Str(self.target.to_string())),
                Update::Set(vec![
                    (
                        "v".into(),
                        value(&[
                            -self.query[0],
                            -self.query[1],
                            -self.query[2],
                            -self.query[3],
                        ]),
                    ),
                    ("tag".into(), Value::Str("new".into())),
                ]),
            )
            .unwrap();
    }
    /// Run `options` on another thread, parked on its first read of `prefix`.
    fn start(
        &self,
        prefix: &'static str,
        options: VectorQueryOptions,
    ) -> std::thread::JoinHandle<(VectorQueryResult, u64)> {
        let col = self.col();
        let query = self.query.clone();
        self.h.arm(prefix);
        let reader = std::thread::spawn(move || {
            let before = vector_reads();
            let result = col.search_vectors("v", &query, 5, None, &options).unwrap();
            (result, vector_reads() - before)
        });
        self.h.wait_entered();
        reader
    }
    fn assert_old_snapshot(&self, old: &VectorQueryResult, reference: &VectorQueryResult) {
        assert_eq!(fingerprint(old), fingerprint(reference));
        assert_eq!(old.hits[0].document.id, self.target);
        assert_eq!(
            old.hits[0].document.get("tag"),
            Some(&Value::Str("old-9".into()))
        );
        for hit in &old.hits {
            let v = body_vector(&hit.document, "v");
            let rescored =
                score_with_query_norm(&VectorMetric::Cosine, &self.query, l2_norm(&self.query), &v);
            assert_eq!(
                hit.score.to_bits(),
                rescored.to_bits(),
                "score and body from one snapshot"
            );
        }
    }
}

/// The write and a newer block land while the query is inside its graph walk:
/// after the snapshot, revision and graph lease are taken, before rescoring.
/// The walk reads only its snapshot and leased graph, so rescoring sees the
/// same shared state as if the write had committed just after the walk.
#[test]
fn a_write_and_newer_block_during_the_walk_never_mix_generations() {
    let t = Interleave::new();
    let col = t.col();
    let reference = col.search_vectors("v", &t.query, 5, None, &ann()).unwrap();
    t.h.clear_cache(); // cold graph: the walk reads `hnsw::` tables
    retain_block(&t.h, &col, "v", 4);
    let reader = t.start("hnsw::", ann());
    t.rewrite_target();
    // The current snapshot's exact search replaces the block with the new revision.
    col.search_vectors("v", &t.query, 1, None, &exact())
        .unwrap();
    assert_eq!(t.h.db.vector_cache_stats().vector_indexes, 1);
    t.h.resume.send(()).unwrap();
    let (old, reads) = reader.join().unwrap();
    assert!(
        reads > 0,
        "the old snapshot has no block left and reads storage"
    );
    t.assert_old_snapshot(&old, &reference);
    let current = col.search_vectors("v", &t.query, 5, None, &ann()).unwrap();
    assert_ne!(current.hits[0].document.id, t.target);
    assert_eq!(current.execution.revision, reference.execution.revision + 1);
    assert_eq!(t.h.db.vector_cache_stats().active_bytes, 0);
}

/// The write commits during the walk but the old revision's block stays: the
/// query rescores from it, because it matches the query's snapshot.
#[test]
fn a_write_during_the_walk_leaves_the_snapshot_block_usable() {
    let t = Interleave::new();
    let col = t.col();
    let reference = col.search_vectors("v", &t.query, 5, None, &ann()).unwrap();
    t.h.clear_cache();
    retain_block(&t.h, &col, "v", 4);
    let reader = t.start("hnsw::", ann());
    t.rewrite_target();
    t.h.resume.send(()).unwrap();
    let (old, reads) = reader.join().unwrap();
    assert_eq!(reads, 0, "rescored from the block of its own revision");
    t.assert_old_snapshot(&old, &reference);
}

/// The write and a newer block land after the walk, at the first rescoring
/// read of a query that found no block: the remaining reads stay on its snapshot.
#[test]
fn a_write_between_the_walk_and_rescoring_reads_never_mix_generations() {
    let t = Interleave::new();
    let col = t.col();
    let reference = col.search_vectors("v", &t.query, 5, None, &ann()).unwrap();
    t.h.clear_cache();
    let reader = t.start("vec::", ann());
    t.rewrite_target();
    col.search_vectors("v", &t.query, 1, None, &exact())
        .unwrap();
    assert_eq!(t.h.db.vector_cache_stats().vector_indexes, 1);
    t.h.resume.send(()).unwrap();
    let (old, _) = reader.join().unwrap();
    t.assert_old_snapshot(&old, &reference);
}

/// A grouped query reads group keys while rescoring, so it can be parked
/// mid-rescoring while it borrows the block. Evicting the block then must
/// keep it valid for the reader and counted as active memory until released.
#[test]
fn a_block_evicted_mid_rescoring_stays_valid_and_accounted() {
    let t = Interleave::new();
    let col = t.col();
    let grouped = VectorQueryOptions {
        group_by: Some("g".into()),
        ..ann()
    };
    let reference = col
        .search_vectors("v", &t.query, 5, None, &grouped)
        .unwrap();
    t.h.clear_cache();
    retain_block(&t.h, &col, "v", 4);
    let block_bytes = t.h.db.vector_cache_stats().retained_bytes;
    // Nothing reads documents before rescoring: the first read is a group key.
    let reader = t.start("docs::docs", grouped);
    let before = t.h.db.vector_cache_stats();
    // Resident, the block counts as retained; the reader's graph lease is active.
    assert_eq!(before.retained_bytes, block_bytes);
    t.h.db.set_vector_cache_budget(0);
    let stats = t.h.db.vector_cache_stats();
    assert_eq!(stats.retained_bytes, 0);
    assert_eq!(
        stats.active_bytes,
        before.active_bytes + block_bytes,
        "a borrowed block stays charged"
    );
    let budget = before.budget_bytes;
    t.h.db.set_vector_cache_budget(budget);
    t.rewrite_target();
    col.search_vectors("v", &t.query, 1, None, &exact())
        .unwrap();
    t.h.resume.send(()).unwrap();
    let (old, reads) = reader.join().unwrap();
    assert_eq!(reads, 0, "rescoring finished on the borrowed block");
    t.assert_old_snapshot(&old, &reference);
    assert_eq!(t.h.db.vector_cache_stats().active_bytes, 0);
}

/// Readers, a writer, block-building exact searches and budget changes run
/// together. Every hit's score must be the score of the vector in the
/// document returned with it — one snapshot — and all loans end released.
#[test]
fn concurrent_reads_writes_and_evictions_score_from_one_snapshot() {
    let h = Harness::new();
    let db = &h.db;
    let col = db.collection("docs").unwrap();
    col.create_index("g").unwrap();
    col.create_vector_index_with_options(
        "v",
        16,
        Some(VectorMetric::Cosine),
        graph(Quantization::None),
    )
    .unwrap();
    let ids = populate(&col, 300, 16, &mut Rng(61));
    let stop = AtomicBool::new(false);
    let readers_left = std::sync::atomic::AtomicUsize::new(3);
    let block_hits = std::sync::atomic::AtomicU64::new(0);
    // The background loops run until every reader is done, or one panics:
    // a failed assertion must end the test, not leave the loops spinning.
    struct ReaderExit<'a>(&'a AtomicBool, &'a std::sync::atomic::AtomicUsize);
    impl Drop for ReaderExit<'_> {
        fn drop(&mut self) {
            if std::thread::panicking() || self.1.fetch_sub(1, Ordering::SeqCst) == 1 {
                self.0.store(true, Ordering::SeqCst);
            }
        }
    }
    std::thread::scope(|s| {
        for seed in 0..3u64 {
            let col = db.collection("docs").unwrap();
            let block_hits = &block_hits;
            let exit = ReaderExit(&stop, &readers_left);
            s.spawn(move || {
                let _exit = exit;
                let mut rng = Rng(100 + seed);
                for i in 0..150 {
                    let query = rng.vector(16);
                    let options = match i % 3 {
                        0 => ann(),
                        1 => VectorQueryOptions { offset: 2, ..ann() },
                        _ => VectorQueryOptions {
                            group_by: Some("g".into()),
                            ..ann()
                        },
                    };
                    let before = vector_reads();
                    let result = col.search_vectors("v", &query, 8, None, &options).unwrap();
                    if result.execution.path == "hnsw" && vector_reads() == before {
                        block_hits.fetch_add(1, Ordering::Relaxed);
                    }
                    for hit in &result.hits {
                        let v = body_vector(&hit.document, "v");
                        let rescored = score_with_query_norm(
                            &VectorMetric::Cosine,
                            &query,
                            l2_norm(&query),
                            &v,
                        );
                        assert_eq!(hit.score.to_bits(), rescored.to_bits(), "mixed snapshot");
                    }
                }
            });
        }
        // Retains a block of whatever revision is current, over and over.
        s.spawn(|| {
            let col = db.collection("docs").unwrap();
            while !stop.load(Ordering::Relaxed) {
                col.search_vectors("v", &[0.25; 16], 1, None, &exact())
                    .unwrap();
            }
        });
        s.spawn(|| {
            let col = db.collection("docs").unwrap();
            let mut rng = Rng(77);
            while !stop.load(Ordering::Relaxed) {
                let id = ids[(rng.next() % ids.len() as u64) as usize];
                col.update_one(
                    Filter::Eq("_id".into(), Value::Str(id.to_string())),
                    Update::Set(vec![("v".into(), value(&rng.vector(16)))]),
                )
                .unwrap();
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        s.spawn(|| {
            let budgets = [64usize, 1, 0, 16, 64, 8];
            let mut i = 0;
            while !stop.load(Ordering::Relaxed) {
                db.set_vector_cache_budget(budgets[i % budgets.len()] * 1024 * 1024);
                i += 1;
                std::thread::sleep(Duration::from_millis(15));
            }
            db.set_vector_cache_budget(64 * 1024 * 1024);
        });
    });
    assert!(
        block_hits.load(Ordering::Relaxed) > 0,
        "no query rescored from a block"
    );
    let stats = db.vector_cache_stats();
    assert_eq!(stats.active_bytes, 0);
    assert!(stats.peak_bytes <= stats.memory_budget_bytes);
}
