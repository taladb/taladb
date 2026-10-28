//! Partial retention of the exact-search vector block.
//!
//! When a field's decoded vectors do not fit the cache budget, unfiltered exact
//! search retains the prefix that fits in idle memory: every valid vector up to
//! some ID, in key order. Later searches score that prefix, then stream the
//! rest of the table after it. Results must be what a full scan returns, bit for
//! bit. The oracle here reads the vector table straight from storage and never
//! touches a retained block.
//!
//! A probe backend counts, per thread, the vector-table entries that scans
//! stream and the vector point reads, and can park readers on their first read
//! of a chosen table so that writes and evictions land at known points.

use std::cell::Cell;
use std::collections::HashSet;
use std::ops::Bound;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use taladb::engine::{KvPairs, ReadTxn, ScanFn, WriteTxn};
use taladb::vector::{
    decode_f32_vec, encode_f32_vec, l2_norm, score_with_query_norm, vec_table_name,
};
use taladb::{
    Collection, Database, Document, Filter, GraphOptions, MemoryPressure, Quantization,
    RedbBackend, StorageBackend, TalaDbError, Ulid, Update, Value, VectorMetric,
    VectorQueryOptions, VectorQueryResult, VectorSearchMode,
};

const MIB: usize = 1024 * 1024;
/// The graph-walk workspace a prefix always leaves idle.
const WALK: usize = 64 * 1024;

thread_local! {
    static STREAMED: Cell<u64> = const { Cell::new(0) };
    static POINT_READS: Cell<u64> = const { Cell::new(0) };
}

/// Vector-table entries streamed by scans and vector point reads, this thread.
fn reads() -> (u64, u64) {
    (STREAMED.with(Cell::get), POINT_READS.with(Cell::get))
}

/// Run `f` and return its result with the vector entries it streamed and the
/// vector point reads it made on this thread.
fn counted<T>(f: impl FnOnce() -> T) -> (T, u64, u64) {
    let (s, p) = reads();
    let out = f();
    let (s2, p2) = reads();
    (out, s2 - s, p2 - p)
}

struct Slot {
    prefix: &'static str,
    entered: mpsc::Sender<()>,
    resume: mpsc::Receiver<()>,
}
/// Readers park on their first read of a table matching an armed prefix, one
/// reader per arm, each with its own channels.
#[derive(Default)]
struct Gate(Mutex<Vec<Slot>>);
impl Gate {
    fn pass(&self, table: &str) {
        let slot = {
            let mut slots = self.0.lock().unwrap();
            slots
                .iter()
                .position(|s| table.starts_with(s.prefix))
                .map(|i| slots.remove(i))
        };
        if let Some(slot) = slot {
            slot.entered.send(()).unwrap();
            slot.resume.recv_timeout(Duration::from_secs(30)).unwrap();
        }
    }
}
struct Parked {
    entered: mpsc::Receiver<()>,
    resume: mpsc::Sender<()>,
}
impl Parked {
    fn wait(&self) {
        self.entered.recv_timeout(Duration::from_secs(30)).unwrap();
    }
    fn release(&self) {
        self.resume.send(()).unwrap();
    }
}

struct Probe {
    inner: RedbBackend,
    gate: Arc<Gate>,
}
impl StorageBackend for Probe {
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
fn is_vector_table(table: &str) -> bool {
    table.starts_with("vec::")
}
impl ReadTxn for ProbeRead<'_> {
    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        if is_vector_table(table) {
            POINT_READS.with(|n| n.set(n.get() + 1));
        }
        self.gate.pass(table);
        self.inner.get(table, key)
    }
    fn get_many(&self, table: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        if is_vector_table(table) {
            POINT_READS.with(|n| n.set(n.get() + keys.len() as u64));
        }
        self.gate.pass(table);
        self.inner.get_many(table, keys)
    }
    fn range(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, TalaDbError> {
        self.gate.pass(table);
        let rows = self.inner.range(table, start, end)?;
        if is_vector_table(table) {
            STREAMED.with(|n| n.set(n.get() + rows.len() as u64));
        }
        Ok(rows)
    }
    fn scan(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
        f: ScanFn<'_>,
    ) -> Result<(), TalaDbError> {
        self.gate.pass(table);
        if !is_vector_table(table) {
            return self.inner.scan(table, start, end, f);
        }
        self.inner.scan(table, start, end, &mut |k, v| {
            STREAMED.with(|n| n.set(n.get() + 1));
            f(k, v)
        })
    }
    fn scan_all(&self, table: &str) -> Result<KvPairs, TalaDbError> {
        self.gate.pass(table);
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
}
impl Harness {
    fn new() -> Self {
        let gate = Arc::new(Gate::default());
        let db = Database::open_with_backend(Box::new(Probe {
            inner: RedbBackend::open_in_memory().unwrap(),
            gate: gate.clone(),
        }))
        .unwrap();
        Self { db, gate }
    }
    /// Park the next reader whose read hits a table starting with `prefix`.
    fn arm(&self, prefix: &'static str) -> Parked {
        let (entered_tx, entered) = mpsc::channel();
        let (resume, resume_rx) = mpsc::channel();
        self.gate.0.lock().unwrap().push(Slot {
            prefix,
            entered: entered_tx,
            resume: resume_rx,
        });
        Parked { entered, resume }
    }
    /// Empty the search cache, keeping the budget.
    fn clear_cache(&self) {
        let budget = self.db.vector_cache_stats().budget_bytes;
        self.db.set_vector_cache_budget(0);
        self.db.set_vector_cache_budget(budget);
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / 16_777_216.0 - 0.5
    }
    fn vector(&mut self, dims: usize) -> Vec<f32> {
        (0..dims).map(|_| self.next()).collect()
    }
}

/// Random vectors mixed with zero and negative-zero vectors, magnitudes whose
/// squares underflow or are subnormal, squares that overflow, and identical or
/// opposite vectors (exact score ties, broken by ID) — the Stage 1B corpus.
fn corpus(dims: usize, count: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut rng = Rng(seed | 1);
    let scaled = |rng: &mut Rng, s: f32| rng.vector(dims).iter().map(|x| x * s).collect();
    (0..count)
        .map(|i| match i % 16 {
            0 => vec![0.0; dims],
            1 => vec![-0.0; dims],
            2 => scaled(&mut rng, 1e-20),
            3 => scaled(&mut rng, 1e-41),
            4 => scaled(&mut rng, 1e18),
            5 => scaled(&mut rng, 1e30),
            6 => vec![1.0; dims],
            7 => vec![-1.0; dims],
            _ => rng.vector(dims),
        })
        .collect()
}

/// The corpus without the magnitudes HNSW refuses to index (`1e30`).
fn graph_corpus(dims: usize, count: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut rng = Rng(seed ^ 0x5555);
    let mut data = corpus(dims, count, seed);
    for v in data.iter_mut().skip(5).step_by(16) {
        *v = rng.vector(dims);
    }
    data
}

fn queries(dims: usize, data: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03 ^ dims as u64);
    vec![
        rng.vector(dims),
        rng.vector(dims).iter().map(|x| x * 1e18).collect(),
        vec![0.0; dims],
        vec![1.0; dims],
        data[9].clone(),
    ]
}

