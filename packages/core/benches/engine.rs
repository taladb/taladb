//! Engine benchmarks.
//!
//! Performance is a headline claim for this project, and until now the only
//! numbers came from `pnpm bench` through the Node binding — which measures the
//! N-API boundary and JSON conversion alongside the engine, and cannot be run
//! at all while iterating on a Rust-level change. These benchmarks measure the
//! engine directly so an optimisation can be accepted or rejected on evidence.
//!
//! Run with `cargo bench -p taladb`. Add `-- --save-baseline before` and
//! `-- --baseline before` around a change to get a like-for-like comparison.
//! CI compares two checkouts through `scripts/vector-bench-ci.py --workload
//! criterion`, which also runs this binary with `TALADB_BENCH_FINGERPRINTS=<file>`
//! to check that both sides return the same results (see `support/suite.rs`).

#[path = "support/regression.rs"]
mod regression;
#[path = "support/stabilization.rs"]
mod stabilization;
#[path = "support/suite.rs"]
mod suite;

use criterion::{Criterion, Throughput};
use std::hint::black_box;
use suite::{FieldSet, Scores, Suite};
use taladb::vector::{VectorMetric, l2_norm, score_with_query_norm};
use taladb::{Database, Filter, Value};

/// Deterministic pseudo-random vectors: a benchmark that reseeds differently on
/// each run cannot be compared against its own baseline.
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

/// The innermost loop of a flat `find_nearest`: one query against one stored
/// vector, for each metric. This is the function any SIMD work would target, so
/// it gets measured in isolation rather than through the whole search.
fn bench_vector_scoring(s: &mut Suite) {
    if !s.enabled("vector") {
        return;
    }
    let mut group = s.group("vector/score_one");
    for dims in [128usize, 384, 768, 1536] {
        let vectors = make_vectors(2, dims);
        let (query, stored) = (&vectors[0], &vectors[1]);
        let norm = l2_norm(query);

        group.throughput(Throughput::Elements(dims as u64));
        for metric in [
            VectorMetric::Cosine,
            VectorMetric::Dot,
            VectorMetric::Euclidean,
        ] {
            group.run_with(&format!("{metric:?}"), dims, || {
                score_with_query_norm(
                    black_box(&metric),
                    black_box(query),
                    black_box(norm),
                    black_box(stored),
                )
            });
        }
    }
    group.finish();
}

/// End-to-end exact vector search, which is what the README's numbers describe.
/// `vector/find_nearest_8mib` repeats it at the phone and WASM cache budget,
/// where a 10k × 384-d block no longer fits and only a prefix is retained.
fn bench_find_nearest(s: &mut Suite) {
    const DIMS: usize = 384;
    if !s.enabled("vector") {
        return;
    }
    let fixture = |count: usize| {
        let db = Database::open_in_memory().unwrap();
        let col = db.collection("docs").unwrap();
        let mut vectors = make_vectors(count + 1, DIMS);
        let docs: Vec<Vec<(String, Value)>> = vectors[1..]
            .iter()
            .map(|v| {
                vec![(
                    "embedding".to_string(),
                    Value::Array(v.iter().map(|f| Value::Float(f64::from(*f))).collect()),
                )]
            })
            .collect();
        col.insert_many(docs).unwrap();
        col.create_vector_index("embedding", DIMS, None, None)
            .unwrap();
        (db, col, vectors.swap_remove(0))
    };

    let mut group = s.group("vector/find_nearest");
    group.sample_size(20);
    for count in suite::sizes(&[1_000, 10_000]) {
        let (_db, col, query) = fixture(count);
        group.throughput(Throughput::Elements(count as u64));
        group.run_param(count, || {
            Scores(
                col.find_nearest(black_box("embedding"), black_box(&query), 10, None)
                    .unwrap(),
            )
        });
    }
    group.finish();

    let mut group = s.group("vector/find_nearest_8mib");
    group.sample_size(20);
    for count in suite::sizes(&[1_000, 10_000])
        .into_iter()
        .filter(|&n| n >= 10_000)
    {
        let (db, col, query) = fixture(count);
        db.set_vector_cache_budget(8 * 1024 * 1024);
        group.throughput(Throughput::Elements(count as u64));
        group.run_param(count, || {
            Scores(
                col.find_nearest(black_box("embedding"), black_box(&query), 10, None)
                    .unwrap(),
            )
        });
    }
    group.finish();
}

