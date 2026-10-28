//! Exact vector search per request, at a chosen dimension, metric and cache
//! budget: warm and cold latency, whether the decoded block was retained (in
//! full or as a prefix), the bytes the cache accounts for, the heap the
//! retained block really holds, and how many stored vectors each query decodes
//! from storage (streamed by scans or point-read).
//!
//!     cargo run --release -p taladb --example exact_vector_profile -- 10000 384 cosine 67108864
//!
//! Arguments: count, dimensions, metric (`cosine`, `dot`, `euclidean`), cache
//! budget in bytes, then optionally warm queries (default 200), cold queries
//! (default 10) and a filter (`none`, `sparse`: 1% of documents, read by point
//! reads; `dense`: 50%, visited). Filtered searches never build a block, so a
//! filtered run first lets one unfiltered query retain what fits. The fixture
//! and query are those of the Criterion `vector/find_nearest` case. Prints one
//! JSON object. The result fingerprint covers the top-10 IDs and score bits, so
//! builds can be compared.
use std::alloc::{GlobalAlloc, Layout, System};
use std::ops::Bound;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Instant;
use taladb::engine::{KvPairs, ReadTxn, ScanFn, WriteTxn};
use taladb::{
    Database, Filter, RedbBackend, StorageBackend, TalaDbError, Ulid, Value, VectorMetric,
};

struct Counting;

static LIVE: AtomicI64 = AtomicI64::new(0);

// SAFETY: forwards every call to `System` unchanged; the counter is a side
// effect only.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size() as i64, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE.fetch_add(new_size as i64 - layout.size() as i64, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as i64, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Stored vectors decoded from storage: entries of `vec::` tables streamed by
/// scans, and vector point reads.
static STREAMED: AtomicU64 = AtomicU64::new(0);
static POINT_READS: AtomicU64 = AtomicU64::new(0);

struct CountingBackend(RedbBackend);
impl StorageBackend for CountingBackend {
    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        Ok(Box::new(CountingRead(self.0.begin_read()?)))
    }
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        self.0.begin_write()
    }
}
struct CountingRead<'a>(Box<dyn ReadTxn + 'a>);
impl ReadTxn for CountingRead<'_> {
    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        if table.starts_with("vec::") {
            POINT_READS.fetch_add(1, Ordering::Relaxed);
        }
        self.0.get(table, key)
    }
    fn get_many(&self, table: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        if table.starts_with("vec::") {
            POINT_READS.fetch_add(keys.len() as u64, Ordering::Relaxed);
        }
        self.0.get_many(table, keys)
    }
    fn range(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, TalaDbError> {
        self.0.range(table, start, end)
    }
    fn scan(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
        f: ScanFn<'_>,
    ) -> Result<(), TalaDbError> {
        if !table.starts_with("vec::") {
            return self.0.scan(table, start, end, f);
        }
        self.0.scan(table, start, end, &mut |k, v| {
            STREAMED.fetch_add(1, Ordering::Relaxed);
            f(k, v)
        })
    }
    fn scan_all(&self, table: &str) -> Result<KvPairs, TalaDbError> {
        self.0.scan_all(table)
    }
    fn list_tables(&self) -> Result<Vec<String>, TalaDbError> {
        self.0.list_tables()
    }
    fn count_entries(&self, table: &str) -> Result<u64, TalaDbError> {
        self.0.count_entries(table)
    }
}
fn decoded() -> u64 {
    STREAMED.load(Ordering::Relaxed) + POINT_READS.load(Ordering::Relaxed)
}