/// Document IDs in key order, two apart so that other keys fit between them.
fn id(i: usize) -> Ulid {
    Ulid::from(0x0001_0000_0000_0000u128 + 2 * i as u128)
}

fn embedding(v: &[f32]) -> Value {
    Value::Array(v.iter().map(|f| Value::Float(f64::from(*f))).collect())
}

fn doc(i: usize, field: &str, v: &[f32]) -> Vec<(String, Value)> {
    vec![
        ("_id".into(), Value::Str(id(i).to_string())),
        (field.into(), embedding(v)),
        ("bucket".into(), Value::Int((i % 100) as i64)),
        ("group".into(), Value::Int((i % 7) as i64)),
        (
            "name".into(),
            Value::Str(format!("doc {} word{}", i, i % 5)),
        ),
    ]
}

fn populate(
    db: &Database,
    name: &str,
    dims: usize,
    metric: VectorMetric,
    data: &[Vec<f32>],
) -> Collection {
    let col = db.collection(name).unwrap();
    col.create_vector_index("v", dims, Some(metric), None)
        .unwrap();
    for (start, chunk) in (0..).step_by(500).zip(data.chunks(500)) {
        col.insert_many(
            chunk
                .iter()
                .enumerate()
                .map(|(i, v)| doc(start + i, "v", v))
                .collect(),
        )
        .unwrap();
    }
    col
}

/// Bytes a retained vector costs: ID, values and, for cosine, its norm.
fn per_vector(dims: usize, metric: VectorMetric) -> usize {
    16 + 4 * dims + if metric == VectorMetric::Cosine { 4 } else { 0 }
}

/// The vector table as exact search sees it, read straight from storage: the
/// entries with 16-byte keys that decode to `dims` floats, in key order.
fn table(db: &Database, collection: &str, field: &str, dims: usize) -> Vec<(Ulid, Vec<f32>)> {
    let txn = db.backend().begin_read().unwrap();
    txn.scan_all(&vec_table_name(collection, field))
        .unwrap()
        .into_iter()
        .filter_map(|(key, bytes)| {
            let key = <[u8; 16]>::try_from(key.as_slice()).ok()?;
            let v = decode_f32_vec(&bytes)?;
            (v.len() == dims).then(|| (Ulid::from_bytes(key), v))
        })
        .collect()
}

type Signature = Vec<(Ulid, u32)>;

/// Full-scan exact top-k: every eligible stored vector scored with the
/// pre-cache kernel, non-finite scores dropped, best score first, then lowest
/// ID; vectors without a document are skipped, as result loading does.
fn oracle(
    metric: VectorMetric,
    query: &[f32],
    rows: &[(Ulid, Vec<f32>)],
    eligible: impl Fn(&Ulid) -> bool,
    k: usize,
) -> Signature {
    let norm = l2_norm(query);
    let mut scored: Vec<(Ulid, f32)> = rows
        .iter()
        .filter(|(id, _)| eligible(id))
        .map(|(id, v)| (*id, score_with_query_norm(&metric, query, norm, v)))
        .filter(|(_, s)| s.is_finite())
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(k);
    scored
        .into_iter()
        .map(|(id, s)| (id, s.to_bits()))
        .collect()
}

fn signature(r: &VectorQueryResult) -> Signature {
    r.hits
        .iter()
        .map(|h| (h.document.id, h.score.to_bits()))
        .collect()
}

fn exact() -> VectorQueryOptions {
    VectorQueryOptions {
        mode: VectorSearchMode::Exact,
        ..Default::default()
    }
}

fn search(col: &Collection, query: &[f32], k: usize, filter: Option<Filter>) -> VectorQueryResult {
    col.search_vectors("v", query, k, filter, &exact()).unwrap()
}

/// Everything a caller can observe: hits (ID, score bits, body), next offset,
/// and the execution report.
type Fingerprint = (
    Vec<(Ulid, u32, String)>,
    Option<usize>,
    String,
    String,
    u64,
    usize,
);
fn fingerprint(r: &VectorQueryResult) -> Fingerprint {
    (
        r.hits
            .iter()
            .map(|h| {
                (
                    h.document.id,
                    h.score.to_bits(),
                    format!("{:?}", h.document.fields),
                )
            })
            .collect(),
        r.next_offset,
        r.execution.path.clone(),
        r.execution.reason.clone(),
        r.execution.revision,
        r.execution.distance_computations,
    )
}

/// A budget whose idle memory holds a prefix of `share` vectors.
fn budget_for(share: usize, dims: usize, metric: VectorMetric) -> usize {
    share * per_vector(dims, metric) + WALK
}

/// What a cold exact search retains at `budget` with nothing else resident,
/// for a table of `count` valid vectors: a full block, with norms for cosine,
/// else the same without norms (the unchanged fallback), else the prefix that
/// fits beside the walk workspace if it holds an eighth of the table.
/// Returns (bytes, vectors held).
fn expected_block(
    count: usize,
    budget: usize,
    dims: usize,
    metric: VectorMetric,
) -> (usize, usize) {
    let per = per_vector(dims, metric);
    let prefix = (budget.max(WALK) - WALK).min(budget) / per;
    if count * per <= budget {
        (count * per, count)
    } else if count * (16 + 4 * dims) <= budget {
        (count * (16 + 4 * dims), count)
    } else if prefix >= count.div_ceil(8) {
        (prefix * per, prefix)
    } else {
        (0, 0)
    }
}

/// What the cache holds: (vector blocks, graphs, retained bytes).
fn resident(db: &Database) -> (usize, usize, usize) {
    let s = db.vector_cache_stats();
    (s.vector_indexes, s.graph_indexes, s.retained_bytes)
}

