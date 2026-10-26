//! Unindexed full scans, per request: latency, heap allocations, bytes
//! allocated and peak live heap for `find`, `find_one`, `count` and paged or
//! sorted `find_with_options` at 0%, 1%, 10%, 50% and 100% selectivity, plus an
//! unfiltered scan and an indexed control.
//!
//!     cargo run --release -p taladb --example scan_profile -- 10000 [wide|small] [case]
//!
//! A third argument runs only the cases whose name contains it.
//!
//! Two fixtures, neither with an index on a filtered field:
//!
//! * `wide` (default): the stabilization document — integers `n`, `bucket`
//!   (`i % 100`) and `counter` (0), a short `body`, a 1 KiB `payload` and an
//!   8-d vector `v`. Only `n` is indexed, for the control case.
//! * `small`: `{ n, bucket }`, the shape of `query/range_scan_unindexed`.
//!
//! `bucket < k` matches k% of the collection. Ids are fixed, so each case's
//! fingerprint (ids, order and full contents of every returned document except
//! the insert-time `_changed_at`) compares across builds. Prints one JSON
//! object.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use taladb::query::{FindOptions, SortSpec};
use taladb::{Database, Document, Filter, Ulid, Value};

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

enum Query {
    Find(Filter),
    FindOne(Filter),
    Count(Filter),
    Options(Filter, FindOptions),
}

enum Output {
    Docs(Vec<Document>),
    Count(u64),
}

fn fingerprint(out: &Output) -> (usize, String) {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    let rows = match out {
        Output::Docs(docs) => {
            for doc in docs {
                // Every field but the insert-time `_changed_at` stamp.
                let mut doc = doc.clone();
                doc.fields.retain(|(name, _)| name != "_changed_at");
                eat(&postcard::to_allocvec(&doc).unwrap());
            }
            docs.len()
        }
        Output::Count(n) => {
            eat(&n.to_le_bytes());
            *n as usize
        }
    };
    (rows, format!("{h:016x}"))
}

fn document(i: usize, wide: bool) -> Vec<(String, Value)> {
    let mut fields = vec![
        (
            "_id".into(),
            Value::Str(Ulid::from(i as u128 + 1).to_string()),
        ),
        ("n".into(), Value::Int(i as i64)),
        ("bucket".into(), Value::Int((i % 100) as i64)),
    ];
    if wide {
        fields.extend([
            ("counter".into(), Value::Int(0)),
            (
                "body".into(),
                Value::Str(format!("rust database storage token{}", i % 100)),
            ),
            ("payload".into(), Value::Str("x".repeat(1024))),
            (
                "v".into(),
                Value::Array(
                    (0..8)
                        .map(|j| Value::Float(((i + j) % 31) as f64 / 31.0))
                        .collect(),
                ),
            ),
        ]);
    }
    fields
}

fn main() {
    let mut args = std::env::args().skip(1);
    let count: usize = args
        .next()
        .map_or(10_000, |a| a.parse().expect("count must be an integer"));
    let wide = match args.next().as_deref() {
        None | Some("wide") => true,
        Some("small") => false,
        Some(other) => panic!("unknown fixture {other:?}"),
    };
    let only = args.next();
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    for start in (0..count).step_by(1_000) {
        col.insert_many(
            (start..(start + 1_000).min(count))
                .map(|i| document(i, wide))
                .collect(),
        )
        .unwrap();
    }
    col.create_index("n").unwrap();

    let below = |k: i64| Filter::Lt("bucket".into(), Value::Int(k));
    let none = Filter::Eq("bucket".into(), Value::Int(-1));
    let mut cases: Vec<(String, Query)> = vec![
        ("find_none".into(), Query::Find(none.clone())),
        ("count_none".into(), Query::Count(none)),
    ];
    for k in [0, 1, 10, 50, 100] {
        cases.push((format!("find_{k}pct"), Query::Find(below(k))));
        cases.push((format!("count_{k}pct"), Query::Count(below(k))));
    }
    cases.extend([
        ("find_all".into(), Query::Find(Filter::All)),
        (
            "find_one_last".into(),
            Query::FindOne(Filter::Eq("bucket".into(), Value::Int(99))),
        ),
        (
            "page_1pct_limit10".into(),
            Query::Options(
                Filter::Eq("bucket".into(), Value::Int(42)),
                FindOptions {
                    skip: 5,
                    limit: Some(10),
                    ..Default::default()
                },
            ),
        ),
        (
            "sorted_10pct_limit10".into(),
            Query::Options(
                below(10),
                FindOptions {
                    sort: vec![SortSpec::desc("n")],
                    skip: 5,
                    limit: Some(10),
                    ..Default::default()
                },
            ),
        ),
        (
            "indexed_eq".into(),
            Query::Find(Filter::Eq("n".into(), Value::Int(42))),
        ),
    ]);
    let repeats = match count {
        0..=2_000 => 200,
        2_001..=20_000 => 50,
        _ => 10,
    };

    let mut out = Vec::new();
    for (name, query) in &cases {
        if only.as_deref().is_some_and(|o| !name.contains(o)) {
            continue;
        }
        let run = || match query {
            Query::Find(f) => Output::Docs(col.find(f.clone()).unwrap()),
            Query::FindOne(f) => {
                Output::Docs(col.find_one(f.clone()).unwrap().into_iter().collect())
            }
            Query::Count(f) => Output::Count(col.count(f.clone()).unwrap()),
            Query::Options(f, o) => {
                Output::Docs(col.find_with_options(f.clone(), o.clone()).unwrap())
            }
        };
        for _ in 0..3 {
            drop(run());
        }
        let (allocs, bytes) = (
            ALLOCATIONS.load(Ordering::Relaxed),
            ALLOCATED_BYTES.load(Ordering::Relaxed),
        );
        let live = LIVE.load(Ordering::Relaxed);
        PEAK.store(live, Ordering::Relaxed);
        let result = run();
        let peak = PEAK.load(Ordering::Relaxed) - live;
        let allocs = ALLOCATIONS.load(Ordering::Relaxed) - allocs;
        let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes;
        let (rows, fingerprint) = fingerprint(&result);
        drop(result);
        let mut times: Vec<f64> = (0..repeats)
            .map(|_| {
                let t = Instant::now();
                drop(run());
                t.elapsed().as_nanos() as f64
            })
            .collect();
        times.sort_by(f64::total_cmp);
        out.push(serde_json::json!({
            "case": name,
            "p50_us": times[times.len() / 2] / 1e3,
            "min_us": times[0] / 1e3,
            "allocations": allocs,
            "bytes_allocated": bytes,
            "peak_live_bytes": peak,
            "rows": rows,
            "fingerprint": fingerprint,
        }));
    }
    println!(
        "{}",
        serde_json::json!({
            "count": count,
            "fixture": if wide { "wide" } else { "small" },
            "repeats": repeats,
            "cases": out,
        })
    );
}
