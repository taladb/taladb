//! Concurrent core searches with the same 1 MiB / 8 MiB budgets used on phones.
//! cargo run --release -p taladb --example vector_memory_profile -- --cache-bytes 1048576
//! Peak reservation is conservative accounting, not peak RSS or an allocator trace.
//!
//! `--steady N` (CONC-2) warms every thread's search path first and then times N
//! requests per thread with per-thread CPU time, so serialisation (cores busy ≈ 1,
//! throughput flat) can be told apart from extra work per request (cores busy ≈
//! threads, CPU per request rising). Without it, each case starts from a reset
//! cache with 20-ish requests per thread, which mostly measures cache warming.
//! `--mode exact` runs the same loop with exact search.
//!
//! `--retain` runs one unfiltered exact search after each case's cache reset,
//! which retains the decoded vector block when it fits, so ANN rescoring can
//! use it. `--unfiltered` drops the tenant filter from every search.
use std::sync::{Arc, Barrier};
use std::time::Instant;
use taladb::{
    Database, Filter, GraphOptions, Quantization, Value, VectorQueryOptions, VectorSearchMode,
};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let option = |name: &str, default: usize| {
        args.iter()
            .position(|arg| arg == name)
            .map_or(default, |i| {
                args[i + 1].parse::<usize>().expect("invalid integer")
            })
    };
    let budget = option("--cache-bytes", 8 * 1024 * 1024);
    let count = option("--count", 2000);
    let dimensions = option("--dims", 128);
    let queries = option("--queries", 20);
    let steady = args
        .iter()
        .any(|a| a == "--steady")
        .then(|| option("--steady", 400));
    let mode = match args
        .iter()
        .position(|a| a == "--mode")
        .map(|i| args[i + 1].as_str())
    {
        None | Some("ann") => VectorSearchMode::Ann,
        Some("exact") => VectorSearchMode::Exact,
        Some(other) => panic!("mode must be ann or exact, not {other}"),
    };
    let retain = args.iter().any(|a| a == "--retain");
    let unfiltered = args.iter().any(|a| a == "--unfiltered");
    assert!(count >= 100 && dimensions > 0 && queries > 0);
    assert!(steady != Some(0), "--steady needs a positive request count");
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("vectors").unwrap();
    let point = |i: usize| {
        (0..dimensions)
            .map(|j| {
                (((i % 32) * 17 + j * 13) as f32).sin() + ((i * 37 + j * 7) as f32).cos() * 0.2
            })
            .collect::<Vec<_>>()
    };
    col.insert_many(
        (0..count)
            .map(|i| {
                vec![
                    (
                        "v".into(),
                        Value::Array(
                            point(i)
                                .into_iter()
                                .map(|v| Value::Float(f64::from(v)))
                                .collect(),
                        ),
                    ),
                    ("tenant".into(), Value::Int((i % 10) as i64)),
                ]
            })
            .collect(),
    )
    .unwrap();
    col.create_index("tenant").unwrap();
    col.create_vector_index_with_options(
        "v",
        dimensions,
        None,
        Some(GraphOptions {
            m: 8,
            ef_construction: 64,
            quantization: Quantization::Binary,
        }),
    )
    .unwrap();
    let probes = Arc::new(
        (0..queries)
            .map(|i| point(count + i * 7))
            .collect::<Vec<_>>(),
    );
    let filter = (!unfiltered).then(|| Filter::Eq("tenant".into(), Value::Int(0)));
    let exact_options = VectorQueryOptions {
        mode: VectorSearchMode::Exact,
        ..Default::default()
    };
    let truth = Arc::new(
        probes
            .iter()
            .map(|q| {
                col.search_vectors("v", q, 10, filter.clone(), &exact_options)
                    .unwrap()
                    .hits
                    .into_iter()
                    .map(|h| h.document.id)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
    );
    let mut cases = Vec::new();
    for concurrency in [1, 2, 4] {
        db.set_vector_cache_budget(0);
        db.set_vector_cache_budget(budget); // resets peak accounting before this case
        if retain {
            col.search_vectors("v", &probes[0], 1, None, &exact_options)
                .unwrap();
        }
        let block_resident = db.vector_cache_stats().vector_indexes == 1;
        // Steady runs release all workers together after their warm-up; the
        // main thread joins the barrier so the wall clock starts there too.
        let gate = Arc::new(Barrier::new(concurrency + usize::from(steady.is_some())));
        let mut started = Instant::now();
        let workers: Vec<_> = (0..concurrency)
            .map(|_| {
                let col = db.collection("vectors").unwrap();
                let gate = gate.clone();
                let probes = probes.clone();
                let truth = truth.clone();
                let filter = filter.clone();
                std::thread::spawn(move || {
                    let options = VectorQueryOptions {
                        mode,
                        ef_search: Some(100),
                        ..Default::default()
                    };
                    let mut times = Vec::new();
                    let mut fallbacks = 0;
                    let mut recall = 0.0;
                    let requests: Vec<usize> = match steady {
                        Some(n) => {
                            for query in probes.iter() {
                                col.search_vectors("v", query, 10, filter.clone(), &options)
                                    .unwrap();
                            }
                            gate.wait();
                            (0..n).map(|i| i % probes.len()).collect()
                        }
                        None => (0..probes.len()).collect(),
                    };
                    let cpu_before = thread_cpu_ns();
                    for i in requests {
                        let query = &probes[i];
                        if steady.is_none() {
                            gate.wait(); // force overlapping starts without serializing work
                        }
                        let start = Instant::now();
                        let result = col
                            .search_vectors("v", query, 10, filter.clone(), &options)
                            .unwrap();
                        times.push(start.elapsed().as_secs_f64() * 1000.0);
                        let ids: Vec<_> = result.hits.iter().map(|h| h.document.id).collect();
                        let found = ids.iter().filter(|id| truth[i].contains(id)).count();
                        recall += found as f64 / truth[i].len() as f64;
                        if mode == VectorSearchMode::Exact {
                            assert_eq!(ids, truth[i], "exact search must return the exact ranking");
                        } else if result.execution.reason == "memoryBudget" {
                            fallbacks += 1;
                            assert_eq!(ids, truth[i], "fallback must return the exact ranking");
                        } else {
                            assert_eq!(result.execution.path, "hnsw");
                        }
                    }
                    let cpu_ns = cpu_before.zip(thread_cpu_ns()).map(|(a, b)| b - a);
                    (times, fallbacks, recall, cpu_ns)
                })
            })
            .collect();
        if steady.is_some() {
            gate.wait();
            started = Instant::now();
        }
        let mut times = Vec::new();
        let mut fallbacks = 0;
        let mut recall = 0.0;
        let mut cpu_ns = Some(0u64);
        for worker in workers {
            let (t, f, r, c) = worker.join().unwrap();
            times.extend(t);
            fallbacks += f;
            recall += r;
            cpu_ns = cpu_ns.zip(c).map(|(a, b)| a + b);
        }
        let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
        times.sort_by(f64::total_cmp);
        let stats = db.vector_cache_stats();
        assert_eq!(stats.active_bytes, 0, "all memory loans must be released");
        assert!(
            stats.peak_bytes <= stats.memory_budget_bytes,
            "accounted search allowance exceeded"
        );
        assert!(stats.retained_bytes <= budget);
        let cpu_ms = cpu_ns.map(|ns| ns as f64 / 1e6);
        cases.push(
            serde_json::json!({ "concurrency": concurrency, "requests": times.len(),
            "p50_ms": times[times.len()/2], "p95_ms": times[times.len()*95/100],
            "elapsed_ms": wall_ms, "memory_fallbacks": fallbacks,
            "recall_at_10": recall / times.len() as f64, "cache": stats,
            "block_resident_before": block_resident, "block_resident_after": stats.vector_indexes == 1,
            "throughput_per_s": times.len() as f64 / wall_ms * 1000.0,
            "cpu_ms_per_request": cpu_ms.map(|c| c / times.len() as f64),
            "cpu_cores_busy": cpu_ms.map(|c| c / wall_ms) }),
        );
    }
    println!(
        "{}",
        serde_json::json!({ "schema": 1, "workload": "concurrent-vector-memory", "count": count,
        "dimensions": dimensions, "queries": queries, "cache_bytes": budget,
        "mode": mode, "steady_requests_per_thread": steady, "retain": retain,
        "unfiltered": unfiltered, "cases": cases })
    );
}

/// This thread's CPU time in nanoseconds, from `/proc/thread-self/schedstat`
/// (first field). `None` where that file does not exist (non-Linux): the CPU
/// columns are then reported as null rather than measured some other way.
fn thread_cpu_ns() -> Option<u64> {
    std::fs::read_to_string("/proc/thread-self/schedstat")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}