/// Filtered scans — the other half of a hybrid query, and the path that the
/// `Int`/`Float` comparison fix runs through.
fn bench_query(s: &mut Suite) {
    if !s.enabled("query") {
        return;
    }
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    let docs: Vec<Vec<(String, Value)>> = (0..10_000i64)
        .map(|i| {
            vec![
                ("n".to_string(), Value::Int(i)),
                ("bucket".to_string(), Value::Int(i % 100)),
            ]
        })
        .collect();
    col.insert_many(docs).unwrap();

    let mut group = s.group("query");
    group.sample_size(30);
    group.run("range_scan_unindexed", || {
        FieldSet(
            "n",
            col.find(black_box(Filter::Gte("n".into(), Value::Int(5_000))))
                .unwrap(),
        )
    });
    // Mixed Int/Float comparison takes the exact-comparison path rather than a
    // plain integer compare — worth watching that it stays cheap.
    group.run("range_scan_mixed_numeric", || {
        FieldSet(
            "n",
            col.find(black_box(Filter::Gte("n".into(), Value::Float(5_000.5))))
                .unwrap(),
        )
    });
    // Unindexed: `bench_query` creates no index. `doc/indexed_eq` is the
    // indexed counterpart.
    group.run("equality", || {
        FieldSet(
            "n",
            col.find(black_box(Filter::Eq("bucket".into(), Value::Int(42))))
                .unwrap(),
        )
    });
    group.finish();
}

/// Write throughput, and the snapshot export that the WASM persistence path
/// calls on every save.
fn bench_writes_and_snapshot(s: &mut Suite) {
    if !s.enabled("write") {
        return;
    }
    let mut group = s.group("write");
    group.sample_size(20);

    group.throughput(Throughput::Elements(1_000));
    group.run_batched(
        "insert_many_1k",
        None,
        || {
            let db = Database::open_in_memory().unwrap();
            let docs: Vec<Vec<(String, Value)>> = (0..1_000i64)
                .map(|i| {
                    vec![
                        ("n".to_string(), Value::Int(i)),
                        ("name".to_string(), Value::Str(format!("doc-{i}"))),
                    ]
                })
                .collect();
            (db, docs)
        },
        |(db, docs)| db.collection("docs").unwrap().insert_many(docs).unwrap(),
        criterion::BatchSize::SmallInput,
    );

    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.insert_many(
        (0..10_000i64)
            .map(|i| {
                vec![
                    ("n".to_string(), Value::Int(i)),
                    ("name".to_string(), Value::Str(format!("doc-{i}"))),
                ]
            })
            .collect(),
    )
    .unwrap();
    group.run("export_snapshot_10k", || db.export_snapshot().unwrap());
    group.finish();
}

fn run_all(s: &mut Suite) {
    bench_vector_scoring(s);
    bench_find_nearest(s);
    bench_query(s);
    bench_writes_and_snapshot(s);
    stabilization::bench_stabilization(s);
    regression::bench_documents(s);
    regression::bench_text(s);
    regression::bench_bulk(s);
    regression::bench_index_maintenance(s);
}

fn main() {
    if let Some(path) = std::env::var_os("TALADB_BENCH_FINGERPRINTS") {
        let mut suite = Suite::Fingerprint(Vec::new());
        run_all(&mut suite);
        suite.write_fingerprints(&path);
        return;
    }
    let mut criterion = Criterion::default().configure_from_args();
    run_all(&mut Suite::Bench(&mut criterion));
    criterion.final_summary();
}
