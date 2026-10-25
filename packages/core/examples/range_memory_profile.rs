//! Heap cost of a two-sided indexed range (DOC-11): allocations, allocated
//! bytes and peak live heap while the query runs, for eight windows of 100
//! documents, next to a one-sided range returning the same number of documents.
//! RSS cannot show this: the loaded fixture dominates the high-water mark.
//!
//!     cargo run --release -p taladb --example range_memory_profile -- 100000
//!
//! The fixture is the Criterion `doc/` one in miniature: fixed IDs, an indexed
//! integer `n`, a 1 KiB payload. Prints one JSON object.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use taladb::{Database, Filter, Ulid, Value};

struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);

fn grew(bytes: u64) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

// SAFETY: forwards every call to `System` unchanged; the counters are side
// effects only.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        grew(layout.size() as u64);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        grew(layout.size() as u64);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        LIVE.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        grew(new_size as u64);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Run `query` and report its heap cost; the peak is relative to the live heap
/// at the start, so it is what the query itself held at once.
fn measure(mut query: impl FnMut() -> usize) -> serde_json::Value {
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    let started = Instant::now();
    let documents = query();
    let elapsed = started.elapsed();
    serde_json::json!({
        "documents": documents,
        "allocations": ALLOCATIONS.load(Ordering::Relaxed) - allocations,
        "allocated_bytes": ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes,
        "peak_live_bytes": PEAK.load(Ordering::Relaxed) - live,
        "elapsed_ms": elapsed.as_secs_f64() * 1000.0,
    })
}

fn main() {
    let count: usize = std::env::args()
        .nth(1)
        .map_or(10_000, |a| a.parse().expect("count must be an integer"));
    assert!(count > 1_000, "count must exceed the window offsets");
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    for start in (0..count).step_by(1_000) {
        col.insert_many(
            (start..(start + 1_000).min(count))
                .map(|i| {
                    vec![
                        (
                            "_id".into(),
                            Value::Str(Ulid::from(i as u128 + 1).to_string()),
                        ),
                        ("n".into(), Value::Int(i as i64)),
                        ("payload".into(), Value::Str("x".repeat(1024))),
                    ]
                })
                .collect(),
        )
        .unwrap();
    }
    col.create_index("n").unwrap();
    // The same offsets as `doc/range_window` in the Criterion suite.
    let windows: Vec<i64> = (0..8)
        .map(|k| ((k * 7919) % (count - 100)) as i64)
        .collect();
    let window = |x: i64| {
        Filter::And(vec![
            Filter::Gte("n".into(), Value::Int(x)),
            Filter::Lt("n".into(), Value::Int(x + 100)),
        ])
    };
    // Warm the read path once so first-use allocations are not attributed.
    col.find(window(windows[0])).unwrap();
    let two_sided: Vec<_> = windows
        .iter()
        .map(|&x| measure(|| col.find(window(x)).unwrap().len()))
        .collect();
    let one_sided = measure(|| {
        col.find(Filter::Gte("n".into(), Value::Int((count - 100) as i64)))
            .unwrap()
            .len()
    });
    let count_two_sided = measure(|| col.count(window(windows[1])).unwrap() as usize);
    let sum = |key: &str| {
        two_sided
            .iter()
            .map(|w| w[key].as_f64().unwrap())
            .sum::<f64>()
    };
    let max = |key: &str| {
        two_sided
            .iter()
            .map(|w| w[key].as_f64().unwrap())
            .fold(0.0, f64::max)
    };
    println!(
        "{}",
        serde_json::json!({
            "schema": 1, "workload": "range-memory", "count": count, "windows": windows,
            "two_sided": {
                "per_window": two_sided,
                "documents": sum("documents"), "allocations": sum("allocations"),
                "allocated_bytes": sum("allocated_bytes"), "max_peak_live_bytes": max("peak_live_bytes"),
                "elapsed_ms": sum("elapsed_ms"),
            },
            "count_two_sided": count_two_sided,
            "one_sided_100": one_sided,
        })
    );
}
