//! Engine benchmarks.
//!
//! Performance is a headline claim for this project, and until now the only
//! numbers came from `pnpm bench` through the Node binding — which measures the
//! N-API boundary and JSON conversion alongside the engine, and cannot be run
//! at all while iterating on a Rust-level change. These benchmarks measure the
//! engine directly so an optimisation can be accepted or rejected on evidence.
//!
//! Run with `cargo bench -p taladb-core`. Add `-- --save-baseline before` and
//! `-- --baseline before` around a change to get a like-for-like comparison.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use taladb_core::vector::{VectorMetric, l2_norm, score_with_query_norm};
use taladb_core::{Database, Filter, Value};

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
fn bench_vector_scoring(c: &mut Criterion) {
    let mut group = c.benchmark_group("vector/score_one");
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
            group.bench_with_input(
                BenchmarkId::new(format!("{metric:?}"), dims),
                &dims,
                |b, _| {
                    b.iter(|| {
                        score_with_query_norm(
                            black_box(&metric),
                            black_box(query),
                            black_box(norm),
                            black_box(stored),
                        )
                    });
                },
            );
        }
    }
    group.finish();
}

/// End-to-end exact vector search, which is what the README's numbers describe.
fn bench_find_nearest(c: &mut Criterion) {
    const DIMS: usize = 384;
    let mut group = c.benchmark_group("vector/find_nearest");
    group.sample_size(20);

    for count in [1_000usize, 10_000] {
        let db = Database::open_in_memory().unwrap();
        let col = db.collection("docs").unwrap();
        let vectors = make_vectors(count + 1, DIMS);
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

        let query = &vectors[0];
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, _| {
            b.iter(|| {
                col.find_nearest(black_box("embedding"), black_box(query), 10, None)
                    .unwrap()
            });
        });
    }
    group.finish();
}

/// Filtered scans — the other half of a hybrid query, and the path that the
/// `Int`/`Float` comparison fix runs through.
fn bench_query(c: &mut Criterion) {
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

    let mut group = c.benchmark_group("query");
    group.sample_size(30);
    group.bench_function("range_scan_unindexed", |b| {
        b.iter(|| {
            col.find(black_box(Filter::Gte("n".into(), Value::Int(5_000))))
                .unwrap()
        });
    });
    // Mixed Int/Float comparison takes the exact-comparison path rather than a
    // plain integer compare — worth watching that it stays cheap.
    group.bench_function("range_scan_mixed_numeric", |b| {
        b.iter(|| {
            col.find(black_box(Filter::Gte("n".into(), Value::Float(5_000.5))))
                .unwrap()
        });
    });
    group.bench_function("equality", |b| {
        b.iter(|| {
            col.find(black_box(Filter::Eq("bucket".into(), Value::Int(42))))
                .unwrap()
        });
    });
    group.finish();
}

/// Write throughput, and the snapshot export that the WASM persistence path
/// calls on every save.
fn bench_writes_and_snapshot(c: &mut Criterion) {
    let mut group = c.benchmark_group("write");
    group.sample_size(20);

    group.throughput(Throughput::Elements(1_000));
    group.bench_function("insert_many_1k", |b| {
        b.iter_batched(
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
    });

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
    group.bench_function("export_snapshot_10k", |b| {
        b.iter(|| db.export_snapshot().unwrap());
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_vector_scoring,
    bench_find_nearest,
    bench_query,
    bench_writes_and_snapshot
);
criterion_main!(benches);