#[test]
fn partial_blocks_score_every_vector_once_and_bit_identically() {
    for metric in [
        VectorMetric::Cosine,
        VectorMetric::Dot,
        VectorMetric::Euclidean,
    ] {
        for dims in [3, 8, 13, 128, 383, 384, 385, 768, 1536] {
            // Large enough that a prefix exists beside the walk workspace.
            let count = (4 * WALK / per_vector(dims, metric)).max(96);
            let data = corpus(dims, count, 0x9E37_79B9_7F4A_7C15 ^ dims as u64);
            let h = Harness::new();
            let col = populate(&h.db, "docs", dims, metric, &data);
            let rows = table(&h.db, "docs", "v", dims);
            assert_eq!(rows.len(), count);
            let top = count - WALK / per_vector(dims, metric) - 1;
            for share in [count.div_ceil(8), count / 2, top] {
                let budget = budget_for(share, dims, metric);
                h.db.set_vector_cache_budget(budget);
                let (bytes, held) = expected_block(count, budget, dims, metric);
                for query in queries(dims, &data) {
                    let expected = oracle(metric, &query, &rows, |_| true, 10);
                    h.clear_cache();
                    let (cold, cold_streamed, _) = counted(|| search(&col, &query, 10, None));
                    assert_eq!(
                        resident(&h.db),
                        (1, 0, bytes),
                        "{metric:?} {dims}-d: a {share}-vector prefix (or the vectors-only block)"
                    );
                    let (warm, warm_streamed, warm_reads) =
                        counted(|| search(&col, &query, 10, None));
                    let what = format!("{metric:?} {dims}-d prefix {share}/{count}");
                    assert_eq!(signature(&cold), expected, "{what} cold");
                    assert_eq!(signature(&warm), expected, "{what} warm");
                    // One score per stored vector, on both passes.
                    assert_eq!(cold.execution.distance_computations, count, "{what}");
                    assert_eq!(warm.execution.distance_computations, count, "{what}");
                    // The cold pass streams the table once; the warm pass only
                    // what lies after the prefix, and reads nothing else.
                    assert_eq!(cold_streamed, count as u64, "{what}");
                    assert_eq!(warm_streamed, (count - held) as u64, "{what}");
                    assert_eq!(warm_reads, 0, "{what}");
                }
            }
        }
    }
}

#[test]
fn every_prefix_boundary_ranks_the_whole_table_exactly_once() {
    // `k` above the table size returns every vector, so omitting or
    // double-scoring one at any boundary changes the list.
    let (dims, count) = (385, 128);
    for metric in [
        VectorMetric::Cosine,
        VectorMetric::Dot,
        VectorMetric::Euclidean,
    ] {
        let data = corpus(dims, count, 0x2545_F491_4F6C_DD1D);
        let h = Harness::new();
        let col = populate(&h.db, "docs", dims, metric, &data);
        let rows = table(&h.db, "docs", "v", dims);
        let query = queries(dims, &data)[0].clone();
        let expected = oracle(metric, &query, &rows, |_| true, 100);
        for share in count.div_ceil(8)..=count {
            let budget = budget_for(share, dims, metric);
            h.db.set_vector_cache_budget(budget);
            h.clear_cache();
            let cold = search(&col, &query, 100, None);
            let (warm, streamed, _) = counted(|| search(&col, &query, 100, None));
            assert_eq!(signature(&cold), expected, "{metric:?} share {share}");
            assert_eq!(signature(&warm), expected, "{metric:?} share {share}");
            assert_eq!(warm.execution.distance_computations, count);
            // Near the top a complete block fits (for cosine first without
            // norms, the unchanged fallback) and streams nothing.
            let (bytes, held) = expected_block(count, budget, dims, metric);
            assert_eq!(resident(&h.db), (1, 0, bytes), "{metric:?} share {share}");
            assert_eq!(streamed, (count - held) as u64, "{metric:?} share {share}");
        }
        // Below the minimum useful prefix (an eighth of the table) nothing is
        // retained and every query streams.
        h.db.set_vector_cache_budget(budget_for(count.div_ceil(8) - 1, dims, metric));
        let (r, streamed, _) = counted(|| search(&col, &query, 100, None));
        assert_eq!(signature(&r), expected);
        assert_eq!(resident(&h.db), (0, 0, 0));
        assert_eq!(streamed, count as u64);
    }
}

#[test]
fn malformed_records_and_foreign_keys_around_the_boundary_are_skipped_once() {
    let (dims, count) = (1024, 48);
    let metric = VectorMetric::Cosine;
    let data = corpus(dims, count, 0xBF58_476D_1CE4_E5B9);
    let h = Harness::new();
    let col = populate(&h.db, "docs", dims, metric, &data);
    // Behind the write path's back, as a crafted snapshot or corruption would
    // leave them, without moving the revision: a truncated vector, bytes that
    // do not decode, a vector that is too long, keys that are not 16 bytes
    // (one sorting just after a valid ID, one just before), and a valid vector
    // whose document is missing.
    let name = vec_table_name("docs", "v");
    let after_12 = [id(12).to_bytes().as_slice(), &[0]].concat();
    let before_20 = id(20).to_bytes()[..15].to_vec();
    let orphan = Ulid::from(u128::from(id(30)) + 1);
    {
        let mut w = h.db.backend().begin_write().unwrap();
        w.put(&name, &id(10).to_bytes(), &encode_f32_vec(&[1.0, 0.0]))
            .unwrap();
        w.put(&name, &id(11).to_bytes(), &[1, 2, 3, 4, 5, 6])
            .unwrap();
        w.put(&name, &id(13).to_bytes(), &encode_f32_vec(&[1.0; 1025]))
            .unwrap();
        w.put(&name, &after_12, &encode_f32_vec(&[1.0; 1024]))
            .unwrap();
        w.put(&name, &before_20, &encode_f32_vec(&[1.0; 1024]))
            .unwrap();
        w.put(&name, &orphan.to_bytes(), &encode_f32_vec(&[0.5; 1024]))
            .unwrap();
        w.commit().unwrap();
    }
    let rows = table(&h.db, "docs", "v", dims);
    assert_eq!(rows.len(), count - 3 + 1, "three malformed, one orphan");
    let raw: Vec<Vec<u8>> = {
        let txn = h.db.backend().begin_read().unwrap();
        txn.scan_all(&name)
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect()
    };
    for query in queries(dims, &data) {
        let has_doc = |id: &Ulid| *id != orphan;
        let expected = oracle(metric, &query, &rows, has_doc, 100);
        // The minimum prefix is an eighth of the table's entries, valid or
        // not; full blocks are sized by entries too.
        for share in raw.len().div_ceil(8)..rows.len() {
            let budget = budget_for(share, dims, metric);
            if budget >= raw.len() * (16 + 4 * dims) {
                break;
            }
            h.db.set_vector_cache_budget(budget);
            h.clear_cache();
            let cold = search(&col, &query, 100, None);
            let (warm, streamed, _) = counted(|| search(&col, &query, 100, None));
            assert_eq!(signature(&cold), expected, "share {share}");
            assert_eq!(signature(&warm), expected, "share {share}");
            assert_eq!(warm.execution.distance_computations, rows.len());
            // The prefix holds the first `share` *valid* vectors; the warm
            // pass streams every raw key after the last of them, once.
            let last = rows[share - 1].0.to_bytes();
            let after = raw
                .iter()
                .filter(|k| k.as_slice() > last.as_slice())
                .count();
            assert_eq!(streamed, after as u64, "share {share}");
            for bad in [id(10), id(11), id(13), orphan] {
                assert!(warm.hits.iter().all(|h| h.document.id != bad));
            }
        }
    }
}

