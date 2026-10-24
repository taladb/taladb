//! HNSW build and search profile.
//!
//! Criterion is the wrong shape for this: a single 10k-vector graph build takes
//! tens of seconds, so a sampling harness that wants many iterations cannot run
//! it at all. This measures one build and a fixed query sweep, prints the
//! numbers, and exits — which is what an optimisation on this path needs to be
//! judged against.
//!
//!     cargo run --release -p taladb --example hnsw_profile [count]
//!     cargo run --release -p taladb --example hnsw_profile -- 10000 0.6 --json --quantization binary --queries 100 --seed 1
//!
//! Optional controls: --dims, --queries, --seed, --m, --ef-construction and
//! --quantization (none/scalar/binary), --cache-bytes (default 8 MiB), and
//! --batch-size (1..=1024) for staged
//! builds. JSON mode uses queries independent of
//! the collection size, so recall can be compared across larger collections.
//!
//! Storage point reads (`get` keys) are counted per query through a wrapping
//! backend, as a node-cache miss proxy: a change that moves ANN latency at one
//! cache budget but not the other usually moves this count too.

use std::ops::Bound;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use taladb::engine::{KvPairs, ReadTxn, ScanFn, WriteTxn};
use taladb::{
    Database, GraphOptions, Quantization, RedbBackend, StorageBackend, TalaDbError, Value,
    VectorMetric, VectorQueryOptions, VectorSearchMode,
};

/// `RedbBackend::open_in_memory` — what `Database::open_in_memory` opens — with
/// read transactions that count point reads. Every other call is forwarded,
/// including the streaming and batched overrides, so the engine takes the same
/// paths it takes unwrapped.
struct CountingBackend {
    inner: RedbBackend,
    reads: Arc<AtomicU64>,
}

impl StorageBackend for CountingBackend {
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        self.inner.begin_write()
    }
    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        Ok(Box::new(CountingRead {
            inner: self.inner.begin_read()?,
            reads: &self.reads,
        }))
    }
    fn compact(&self) -> Result<(), TalaDbError> {
        self.inner.compact()
    }
    fn set_durability(&self, eventual: bool) {
        self.inner.set_durability(eventual);
    }
    fn flush(&self) -> Result<(), TalaDbError> {
        self.inner.flush()
    }
}

struct CountingRead<'a> {
    inner: Box<dyn ReadTxn + 'a>,
    reads: &'a AtomicU64,
}

impl ReadTxn for CountingRead<'_> {
    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.get(table, key)
    }
    fn get_many(&self, table: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        self.reads.fetch_add(keys.len() as u64, Ordering::Relaxed);
        self.inner.get_many(table, keys)
    }
    fn range(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, TalaDbError> {
        self.inner.range(table, start, end)
    }
    fn scan(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
        f: ScanFn<'_>,
    ) -> Result<(), TalaDbError> {
        self.inner.scan(table, start, end, f)
    }
    fn scan_all(&self, table: &str) -> Result<KvPairs, TalaDbError> {
        self.inner.scan_all(table)
    }
    fn list_tables(&self) -> Result<Vec<String>, TalaDbError> {
        self.inner.list_tables()
    }
    fn count_entries(&self, table: &str) -> Result<u64, TalaDbError> {
        self.inner.count_entries(table)
    }
}

const DIMS: usize = 384;
const CLUSTERS: usize = 512;
const TOP_K: usize = 10;
const QUERIES: usize = 20;

/// Deterministic, so two runs of this example are comparable to each other.
struct Rng(u64);
impl Rng {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / 16_777_216.0
    }
    /// Box–Muller. Gaussian components give a well-behaved direction.
    fn gaussian(&mut self) -> f32 {
        let u = self.next_f32().max(1e-9);
        (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * self.next_f32()).cos()
    }
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v
        .iter()
        .map(|x| x * x)
        .sum::<f32>()
        .sqrt()
        .max(f32::MIN_POSITIVE);
    for x in &mut v {
        *x /= norm;
    }
    v
}

/// Clustered rather than uniform: in 384 dimensions uniform points are all
/// nearly equidistant, so a graph has no structure to exploit and the measured
/// recall says more about the generator than about the index.
fn make_point(rng: &mut Rng, centroid: &[f32], spread: f32) -> Vec<f32> {
    normalize(
        centroid
            .iter()
            .map(|c| c + rng.gaussian() * spread)
            .collect(),
    )
}