// The generator of `benches/engine.rs`, so the data matches `vector/find_nearest`.
fn make_vectors(count: usize, dims: usize) -> Vec<Vec<f32>> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 40) as f32 / 16_777_216.0 - 0.5
    };
    (0..count)
        .map(|_| (0..dims).map(|_| next()).collect())
        .collect()
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize, default: usize| {
        args.get(i)
            .map_or(default, |a| a.parse().expect("arguments must be integers"))
    };
    let count = arg(0, 10_000);
    let dims = arg(1, 384);
    let metric = match args.get(2).map_or("cosine", String::as_str) {
        "cosine" => VectorMetric::Cosine,
        "dot" => VectorMetric::Dot,
        "euclidean" => VectorMetric::Euclidean,
        other => panic!("unknown metric {other}"),
    };
    let budget = arg(3, 64 * 1024 * 1024);
    let warm_queries = arg(4, 200);
    let cold_queries = arg(5, 10);
    let filter_name = args.get(6).map_or("none", String::as_str);
    let filter = match filter_name {
        "none" => None,
        "sparse" => Some(Filter::Eq("bucket".into(), Value::Int(1))),
        "dense" => Some(Filter::Lt("bucket".into(), Value::Int(50))),
        other => panic!("unknown filter {other}"),
    };

    let db = Database::open_with_backend(Box::new(CountingBackend(
        RedbBackend::open_in_memory().unwrap(),
    )))
    .unwrap();
    let col = db.collection("docs").unwrap();
    let vectors = make_vectors(count + 1, dims);
    // Fixed IDs, so fingerprints compare across runs and builds.
    for (start, chunk) in (1..).step_by(1_000).zip(vectors[1..].chunks(1_000)) {
        col.insert_many(
            (start..)
                .zip(chunk)
                .map(|(i, v): (u128, _)| {
                    vec![
                        ("_id".to_string(), Value::Str(Ulid::from(i).to_string())),
                        (
                            "embedding".to_string(),
                            Value::Array(v.iter().map(|f| Value::Float(f64::from(*f))).collect()),
                        ),
                        ("bucket".to_string(), Value::Int((i % 100) as i64)),
                    ]
                })
                .collect(),
        )
        .unwrap();
    }
    col.create_vector_index("embedding", dims, Some(metric), None)
        .unwrap();
    let query = &vectors[0];
    let search = || {
        col.find_nearest("embedding", query, 10, filter.clone())
            .unwrap()
    };

    // Cold: no retained block, so the query scans storage and an unfiltered
    // one may build one.
    db.set_vector_cache_budget(budget);
    let mut cold = Vec::with_capacity(cold_queries);
    let mut heap_delta = 0;
    let mut cold_decoded = 0;
    for _ in 0..cold_queries.max(1) {
        db.set_vector_cache_budget(0);
        db.set_vector_cache_budget(budget);
        let live = LIVE.load(Ordering::Relaxed);
        let before = decoded();
        let start = Instant::now();
        let hits = search();
        cold.push(start.elapsed().as_nanos() as f64);
        cold_decoded = decoded() - before;
        drop(hits);
        heap_delta = LIVE.load(Ordering::Relaxed) - live;
    }
    if filter.is_some() {
        col.find_nearest("embedding", query, 10, None).unwrap();
    }
    let stats = db.vector_cache_stats();

    let hits = search();
    let mut fingerprint = 0xcbf2_9ce4_8422_2325u64;
    for hit in &hits {
        for byte in hit
            .document
            .id
            .to_bytes()
            .into_iter()
            .chain(hit.score.to_bits().to_le_bytes())
        {
            fingerprint = (fingerprint ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }

    let mut warm = Vec::with_capacity(warm_queries);
    let (streamed, point_reads) = (
        STREAMED.load(Ordering::Relaxed),
        POINT_READS.load(Ordering::Relaxed),
    );
    for _ in 0..warm_queries {
        let start = Instant::now();
        let hits = search();
        warm.push(start.elapsed().as_nanos() as f64);
        drop(hits);
    }
    let per_query = |total: u64| total as f64 / warm_queries.max(1) as f64;
    let warm_streamed = per_query(STREAMED.load(Ordering::Relaxed) - streamed);
    let warm_point_reads = per_query(POINT_READS.load(Ordering::Relaxed) - point_reads);
    cold.sort_by(f64::total_cmp);
    warm.sort_by(f64::total_cmp);
    let after = db.vector_cache_stats();
    assert_eq!(after.retained_bytes, stats.retained_bytes, "block changed");
    assert_eq!(after.active_bytes, 0);
    assert!(after.peak_bytes <= after.memory_budget_bytes);

    println!(
        "{}",
        serde_json::json!({
            "count": count,
            "dims": dims,
            "metric": format!("{metric:?}"),
            "budget_bytes": budget,
            "filter": filter_name,
            // A prefix is smaller than even a complete block without norms.
            "path": match stats.retained_bytes {
                0 => "streaming",
                n if n < count * (16 + 4 * dims) => "partial",
                _ => "retained",
            },
            "retained_bytes": stats.retained_bytes,
            "retained_bytes_per_vector": stats.retained_bytes as f64 / count as f64,
            "peak_bytes": after.peak_bytes,
            "heap_retained_by_cold_query": heap_delta,
            "warm_p50_ns": percentile(&warm, 0.5),
            "warm_p95_ns": percentile(&warm, 0.95),
            "warm_mean_ns": warm.iter().sum::<f64>() / warm.len().max(1) as f64,
            "cold_p50_ns": percentile(&cold, 0.5),
            "cold_decoded_vectors": cold_decoded,
            "warm_streamed_vectors": warm_streamed,
            "warm_point_read_vectors": warm_point_reads,
            "fingerprint": format!("{fingerprint:016x}"),
            "hits": hits.len(),
        })
    );
}