#[test]
fn ties_across_the_boundary_break_by_id() {
    // Identical vectors on both sides of the boundary: the cut at `k` falls
    // inside the tie run, so the lowest IDs win wherever they are stored.
    let (dims, count, share) = (512, 64, 20);
    let mut data = corpus(dims, count, 0x94D0_49BB_1331_11EB);
    for i in [2, 7, 19, 20, 21, 40, 63] {
        data[i] = vec![0.25; dims];
    }
    for metric in [
        VectorMetric::Cosine,
        VectorMetric::Dot,
        VectorMetric::Euclidean,
    ] {
        let h = Harness::new();
        let col = populate(&h.db, "docs", dims, metric, &data);
        let rows = table(&h.db, "docs", "v", dims);
        h.db.set_vector_cache_budget(budget_for(share, dims, metric));
        let query = vec![0.25; dims];
        for k in [1, 3, 4, 5, 7, 10] {
            let expected = oracle(metric, &query, &rows, |_| true, k);
            h.clear_cache();
            assert_eq!(signature(&search(&col, &query, k, None)), expected);
            assert_eq!(
                signature(&search(&col, &query, k, None)),
                expected,
                "{metric:?} k {k}"
            );
            assert_eq!(resident(&h.db).0, 1);
        }
    }
}

#[test]
fn filtered_searches_over_a_partial_block_match_streaming_and_the_oracle() {
    // 1,000 vectors: `bucket == 1` matches 10 (batched point reads), `bucket <
    // 20` matches 200 (an ID set checked against the prefix, then a scan of
    // the rest), `bucket < 50` and the `$or` match 500 and 620 (above the
    // 256-ID cap, so the filter is visited). With an index on `bucket` and a
    // full-text index the visit follows index order, not ID order.
    for (dims, indexed) in [(64, false), (128, false), (64, true)] {
        let count = 1000;
        let data = corpus(dims, count, 0xA076_1D64_78BD_642F ^ dims as u64);
        let h = Harness::new();
        let col = populate(&h.db, "docs", dims, VectorMetric::Cosine, &data);
        if indexed {
            col.create_index("bucket").unwrap();
            col.create_fts_index("name").unwrap();
        }
        let rows = table(&h.db, "docs", "v", dims);
        let filters = [
            Filter::Eq("bucket".into(), Value::Int(1)),
            Filter::Lt("bucket".into(), Value::Int(20)),
            Filter::Lt("bucket".into(), Value::Int(50)),
            Filter::Or(vec![
                Filter::Gte("bucket".into(), Value::Int(88)),
                Filter::Lt("group".into(), Value::Int(4)),
            ]),
        ];
        let share = count / 2;
        for query in queries(dims, &data) {
            for filter in &filters {
                let allowed: HashSet<Ulid> = col
                    .find(filter.clone())
                    .unwrap()
                    .iter()
                    .map(|d| d.id)
                    .collect();
                let expected = oracle(
                    VectorMetric::Cosine,
                    &query,
                    &rows,
                    |id| allowed.contains(id),
                    10,
                );
                h.db.set_vector_cache_budget(0);
                let streamed = search(&col, &query, 10, Some(filter.clone()));
                h.db.set_vector_cache_budget(budget_for(share, dims, VectorMetric::Cosine));
                search(&col, &query, 10, None); // builds the prefix
                assert_eq!(resident(&h.db).0, 1);
                let (cached, _, _) = counted(|| search(&col, &query, 10, Some(filter.clone())));
                let what = format!("{dims}-d indexed={indexed} {filter:?}");
                assert_eq!(signature(&streamed), expected, "{what}");
                assert_eq!(fingerprint(&cached), fingerprint(&streamed), "{what}");
                assert_eq!(
                    cached.execution.distance_computations,
                    rows.iter().filter(|(id, _)| allowed.contains(id)).count(),
                    "{what}: each eligible vector scored once"
                );
                // A filtered search never builds or replaces a block.
                assert_eq!(resident(&h.db).0, 1);
            }
        }
    }
}

#[test]
fn grouping_offsets_thresholds_and_pages_match_streaming() {
    let (dims, count) = (64, 600);
    let metric = VectorMetric::Cosine;
    let data = corpus(dims, count, 0xE703_7ED1_A0B4_28DB);
    let h = Harness::new();
    let col = populate(&h.db, "docs", dims, metric, &data);
    let option_sets = [
        VectorQueryOptions {
            offset: 5,
            ..exact()
        },
        VectorQueryOptions {
            score_threshold: Some(0.1),
            ..exact()
        },
        VectorQueryOptions {
            group_by: Some("group".into()),
            group_size: Some(2),
            ..exact()
        },
        VectorQueryOptions {
            group_by: Some("bucket".into()),
            offset: 3,
            ..exact()
        },
        VectorQueryOptions {
            group_by: Some("group".into()),
            group_size: Some(3),
            score_threshold: Some(-0.2),
            offset: 4,
            ..exact()
        },
    ];
    let filters = [None, Some(Filter::Lt("bucket".into(), Value::Int(60)))];
    let run = |query: &[f32], options: &VectorQueryOptions, filter: &Option<Filter>| {
        // Page through the whole result with `next_offset`.
        let mut pages = Vec::new();
        let mut options = options.clone();
        loop {
            let r = col
                .search_vectors("v", query, 7, filter.clone(), &options)
                .unwrap();
            let next = r.next_offset;
            pages.push(fingerprint(&r));
            match next {
                Some(offset) if pages.len() < 12 => options.offset = offset,
                _ => break,
            }
        }
        pages
    };
    for query in queries(dims, &data) {
        for options in &option_sets {
            for filter in &filters {
                h.db.set_vector_cache_budget(0);
                let streamed = run(&query, options, filter);
                h.db.set_vector_cache_budget(budget_for(count / 3, dims, metric));
                search(&col, &query, 1, None);
                assert_eq!(resident(&h.db).0, 1, "prefix retained");
                assert_eq!(
                    run(&query, options, filter),
                    streamed,
                    "{options:?} {filter:?}"
                );
                // And when the paging query is the one that builds it.
                h.clear_cache();
                assert_eq!(
                    run(&query, options, filter),
                    streamed,
                    "{options:?} {filter:?}"
                );
            }
        }
    }
}

