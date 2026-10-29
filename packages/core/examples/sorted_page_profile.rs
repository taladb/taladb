//! Sorted pages served from a secondary index, per request: latency, index keys
//! walked, documents decoded, heap allocations, bytes allocated and peak live
//! heap, for ascending, descending and multi-key sorts, small and deep pages,
//! one collection-wide run of ties, a field the index cannot order (mixed
//! Int/Float) and a `$sort` pipeline.
//!
//!     cargo run --release -p taladb --example sorted_page_profile -- 100000
//!
//! Every field sorted on is indexed:
//!
//! * `n`: unique integers in shuffled insertion order;
//! * `g`: runs of 100 equal integers;
//! * `t`: one value shared by every document;
//! * `x`: integers except one float, so the index cannot be used for order;
//! * `m`: a secondary sort key;
//! * a 512-byte payload.
//!
//! Ids are fixed, so the fingerprint of each case (ids and order of the page)
//! compares across builds. Prints one JSON object.
use std::alloc::{GlobalAlloc, Layout, System};
use std::ops::Bound;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use taladb::aggregate::Stage;
use taladb::engine::{KvPairs, ReadTxn, ScanFn, WriteTxn};
use taladb::query::{FindOptions, SortSpec};
use taladb::{Database, Document, Filter, RedbBackend, StorageBackend, TalaDbError, Ulid, Value};

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

/// Index keys handed out and documents read, through any read API.
#[derive(Default)]
struct Reads {
    index_keys: AtomicU64,
    docs: AtomicU64,
}
impl Reads {
    fn note(&self, table: &str, n: usize) {
        if table.starts_with("idx::") {
            self.index_keys.fetch_add(n as u64, Ordering::Relaxed);
        } else if table.starts_with("docs::") {
            self.docs.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    fn take(&self) -> (u64, u64) {
        (
            self.index_keys.swap(0, Ordering::Relaxed),
            self.docs.swap(0, Ordering::Relaxed),
        )
    }
}
struct Backend {
    inner: RedbBackend,
    reads: Arc<Reads>,
}
struct Read<'a> {
    inner: Box<dyn ReadTxn + 'a>,
    reads: Arc<Reads>,
}
impl StorageBackend for Backend {
    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        Ok(Box::new(Read {
            inner: self.inner.begin_read()?,
            reads: self.reads.clone(),
        }))
    }
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        self.inner.begin_write()
    }
}
impl ReadTxn for Read<'_> {
    fn get(&self, t: &str, k: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        let v = self.inner.get(t, k)?;
        self.reads.note(t, usize::from(v.is_some()));
        Ok(v)
    }
    fn get_many(&self, t: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        let v = self.inner.get_many(t, keys)?;
        self.reads.note(t, v.iter().filter(|x| x.is_some()).count());
        Ok(v)
    }
    fn range(&self, t: &str, s: Bound<&[u8]>, e: Bound<&[u8]>) -> Result<KvPairs, TalaDbError> {
        let v = self.inner.range(t, s, e)?;
        self.reads.note(t, v.len());
        Ok(v)
    }
    fn scan_all(&self, t: &str) -> Result<KvPairs, TalaDbError> {
        let v = self.inner.scan_all(t)?;
        self.reads.note(t, v.len());
        Ok(v)
    }
    fn list_tables(&self) -> Result<Vec<String>, TalaDbError> {
        self.inner.list_tables()
    }
    fn count_entries(&self, t: &str) -> Result<u64, TalaDbError> {
        self.inner.count_entries(t)
    }
    fn scan(
        &self,
        t: &str,
        s: Bound<&[u8]>,
        e: Bound<&[u8]>,
        f: ScanFn<'_>,
    ) -> Result<(), TalaDbError> {
        let reads = &self.reads;
        self.inner.scan(t, s, e, &mut |k, v| {
            reads.note(t, 1);
            f(k, v)
        })
    }
}

enum Query {
    Find(Vec<SortSpec>, u64, u64),
    Pipeline(Vec<Stage>),
}

