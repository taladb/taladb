use criterion::{BenchmarkId, Criterion, Throughput};
use taladb::fts::HybridQuery;
use taladb::query::options::{FindOptions, SortSpec};
use taladb::{Database, Filter, HnswOptions, Ulid, Update, Value};

fn seeded(count: usize) -> Database {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    // Keep setup memory bounded as well, especially for the optional 100k run.
    for start in (0..count).step_by(128) {
        col.insert_many(
            (start..(start + 128).min(count))
                .map(|i| {
                    vec![
                        // Fixed IDs also fix the HNSW level/topology seed.
                        (
                            "_id".into(),
                            Value::Str(Ulid::from(i as u128 + 1).to_string()),
                        ),
                        ("n".into(), Value::Int(i as i64)),
                        ("bucket".into(), Value::Int((i % 100) as i64)),
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
                    ]
                })
                .collect(),
        )
        .unwrap();
    }
    col.create_index("n").unwrap();
    col.create_index("bucket").unwrap();
    col.create_fts_index("body").unwrap();
    col.create_vector_index("v", 8, None, None).unwrap();
    db
}

pub fn bench_stabilization(c: &mut Criterion) {
    let mut group = c.benchmark_group("stabilization");
    let samples = std::env::var("TALADB_BENCH_SAMPLES")
        .map(|s| {
            s.parse::<usize>()
                .expect("TALADB_BENCH_SAMPLES must be an integer")
        })
        .unwrap_or(10);
    group.sample_size(samples);
    if std::env::var_os("TALADB_BENCH_FLAT").is_some() {
        group.sampling_mode(criterion::SamplingMode::Flat);
    }
    let counts = if std::env::var_os("TALADB_BENCH_LARGE").is_some() {
        vec![1_000, 10_000, 100_000]
    } else {
        vec![1_000, 10_000]
    };
    for count in counts {
        let db = seeded(count);
        let col = db.collection("docs").unwrap();
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(BenchmarkId::new("indexed_page", count), &count, |b, _| {
            b.iter(|| {
                col.find_with_options(
                    Filter::Gte("n".into(), Value::Int(0)),
                    FindOptions {
                        limit: Some(10),
                        ..Default::default()
                    },
                )
                .unwrap()
            })
        });
        group.bench_with_input(BenchmarkId::new("sorted_page", count), &count, |b, _| {
            b.iter(|| {
                col.find_with_options(
                    Filter::All,
                    FindOptions {
                        sort: vec![SortSpec::desc("n")],
                        skip: 20,
                        limit: Some(10),
                        ..Default::default()
                    },
                )
                .unwrap()
            })
        });
        group.bench_with_input(
            BenchmarkId::new("unindexed_selective", count),
            &count,
            |b, _| {
                b.iter(|| {
                    col.find(Filter::Eq("counter".into(), Value::Int(-1)))
                        .unwrap()
                })
            },
        );
        group.bench_with_input(BenchmarkId::new("bm25", count), &count, |b, _| {
            b.iter(|| col.search_text("body", "rust storage", 10).unwrap())
        });
        group.bench_with_input(BenchmarkId::new("bm25_selective", count), &count, |b, _| {
            b.iter(|| {
                col.search_text_with(
                    "body",
                    "rust storage",
                    10,
                    &Default::default(),
                    Some(Filter::Eq("bucket".into(), Value::Int(42))),
                )
                .unwrap()
            })
        });
        group.bench_with_input(BenchmarkId::new("update_many", count), &count, |b, _| {
            b.iter(|| {
                col.update_many(
                    Filter::All,
                    Update::Inc(vec![("counter".into(), Value::Int(1))]),
                )
                .unwrap()
            })
        });
        group.bench_with_input(BenchmarkId::new("index_cycle", count), &count, |b, _| {
            b.iter(|| {
                col.create_index("counter").unwrap();
                col.drop_index("counter").unwrap();
                col.create_compound_index(&["bucket", "n"]).unwrap();
                col.drop_compound_index(&["bucket", "n"]).unwrap();
            })
        });
        group.bench_with_input(
            BenchmarkId::new("fts_index_cycle", count),
            &count,
            |b, _| {
                b.iter(|| {
                    col.drop_fts_index("body").unwrap();
                    col.create_fts_index("body").unwrap();
                })
            },
        );
        // Snapshot restore is outside the timed delete; every iteration deletes
        // the same indexed corpus, including the FTS and vector side tables.
        let snapshot = db.export_snapshot().unwrap();
        group.bench_with_input(BenchmarkId::new("delete_many", count), &count, |b, _| {
            b.iter_batched(
                || Database::restore_from_snapshot(&snapshot).unwrap(),
                |copy| {
                    copy.collection("docs")
                        .unwrap()
                        .delete_many(Filter::All)
                        .unwrap()
                },
                criterion::BatchSize::PerIteration,
            )
        });
        if count <= 10_000 {
            group.bench_with_input(BenchmarkId::new("exact_vector", count), &count, |b, _| {
                b.iter(|| col.find_nearest("v", &[0.5; 8], 10, None).unwrap())
            });
            group.bench_with_input(BenchmarkId::new("hybrid", count), &count, |b, _| {
                b.iter(|| {
                    let mut query = HybridQuery::new("body", "rust storage", "v", &[0.5; 8], 10);
                    query.filter = Some(Filter::Eq("bucket".into(), Value::Int(42)));
                    col.hybrid_search(query).unwrap()
                })
            });
            col.drop_vector_index("v").unwrap();
            col.create_vector_index(
                "v",
                8,
                None,
                Some(HnswOptions {
                    m: 8,
                    ef_construction: 32,
                }),
            )
            .unwrap();
            group.bench_with_input(BenchmarkId::new("hnsw", count), &count, |b, _| {
                b.iter(|| col.find_nearest("v", &[0.5; 8], 10, None).unwrap())
            });
        }
    }
    group.finish();
}