#[test]
fn inserts_updates_and_deletes_rebuild_the_prefix_for_each_revision() {
    let (dims, count) = (128, 400);
    let metric = VectorMetric::Cosine;
    let data = corpus(dims, count, 0x6A09_E667_F3BC_C908);
    let h = Harness::new();
    let col = populate(&h.db, "docs", dims, metric, &data);
    let share = 200;
    h.db.set_vector_cache_budget(budget_for(share, dims, metric));
    let query = queries(dims, &data)[0].clone();
    let check = |what: &str, rebuilt: bool| {
        let rows = table(&h.db, "docs", "v", dims);
        let expected = oracle(metric, &query, &rows, |_| true, 10);
        let before = h.db.vector_cache_stats().retained_bytes;
        let (cold, cold_streamed, _) = counted(|| search(&col, &query, 10, None));
        let (warm, warm_streamed, _) = counted(|| search(&col, &query, 10, None));
        assert_eq!(signature(&cold), expected, "{what}");
        assert_eq!(signature(&warm), expected, "{what}");
        assert_eq!(cold.execution.revision, warm.execution.revision);
        let capacity = h.db.vector_cache_stats().retained_bytes / per_vector(dims, metric);
        assert_eq!(warm_streamed, (rows.len() - capacity) as u64, "{what}");
        if rebuilt {
            assert_eq!(
                cold_streamed,
                rows.len() as u64,
                "{what}: new revision, new prefix"
            );
        } else {
            assert_eq!(before, h.db.vector_cache_stats().retained_bytes);
            assert_eq!(
                cold_streamed, warm_streamed,
                "{what}: the prefix still serves"
            );
        }
    };
    check("initial", true);
    let mut rng = Rng(77);
    // After every ID, inside the prefix, and before every ID.
    col.insert(doc(5000, "v", &rng.vector(dims))).unwrap();
    check("insert after", true);
    let mut early = doc(0, "v", &rng.vector(dims));
    early[0].1 = Value::Str(Ulid::from(u128::from(id(50)) + 1).to_string());
    col.insert(early).unwrap();
    check("insert inside", true);
    let mut first = doc(0, "v", &rng.vector(dims));
    first[0].1 = Value::Str(Ulid::from(1u128).to_string());
    col.insert(first).unwrap();
    check("insert first", true);
    for (what, i) in [
        ("update inside", 20),
        ("update boundary", share - 3),
        ("update after", 350),
    ] {
        col.update_one(
            Filter::Eq("_id".into(), Value::Str(id(i).to_string())),
            Update::Set(vec![("v".into(), embedding(&query))]),
        )
        .unwrap();
        check(what, true);
    }
    // The last vector the prefix holds, then a range spanning the boundary.
    col.delete_one(Filter::Eq(
        "_id".into(),
        Value::Str(id(share - 3).to_string()),
    ))
    .unwrap();
    check("delete boundary", true);
    col.delete_many(Filter::And(vec![
        Filter::Gte("bucket".into(), Value::Int(90)),
        Filter::Lt("bucket".into(), Value::Int(95)),
    ]))
    .unwrap();
    check("delete range", true);
    // Writes that leave every vector alone keep the revision and the prefix.
    col.update_many(
        Filter::Lt("bucket".into(), Value::Int(30)),
        Update::Set(vec![("name".into(), Value::Str("renamed".into()))]),
    )
    .unwrap();
    check("metadata update", false);
}

#[test]
fn collections_and_fields_keep_separate_prefixes_that_give_way_to_full_blocks() {
    let h = Harness::new();
    let metric = VectorMetric::Cosine;
    let a = populate(&h.db, "a", 256, metric, &corpus(256, 400, 1));
    let b = populate(&h.db, "b", 256, metric, &corpus(256, 400, 2));
    // A second vector field on `a`, small enough for a complete block.
    a.create_vector_index("w", 64, Some(VectorMetric::Euclidean), None)
        .unwrap();
    a.update_many(
        Filter::Lt("bucket".into(), Value::Int(100)),
        Update::Set(vec![("w".into(), embedding(&corpus(64, 9, 4)[8]))]),
    )
    .unwrap();
    let per = per_vector(256, metric);
    h.db.set_vector_cache_budget(budget_for(300, 256, metric));
    let q = queries(256, &corpus(256, 16, 3))[0].clone();
    let search_on = |(name, col): (&str, &Collection), field: &str, dims: usize, k: usize| {
        let query = &q[..dims];
        let rows = table(&h.db, name, field, dims);
        let m = if field == "w" {
            VectorMetric::Euclidean
        } else {
            metric
        };
        let r = col.search_vectors(field, query, k, None, &exact()).unwrap();
        assert_eq!(
            signature(&r),
            oracle(m, query, &rows, |_| true, k),
            "{name}::{field}"
        );
    };
    let (a, b) = (("a", &a), ("b", &b));
    search_on(a, "v", 256, 10);
    assert_eq!(resident(&h.db), (1, 0, 300 * per), "a::v prefix");
    // No idle memory is left, so `b` streams and evicts nothing.
    let (_, streamed, _) = counted(|| search_on(b, "v", 256, 10));
    assert_eq!(streamed, 400);
    assert_eq!(resident(&h.db), (1, 0, 300 * per));
    // A complete block evicts the prefix first.
    search_on(a, "w", 64, 10);
    let w_bytes = 400 * (16 + 4 * 64);
    assert_eq!(
        resident(&h.db),
        (1, 0, w_bytes),
        "a::w complete, a::v prefix evicted"
    );
    // `b` now retains the prefix that fits beside it.
    search_on(b, "v", 256, 10);
    let capacity = (300 * per - w_bytes) / per;
    assert_eq!(resident(&h.db), (2, 0, w_bytes + capacity * per));
    let (_, streamed, _) = counted(|| search_on(b, "v", 256, 10));
    assert_eq!(streamed, (400 - capacity) as u64);
    search_on(a, "v", 256, 10);
    search_on(a, "w", 64, 10);
}