fn to_value(v: &[f32]) -> Value {
    Value::Array(v.iter().map(|f| Value::Float(f64::from(*f))).collect())
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let option = |name: &str, default: u32| -> u32 {
        args.iter().position(|a| a == name).map_or(default, |i| {
            args.get(i + 1)
                .expect("missing parameter value")
                .parse()
                .expect("invalid parameter value")
        })
    };
    let dimensions = option("--dims", DIMS as u32) as usize;
    let queries = option("--queries", QUERIES as u32) as usize;
    let seed = option("--seed", 0);
    let cache_bytes = option("--cache-bytes", 8 * 1024 * 1024) as usize;
    assert!(
        dimensions > 0 && queries > 0,
        "dimensions and queries must be positive"
    );
    let quantization =
        args.iter()
            .position(|a| a == "--quantization")
            .map_or(Quantization::None, |i| {
                match args.get(i + 1).map(String::as_str) {
                    Some("none") => Quantization::None,
                    Some("scalar") => Quantization::Scalar,
                    Some("binary") => Quantization::Binary,
                    _ => panic!("quantization must be none, scalar or binary"),
                }
            });
    let m = option("--m", 16);
    let ef_construction = option("--ef-construction", 200);
    let batch_size = args
        .iter()
        .any(|a| a == "--batch-size")
        .then(|| option("--batch-size", 128) as usize);
    if let Some(size) = batch_size {
        assert!(
            (1..=1024).contains(&size),
            "batch size must be between 1 and 1024"
        );
    }
    let json_output = std::env::args().any(|a| a == "--json");
    let count: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(10_000);
    assert!(count >= TOP_K, "count must be at least {TOP_K}");
    // Cluster spread. Large values wash the clusters out and the set approaches
    // uniform-on-sphere, which is the pathological case for any proximity graph
    // in high dimensions — worth being able to vary before blaming the index.
    let spread: f32 = std::env::args()
        .nth(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(0.6);
    if !json_output {
        println!("count {count}, spread {spread}, dims {dimensions}, clusters {CLUSTERS}");
    }

    let mut rng = Rng(0x2545_F491_4F6C_DD1D ^ u64::from(seed));
    let centroids: Vec<Vec<f32>> = (0..CLUSTERS)
        .map(|_| normalize((0..dimensions).map(|_| rng.gaussian()).collect()))
        .collect();

    let points: Vec<Vec<f32>> = (0..count)
        .map(|i| make_point(&mut rng, &centroids[i % CLUSTERS], spread))
        .collect();
    let mut probe_rng = Rng(0x2AB7_7CA8_8102_4991 ^ u64::from(seed));
    let rng = if json_output {
        &mut probe_rng
    } else {
        &mut rng
    };
    let probes: Vec<Vec<f32>> = (0..queries)
        .map(|q| make_point(rng, &centroids[(q * 7) % CLUSTERS], spread))
        .collect();

    let reads = Arc::new(AtomicU64::new(0));
    let db = Database::open_with_backend(Box::new(CountingBackend {
        inner: RedbBackend::open_in_memory().unwrap(),
        reads: reads.clone(),
    }))
    .unwrap();
    db.set_vector_cache_budget(cache_bytes);
    let col = db.collection("docs").unwrap();

    let t = Instant::now();
    col.insert_many(
        points
            .iter()
            .map(|v| vec![("embedding".to_string(), to_value(v))])
            .collect(),
    )
    .unwrap();
    let insert_ms = t.elapsed().as_secs_f64() * 1000.0;
    if !json_output {
        println!("insert {count}      {:>8.2?}", t.elapsed());
    }

    let t = Instant::now();
    let options = GraphOptions {
        m,
        ef_construction,
        quantization,
    };
    let mut step_times = Vec::new();
    if let Some(batch_size) = batch_size {
        col.create_vector_index_with_options(
            "embedding",
            dimensions,
            Some(VectorMetric::Cosine),
            None,
        )
        .unwrap();
        let build = col.begin_vector_build("embedding", Some(options)).unwrap();
        loop {
            let start = Instant::now();
            let progress = col
                .step_vector_build("embedding", &build.id, batch_size)
                .unwrap();
            step_times.push(start.elapsed().as_secs_f64() * 1000.0);
            if progress.state == "ready" {
                break;
            }
            assert_eq!(progress.state, "building");
        }
    } else {
        col.create_vector_index_with_options(
            "embedding",
            dimensions,
            Some(VectorMetric::Cosine),
            Some(options),
        )
        .unwrap();
    }
    let build = t.elapsed();
    if !json_output {
        println!(
            "hnsw build         {build:>8.2?}   ({:.2} ms/vector)",
            build.as_secs_f64() * 1000.0 / count as f64
        );
    }

    // `Document` carries its ULID as a struct field, not as an `_id` entry in
    // `fields` — that mapping happens in the bindings.
    let ids = |r: taladb::VectorQueryResult| -> Vec<String> {
        r.hits
            .into_iter()
            .map(|h| h.document.id.to_string())
            .collect()
    };

    // Exact is ground truth for the recall numbers below.
    let exact_opts = VectorQueryOptions {
        mode: VectorSearchMode::Exact,
        ..Default::default()
    };
    let mut exact_times = Vec::new();
    let reads_before = reads.load(Ordering::Relaxed);
    let truth: Vec<Vec<String>> = probes
        .iter()
        .map(|q| {
            let start = Instant::now();
            let result = col
                .search_vectors("embedding", q, TOP_K, None, &exact_opts)
                .unwrap();
            exact_times.push(start.elapsed().as_secs_f64() * 1000.0);
            ids(result)
        })
        .collect();
    let exact_ms = exact_times.iter().sum::<f64>() / queries as f64;
    let exact_reads = (reads.load(Ordering::Relaxed) - reads_before) as f64 / queries as f64;
    if !json_output {
        println!("\nexact              {exact_ms:>8.3} ms/query");
    }
    let mut measurements = Vec::new();
    // Start ANN without the exact ground-truth block displacing graph nodes.
    db.set_vector_cache_budget(0);
    db.set_vector_cache_budget(cache_bytes);
    for ef in [50usize, 100, 200, 400] {
        let opts = VectorQueryOptions {
            mode: VectorSearchMode::Ann,
            ef_search: Some(ef),
            ..Default::default()
        };
        if json_output {
            for q in &probes {
                col.search_vectors("embedding", q, TOP_K, None, &opts)
                    .unwrap();
            }
        }
        let t = Instant::now();
        let reads_before = reads.load(Ordering::Relaxed);
        let mut query_times = Vec::new();
        let mut recall = 0.0;
        let mut distances = 0usize;
        for (i, q) in probes.iter().enumerate() {
            let start = Instant::now();
            let result = col
                .search_vectors("embedding", q, TOP_K, None, &opts)
                .unwrap();
            query_times.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(result.execution.path, "hnsw");
            distances += result.execution.distance_computations;
            let found = ids(result)
                .iter()
                .filter(|id| truth[i].contains(id))
                .count();
            recall += found as f64 / TOP_K as f64;
        }
        let elapsed_ms = t.elapsed().as_secs_f64() * 1000.0 / queries as f64;
        let storage_reads = (reads.load(Ordering::Relaxed) - reads_before) as f64 / queries as f64;
        if !json_output {
            println!(
                "ann ef={ef:<4}        {:>8.3} ms/query   recall@{TOP_K} {:>5.1}%   {:>6} distances",
                t.elapsed().as_secs_f64() * 1000.0 / queries as f64,
                recall / queries as f64 * 100.0,
                distances / queries
            );
        }
        query_times.sort_by(f64::total_cmp);
        measurements.push(serde_json::json!({ "ef_search": ef, "mean_ms": elapsed_ms,
            "p50_ms": query_times[queries / 2], "p95_ms": query_times[queries * 95 / 100],
            "recall_at_10": recall / queries as f64, "distances": distances / queries,
            "storage_reads": storage_reads,
            "retained_cache_bytes": db.vector_cache_stats().retained_bytes }));
    }
    if json_output {
        step_times.sort_by(f64::total_cmp);
        let step_p95_ms = (!step_times.is_empty()).then(|| step_times[step_times.len() * 95 / 100]);
        let peak_rss_bytes = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("VmHWM:")
                            .and_then(|rest| rest.split_whitespace().next())
                            .and_then(|n| n.parse::<u64>().ok())
                    })
                    .map(|kb| kb * 1024)
            });
        println!(
            "{}",
            serde_json::json!({ "schema": 1, "count": count, "dimensions": dimensions,
            "spread": spread, "m": m, "ef_construction": ef_construction, "queries": queries, "seed": seed, "quantization": quantization,
            "cache_bytes": cache_bytes,
            "batch_size": batch_size, "build_steps": step_times.len(), "step_p95_ms": step_p95_ms,
            "insert_ms": insert_ms, "build_ms": build.as_secs_f64() * 1000.0,
            "exact_mean_ms": exact_ms, "exact_storage_reads": exact_reads, "ann": measurements, "peak_rss_bytes": peak_rss_bytes })
        );
    }
}
