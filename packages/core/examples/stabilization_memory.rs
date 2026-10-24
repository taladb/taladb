//! Standalone workload for `/usr/bin/time -v` and Valgrind memory profiles.
//! Run: cargo run -p taladb --release --example stabilization_memory -- 10000 update
//!
//! Each checkpoint also prints the cumulative heap allocation count and bytes
//! (MEM-2). For a fixed fixture those are deterministic, so unlike RSS they can
//! be gated tightly (`scripts/bench-tier0.py allocations`).
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use taladb::{Database, Filter, Update, Value};

struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards every call to `System` unchanged; the counters are
// side effects only.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let count: usize = args.next().unwrap_or_else(|| "10000".into()).parse()?;
    let operation = args.next().unwrap_or_else(|| "update".into());
    let export = args.next().as_deref() != Some("no-export");
    let db = Database::open_in_memory()?;
    db.set_vector_cache_budget(64 * 1024);
    let col = db.collection("docs")?;
    for start in (0..count).step_by(128) {
        col.insert_many(
            (start..(start + 128).min(count))
                .map(|i| {
                    vec![
                        ("n".into(), Value::Int(i as i64)),
                        ("bucket".into(), Value::Int((i % 100) as i64)),
                        ("body".into(), Value::Str("rust database storage".into())),
                        ("payload".into(), Value::Str("x".repeat(8192))),
                    ]
                })
                .collect(),
        )?;
    }
    checkpoint("loaded");
    let started = std::time::Instant::now();
    match operation.as_str() {
        "index" => {
            col.create_index("n")?;
            col.create_compound_index(&["bucket", "n"])?;
            col.create_fts_index("body")?;
            col.drop_index("n")?;
            col.drop_compound_index(&["bucket", "n"])?;
            col.drop_fts_index("body")?;
        }
        "update" | "delete" => {
            col.create_index("n")?;
            col.create_fts_index("body")?;
            checkpoint("indexed");
            if operation == "update" {
                assert_eq!(
                    col.update_many(Filter::All, Update::Inc(vec![("n".into(), Value::Int(1))]))?,
                    count as u64
                );
            } else {
                assert_eq!(col.delete_many(Filter::All)?, count as u64);
            }
        }
        other => return Err(format!("unknown workload: {other}").into()),
    }
    checkpoint("mutated");
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    let snapshot_bytes = if export {
        Some(db.export_snapshot()?.len())
    } else {
        None
    };
    checkpoint("exported");
    println!(
        "operation={operation} documents={count} elapsed_ms={elapsed:.3} snapshot_bytes={snapshot_bytes:?}"
    );
    drop(col);
    drop(db);
    checkpoint("dropped");
    Ok(())
}

fn checkpoint(phase: &str) {
    println!(
        "phase={phase} allocations={} allocated_bytes={}",
        ALLOCATIONS.load(Ordering::Relaxed),
        ALLOCATED_BYTES.load(Ordering::Relaxed)
    );
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for field in ["VmRSS:", "VmHWM:"] {
            if let Some(line) = status.lines().find(|line| line.starts_with(field)) {
                println!("phase={phase} {line}");
            }
        }
    }
}