#[test]
fn reopening_rebuilds_the_same_prefix_and_results() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("partial.db");
    let (dims, count) = (256, 300);
    let metric = VectorMetric::Cosine;
    let data = corpus(dims, count, 0x3C6E_F372_FE94_F82B);
    let budget = budget_for(150, dims, metric);
    let query = queries(dims, &data)[0].clone();
    let first = {
        let db = Database::open(&path).unwrap();
        db.set_vector_cache_budget(budget);
        let col = populate(&db, "docs", dims, metric, &data);
        search(&col, &query, 10, None);
        let warm = search(&col, &query, 10, None);
        assert_eq!(resident(&db), (1, 0, budget - WALK));
        assert_eq!(
            signature(&warm),
            oracle(metric, &query, &table(&db, "docs", "v", dims), |_| true, 10)
        );
        fingerprint(&warm)
    };
    let db = Database::open(&path).unwrap();
    db.set_vector_cache_budget(budget);
    let col = db.collection("docs").unwrap();
    assert_eq!(fingerprint(&search(&col, &query, 10, None)), first);
    assert_eq!(resident(&db), (1, 0, budget - WALK));
    assert_eq!(fingerprint(&search(&col, &query, 10, None)), first);
}

/// Build a small HNSW graph for `v` (m 8, efConstruction 32) at 64 MiB, as
/// builds are slow under tiny budgets, then restore the budget.
fn build_graph(db: &Database, col: &Collection) {
    let budget = db.vector_cache_stats().budget_bytes;
    db.set_vector_cache_budget(64 * MIB);
    let options = GraphOptions {
        m: 8,
        ef_construction: 32,
        quantization: Quantization::None,
    };
    let build = col.begin_vector_build("v", Some(options)).unwrap();
    while col.step_vector_build("v", &build.id, 1024).unwrap().state != "ready" {}
    db.set_vector_cache_budget(budget);
}

fn ann() -> VectorQueryOptions {
    VectorQueryOptions {
        mode: VectorSearchMode::Ann,
        ..Default::default()
    }
}

#[test]
fn graphs_reclaim_partial_blocks_and_block_new_ones_until_rebuilt() {
    let (dims, count) = (64, 600);
    let metric = VectorMetric::Cosine;
    let data = graph_corpus(dims, count, 0xBB67_AE85_84CA_A73B);
    let h = Harness::new();
    let col = populate(&h.db, "docs", dims, metric, &data);
    let budget = budget_for(count / 2, dims, metric);
    h.db.set_vector_cache_budget(budget);
    build_graph(&h.db, &col);
    let rows = table(&h.db, "docs", "v", dims);
    let query = queries(dims, &data)[0].clone();
    let expected = oracle(metric, &query, &rows, |_| true, 10);
    // A finished build leaves nothing resident, so exact search retains a prefix.
    assert_eq!(resident(&h.db).1, 0);
    assert_eq!(signature(&search(&col, &query, 10, None)), expected);
    assert_eq!(resident(&h.db).0, 1);
    // A graph walk takes back the prefix's memory before reserving its own.
    h.clear_cache();
    let reference = col.search_vectors("v", &query, 10, None, &ann()).unwrap();
    assert_eq!(reference.execution.path, "hnsw");
    h.clear_cache();
    search(&col, &query, 10, None);
    assert_eq!(resident(&h.db).0, 1, "prefix rebuilt");
    let walked = col.search_vectors("v", &query, 10, None, &ann()).unwrap();
    assert_eq!(fingerprint(&walked), fingerprint(&reference));
    assert_eq!(resident(&h.db).0, 0, "the walk reclaimed the prefix");
    assert_eq!(resident(&h.db).1, 1, "and left its graph");
    // While the graph is resident, exact search streams and retains nothing.
    let (r, streamed, _) = counted(|| search(&col, &query, 10, None));
    assert_eq!(signature(&r), expected);
    assert_eq!(streamed, count as u64);
    assert_eq!(resident(&h.db).0, 0);
    // A rebuild evicts the field; the next exact search retains a prefix again.
    build_graph(&h.db, &col);
    assert_eq!(signature(&search(&col, &query, 10, None)), expected);
    assert_eq!(resident(&h.db), (1, 0, budget - WALK));
}

#[test]
fn ann_rescoring_reads_candidates_beyond_a_partial_prefix_from_storage() {
    // The one way a partial block meets ANN rescoring: an exact search reserves
    // its prefix before a walk starts (so the walk cannot reclaim it) and
    // installs it before the walk rescores. The prefix leaves the walk its
    // 64 KiB workspace.
    // Random vectors: the edge-case corpus's duplicates make walks too short
    // to return candidates on both sides.
    let (dims, count) = (32, 2000);
    let metric = VectorMetric::Cosine;
    let mut rng = Rng(0x510E_527F_ADE6_82D1);
    let data: Vec<_> = (0..count).map(|_| rng.vector(dims)).collect();
    let h = Harness::new();
    let col = populate(&h.db, "docs", dims, metric, &data);
    let budget = 256 * 1024;
    h.db.set_vector_cache_budget(budget);
    build_graph(&h.db, &col);
    for query in queries(dims, &data).into_iter().take(2) {
        let options = VectorQueryOptions {
            ef_search: Some(64),
            ..ann()
        };
        h.clear_cache();
        let (reference, _, reference_reads) =
            counted(|| col.search_vectors("v", &query, 10, None, &options).unwrap());
        assert_eq!(reference.execution.path, "hnsw");
        h.clear_cache();
        let exact_parked = h.arm("vec::");
        std::thread::scope(|s| {
            let exact_reader = s.spawn(|| search(&col, &query, 10, None));
            exact_parked.wait(); // the prefix is reserved, not yet built
            let walk_parked = h.arm("hnsw::");
            let ann_reader = s
                .spawn(|| counted(|| col.search_vectors("v", &query, 10, None, &options).unwrap()));
            walk_parked.wait(); // the lease is taken
            exact_parked.release();
            exact_reader.join().unwrap();
            assert_eq!(resident(&h.db).0, 1, "prefix installed during the walk");
            walk_parked.release();
            let (hit, _, reads) = ann_reader.join().unwrap();
            assert_eq!(fingerprint(&hit), fingerprint(&reference));
            // Candidates inside the prefix came from it, the rest from storage
            // (the reference reads every candidate).
            assert!(
                reads > 0 && reads < reference_reads,
                "{reads} of {reference_reads}"
            );
        });
    }
}

