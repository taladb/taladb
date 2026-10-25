//! Exact vector search per request, at a chosen dimension, metric and cache
//! budget: warm and cold latency, whether the decoded block was retained, the
//! bytes the cache accounts for, and the heap the retained block really holds.
//!
//!     cargo run --release -p taladb --example exact_vector_profile -- 10000 384 cosine 67108864
//!
//! Arguments: count, dimensions, metric (`cosine`, `dot`, `euclidean`), cache
//! budget in bytes, then optionally warm queries (default 200) and cold queries
//! (default 10). The fixture and query are those of the Criterion
//! `vector/find_nearest` case. Prints one JSON object. The result fingerprint
//! covers the top-10 IDs and score bits, so builds can be compared.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Instant;
use taladb::{Database, Value, VectorMetric};

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

    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    let vectors = make_vectors(count + 1, dims);
    for chunk in vectors[1..].chunks(1_000) {
        col.insert_many(
            chunk
                .iter()
                .map(|v| {
                    vec![(
                        "embedding".to_string(),
                        Value::Array(v.iter().map(|f| Value::Float(f64::from(*f))).collect()),
                    )]
                })
                .collect(),
        )
        .unwrap();
    }
    col.create_vector_index("embedding", dims, Some(metric), None)
        .unwrap();
    let query = &vectors[0];
    let search = || col.find_nearest("embedding", query, 10, None).unwrap();

    // Cold: no retained block, so the query scans storage and may build one.
    db.set_vector_cache_budget(budget);
    let mut cold = Vec::with_capacity(cold_queries);
    let mut heap_delta = 0;
    for _ in 0..cold_queries.max(1) {
        db.set_vector_cache_budget(0);
        db.set_vector_cache_budget(budget);
        let live = LIVE.load(Ordering::Relaxed);
        let start = Instant::now();
        let hits = search();
        cold.push(start.elapsed().as_nanos() as f64);
        drop(hits);
        heap_delta = LIVE.load(Ordering::Relaxed) - live;
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
    for _ in 0..warm_queries {
        let start = Instant::now();
        let hits = search();
        warm.push(start.elapsed().as_nanos() as f64);
        drop(hits);
    }
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
            "path": if stats.retained_bytes > 0 { "retained" } else { "streaming" },
            "retained_bytes": stats.retained_bytes,
            "retained_bytes_per_vector": stats.retained_bytes as f64 / count as f64,
            "peak_bytes": after.peak_bytes,
            "heap_retained_by_cold_query": heap_delta,
            "warm_p50_ns": percentile(&warm, 0.5),
            "warm_p95_ns": percentile(&warm, 0.95),
            "warm_mean_ns": warm.iter().sum::<f64>() / warm.len().max(1) as f64,
            "cold_p50_ns": percentile(&cold, 0.5),
            "fingerprint": format!("{fingerprint:016x}"),
            "hits": hits.len(),
        })
    );
}