fn fingerprint(docs: &[Document]) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in docs.iter().flat_map(|d| d.id.to_bytes()) {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn main() {
    let count: usize = std::env::args()
        .nth(1)
        .map_or(10_000, |a| a.parse().expect("count must be an integer"));
    let reads = Arc::new(Reads::default());
    let db = Database::open_with_backend(Box::new(Backend {
        inner: RedbBackend::open_in_memory().unwrap(),
        reads: reads.clone(),
    }))
    .unwrap();
    let col = db.collection("docs").unwrap();
    for field in ["n", "g", "t", "x"] {
        col.create_index(field).unwrap();
    }
    for start in (0..count).step_by(1_000) {
        col.insert_many(
            (start..(start + 1_000).min(count))
                .map(|i| {
                    vec![
                        (
                            "_id".into(),
                            Value::Str(Ulid::from(i as u128 + 1).to_string()),
                        ),
                        ("n".into(), Value::Int((i * 7_919 % count) as i64)),
                        ("g".into(), Value::Int((i / 100) as i64)),
                        ("t".into(), Value::Int(7)),
                        (
                            "x".into(),
                            if i == count / 2 {
                                Value::Float(0.5)
                            } else {
                                Value::Int(i as i64)
                            },
                        ),
                        ("m".into(), Value::Int((i % 13) as i64)),
                        ("payload".into(), Value::Str("p".repeat(512))),
                    ]
                })
                .collect(),
        )
        .unwrap();
    }

    let half = (count / 2) as u64;
    let asc = SortSpec::asc;
    let desc = SortSpec::desc;
    let cases: Vec<(&str, Query)> = vec![
        ("asc_single", Query::Find(vec![asc("n")], 20, 10)),
        ("desc_single", Query::Find(vec![desc("n")], 20, 10)),
        ("asc_single_deep", Query::Find(vec![asc("n")], half, 10)),
        ("desc_single_deep", Query::Find(vec![desc("n")], half, 10)),
        ("asc_single_first", Query::Find(vec![asc("n")], 0, 1)),
        ("asc_multi", Query::Find(vec![asc("g"), desc("m")], 20, 10)),
        ("desc_multi", Query::Find(vec![desc("g"), asc("m")], 20, 10)),
        (
            "asc_multi_deep",
            Query::Find(vec![asc("g"), desc("m")], half, 10),
        ),
        (
            "desc_multi_deep",
            Query::Find(vec![desc("g"), asc("m")], half, 10),
        ),
        ("asc_one_run", Query::Find(vec![asc("t")], 20, 10)),
        ("desc_one_run_deep", Query::Find(vec![desc("t")], half, 10)),
        (
            "multi_one_run",
            Query::Find(vec![asc("t"), asc("m")], 20, 10),
        ),
        ("fallback_mixed", Query::Find(vec![asc("x")], 20, 10)),
        (
            "pipeline_desc",
            Query::Pipeline(vec![
                Stage::Sort(vec![desc("n")]),
                Stage::Skip(20),
                Stage::Limit(10),
            ]),
        ),
    ];
    let repeats = match count {
        0..=2_000 => 200,
        2_001..=20_000 => 50,
        _ => 10,
    };

    let mut out = Vec::new();
    for (name, query) in &cases {
        let run = || match query {
            Query::Find(sort, skip, limit) => col
                .find_with_options(
                    Filter::All,
                    FindOptions {
                        sort: sort.clone(),
                        skip: *skip,
                        limit: Some(*limit),
                        ..Default::default()
                    },
                )
                .unwrap(),
            Query::Pipeline(p) => col.aggregate(p.clone()).unwrap(),
        };
        for _ in 0..3 {
            drop(run());
        }
        reads.take();
        let (allocs, bytes) = (
            ALLOCATIONS.load(Ordering::Relaxed),
            ALLOCATED_BYTES.load(Ordering::Relaxed),
        );
        let live = LIVE.load(Ordering::Relaxed);
        PEAK.store(live, Ordering::Relaxed);
        let page = run();
        let peak = PEAK.load(Ordering::Relaxed) - live;
        let allocs = ALLOCATIONS.load(Ordering::Relaxed) - allocs;
        let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes;
        let (index_keys, docs) = reads.take();
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
            "index_keys": index_keys,
            "docs_decoded": docs,
            "allocations": allocs,
            "bytes_allocated": bytes,
            "peak_live_bytes": peak,
            "rows": page.len(),
            "fingerprint": fingerprint(&page),
        }));
    }
    println!(
        "{}",
        serde_json::json!({ "count": count, "repeats": repeats, "cases": out })
    );
}