#[test]
fn a_write_during_a_partial_hit_never_mixes_revisions() {
    let (dims, count) = (128, 300);
    let metric = VectorMetric::Cosine;
    let data = corpus(dims, count, 0x9B05_688C_2B3E_6C1F);
    let h = Harness::new();
    let col = populate(&h.db, "docs", dims, metric, &data);
    h.db.set_vector_cache_budget(budget_for(150, dims, metric));
    let query = queries(dims, &data)[0].clone();
    search(&col, &query, 10, None);
    let old = oracle(
        metric,
        &query,
        &table(&h.db, "docs", "v", dims),
        |_| true,
        10,
    );
    let parked = h.arm("vec::");
    std::thread::scope(|s| {
        // Parked at its scan of the rest of the table, holding the prefix.
        let reader = s.spawn(|| search(&col, &query, 10, None));
        parked.wait();
        // The query vector itself, inside and after the prefix: a mixed read
        // would rank either first.
        for i in [10, 250] {
            col.update_one(
                Filter::Eq("_id".into(), Value::Str(id(i).to_string())),
                Update::Set(vec![("v".into(), embedding(&query))]),
            )
            .unwrap();
        }
        let new = oracle(
            metric,
            &query,
            &table(&h.db, "docs", "v", dims),
            |_| true,
            10,
        );
        assert_ne!(new, old);
        // The new revision drops the old prefix, which the reader still
        // borrows, so nothing new is retained until the reader is done.
        let (r, streamed, _) = counted(|| search(&col, &query, 10, None));
        assert_eq!(signature(&r), new);
        assert_eq!(streamed, count as u64);
        assert_eq!(resident(&h.db).0, 0);
        parked.release();
        let r = reader.join().unwrap();
        assert_eq!(signature(&r), old);
        assert_eq!(signature(&search(&col, &query, 10, None)), new);
        let (r, streamed, _) = counted(|| search(&col, &query, 10, None));
        assert_eq!(signature(&r), new);
        assert_eq!(streamed, (count - 150) as u64);
    });
    assert_eq!(h.db.vector_cache_stats().active_bytes, 0);
}

#[test]
fn a_prefix_built_from_an_old_snapshot_is_never_served_for_a_newer_one() {
    let (dims, count) = (128, 300);
    let metric = VectorMetric::Cosine;
    let data = corpus(dims, count, 0x1F83_D9AB_FB41_BD6B);
    let h = Harness::new();
    let col = populate(&h.db, "docs", dims, metric, &data);
    h.db.set_vector_cache_budget(budget_for(150, dims, metric));
    let query = queries(dims, &data)[0].clone();
    let old = oracle(
        metric,
        &query,
        &table(&h.db, "docs", "v", dims),
        |_| true,
        10,
    );
    let parked = h.arm("vec::");
    std::thread::scope(|s| {
        // Cold: parked after reserving its prefix, before building it.
        let reader = s.spawn(|| search(&col, &query, 10, None));
        parked.wait();
        assert!(
            h.db.vector_cache_stats().active_bytes > 0,
            "prefix reserved"
        );
        col.update_one(
            Filter::Eq("_id".into(), Value::Str(id(5).to_string())),
            Update::Set(vec![("v".into(), embedding(&query))]),
        )
        .unwrap();
        let new = oracle(
            metric,
            &query,
            &table(&h.db, "docs", "v", dims),
            |_| true,
            10,
        );
        // The reservation is live, so this query streams and retains nothing.
        let (r, streamed, _) = counted(|| search(&col, &query, 10, None));
        assert_eq!(signature(&r), new);
        assert_eq!(streamed, count as u64);
        parked.release();
        assert_eq!(signature(&reader.join().unwrap()), old);
        // The old revision's prefix may be installed; it must not serve.
        let (r, streamed, _) = counted(|| search(&col, &query, 10, None));
        assert_eq!(signature(&r), new);
        assert_eq!(streamed, count as u64, "rebuilt for the new revision");
        let (r, streamed, _) = counted(|| search(&col, &query, 10, None));
        assert_eq!(signature(&r), new);
        assert_eq!(streamed, (count - 150) as u64);
    });
}

#[test]
fn a_prefix_evicted_mid_query_stays_valid_and_accounted() {
    let (dims, count) = (128, 300);
    let metric = VectorMetric::Cosine;
    let data = corpus(dims, count, 0x5BE0_CD19_137E_2179);
    let h = Harness::new();
    let col = populate(&h.db, "docs", dims, metric, &data);
    let budget = budget_for(150, dims, metric);
    h.db.set_vector_cache_budget(budget);
    let query = queries(dims, &data)[0].clone();
    let expected = oracle(
        metric,
        &query,
        &table(&h.db, "docs", "v", dims),
        |_| true,
        10,
    );
    search(&col, &query, 10, None);
    let parked = h.arm("vec::");
    std::thread::scope(|s| {
        let reader = s.spawn(|| search(&col, &query, 10, None));
        parked.wait();
        h.db.notify_memory_pressure(MemoryPressure::Critical);
        let stats = h.db.vector_cache_stats();
        assert_eq!(stats.retained_bytes, 0);
        assert_eq!(
            stats.active_bytes,
            budget - WALK,
            "the borrowed prefix stays charged"
        );
        parked.release();
        assert_eq!(signature(&reader.join().unwrap()), expected);
    });
    assert_eq!(h.db.vector_cache_stats().active_bytes, 0);
    h.db.notify_memory_pressure(MemoryPressure::Normal);
}

#[test]
fn memory_pressure_shrinks_drops_and_restores_the_prefix() {
    let (dims, count) = (128, 2000);
    let metric = VectorMetric::Cosine;
    let data = corpus(dims, count, 0xCBBB_9D5D_C105_9ED8);
    let h = Harness::new();
    let col = populate(&h.db, "docs", dims, metric, &data);
    let per = per_vector(dims, metric);
    let budget = MIB;
    h.db.set_vector_cache_budget(budget);
    let query = queries(dims, &data)[0].clone();
    let expected = oracle(
        metric,
        &query,
        &table(&h.db, "docs", "v", dims),
        |_| true,
        10,
    );
    let mut seen = Vec::new();
    for pressure in [
        MemoryPressure::Normal,
        MemoryPressure::Moderate,
        MemoryPressure::Critical,
        MemoryPressure::Moderate,
        MemoryPressure::Normal,
    ] {
        h.db.notify_memory_pressure(pressure);
        assert_eq!(
            signature(&search(&col, &query, 10, None)),
            expected,
            "{pressure:?}"
        );
        let (r, streamed, _) = counted(|| search(&col, &query, 10, None));
        assert_eq!(signature(&r), expected, "{pressure:?}");
        let retained = h.db.vector_cache_stats().retained_bytes;
        assert_eq!(streamed, (count - retained / per) as u64, "{pressure:?}");
        assert!(retained <= h.db.vector_cache_stats().budget_bytes);
        seen.push(retained / per);
    }
    // A prefix, a smaller one for a quarter of the budget (above the 250
    // minimum), nothing, and back.
    let normal = expected_block(count, budget, dims, metric).1;
    let moderate = expected_block(count, budget / 4, dims, metric).1;
    assert!(normal > moderate && moderate >= count.div_ceil(8) && normal < count);
    assert_eq!(seen, [normal, moderate, 0, moderate, normal]);
}

#[test]
fn budgets_from_zero_to_64_mib_keep_results_and_accounting() {
    // 3,000 × 1,536-d cosine: 18.5 MB decoded, so 4, 8 and 16 MiB hold a
    // prefix, 1 MiB is below the minimum useful share, and 64 MiB holds all.
    let (dims, count) = (1536, 3000);
    let metric = VectorMetric::Cosine;
    let data = corpus(dims, count, 0x6295_2E2F_E1D3_4AC5);
    let h = Harness::new();
    let col = populate(&h.db, "docs", dims, metric, &data);
    let rows = table(&h.db, "docs", "v", dims);
    let per = per_vector(dims, metric);
    let query = queries(dims, &data)[0].clone();
    let expected = oracle(metric, &query, &rows, |_| true, 10);
    for (budget, held) in [
        (0, 0),
        (MIB, 0),
        (4 * MIB, (4 * MIB - WALK) / per),
        (8 * MIB, (8 * MIB - WALK) / per),
        (16 * MIB, (16 * MIB - WALK) / per),
        (64 * MIB, count),
    ] {
        assert_eq!(expected_block(count, budget, dims, metric).1, held);
        h.db.set_vector_cache_budget(0);
        h.db.set_vector_cache_budget(budget);
        let (cold, cold_streamed, _) = counted(|| search(&col, &query, 10, None));
        let (warm, warm_streamed, _) = counted(|| search(&col, &query, 10, None));
        assert_eq!(signature(&cold), expected, "{budget}");
        assert_eq!(signature(&warm), expected, "{budget}");
        assert_eq!(cold_streamed, count as u64);
        assert_eq!(warm_streamed, (count - held) as u64, "{budget}");
        let stats = h.db.vector_cache_stats();
        assert_eq!(stats.retained_bytes, held * per, "{budget}");
        assert_eq!(stats.active_bytes, 0);
        assert!(stats.peak_bytes <= stats.memory_budget_bytes, "{budget}");
    }
}

#[test]
fn concurrent_reads_writes_and_evictions_score_from_one_snapshot() {
    let (dims, count) = (64, 600);
    let metric = VectorMetric::Cosine;
    let data = corpus(dims, count, 0x0123_4567_89AB_CDEF);
    let h = Harness::new();
    let col = populate(&h.db, "docs", dims, metric, &data);
    let budget = |share| budget_for(share, dims, metric);
    let budgets = [0, budget(100), budget(300), budget(550), 4 * MIB, 64 * MIB];
    h.db.set_vector_cache_budget(budgets[2]);
    let stop = AtomicBool::new(false);
    let queries_done = AtomicUsize::new(0);
    let body = |d: &Document| match d.get("v") {
        Some(Value::Array(items)) => items
            .iter()
            .map(|x| match x {
                Value::Float(f) => *f as f32,
                other => panic!("component {other:?}"),
            })
            .collect::<Vec<f32>>(),
        other => panic!("no vector: {other:?}"),
    };
    // Stop the helper loops if a reader panics, so the scope can end.
    struct StopOnUnwind<'a>(&'a AtomicBool);
    impl Drop for StopOnUnwind<'_> {
        fn drop(&mut self) {
            if std::thread::panicking() {
                self.0.store(true, Ordering::SeqCst);
            }
        }
    }
    std::thread::scope(|s| {
        let readers: Vec<_> = (0..3)
            .map(|r| {
                let (col, stop, done) = (&col, &stop, &queries_done);
                s.spawn(move || {
                    let _guard = StopOnUnwind(stop);
                    let mut rng = Rng(r + 11);
                    for n in 0..120 {
                        let query = rng.vector(dims);
                        let norm = l2_norm(&query);
                        let filter = (r == 1).then(|| Filter::Lt("bucket".into(), Value::Int(70)));
                        let options = VectorQueryOptions {
                            group_by: (r == 2).then(|| "group".into()),
                            ..exact()
                        };
                        let res = col
                            .search_vectors("v", &query, 8, filter, &options)
                            .unwrap();
                        let mut seen = HashSet::new();
                        for hit in &res.hits {
                            assert!(seen.insert(hit.document.id), "duplicate hit");
                            let v = body(&hit.document);
                            let score = score_with_query_norm(&metric, &query, norm, &v);
                            assert_eq!(
                                hit.score.to_bits(),
                                score.to_bits(),
                                "reader {r} query {n}"
                            );
                        }
                        assert!(res.hits.windows(2).all(|w| w[0].score >= w[1].score));
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        let writer = s.spawn(|| {
            let mut rng = Rng(99);
            let mut i = 0;
            while !stop.load(Ordering::SeqCst) {
                let target = id((i * 37) % count).to_string();
                col.update_one(
                    Filter::Eq("_id".into(), Value::Str(target)),
                    Update::Set(vec![("v".into(), embedding(&rng.vector(dims)))]),
                )
                .unwrap();
                if i % 5 == 0 {
                    col.insert(doc(10_000 + i, "v", &rng.vector(dims))).unwrap();
                }
                i += 1;
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let cycler = s.spawn(|| {
            let mut i = 0;
            while !stop.load(Ordering::SeqCst) {
                match i % 9 {
                    7 => h.db.notify_memory_pressure(MemoryPressure::Moderate),
                    8 => h.db.notify_memory_pressure(MemoryPressure::Normal),
                    n => h.db.set_vector_cache_budget(budgets[n % budgets.len()]),
                }
                i += 1;
                std::thread::sleep(Duration::from_millis(3));
            }
        });
        for reader in readers {
            reader.join().unwrap();
        }
        stop.store(true, Ordering::SeqCst);
        writer.join().unwrap();
        cycler.join().unwrap();
    });
    assert_eq!(queries_done.load(Ordering::Relaxed), 360);
    h.db.notify_memory_pressure(MemoryPressure::Normal);
    h.db.set_vector_cache_budget(budget(300));
    let query = Rng(5).vector(dims);
    let rows = table(&h.db, "docs", "v", dims);
    let expected = oracle(metric, &query, &rows, |_| true, 10);
    assert_eq!(signature(&search(&col, &query, 10, None)), expected);
    assert_eq!(signature(&search(&col, &query, 10, None)), expected);
    let stats = h.db.vector_cache_stats();
    assert_eq!(stats.active_bytes, 0);
    assert_eq!(stats.vector_indexes, 1);
    assert!(stats.peak_bytes <= stats.memory_budget_bytes);
}
