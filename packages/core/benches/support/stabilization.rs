use crate::suite::{self, Suite};
use criterion::Throughput;
use taladb::fts::HybridQuery;
use taladb::query::options::{FindOptions, SortSpec};
use taladb::{Database, Filter, HnswOptions, Ulid, Update, Value};

/// Document `i` of the stabilization fixture. Its ID is `i + 1`, so documents
/// added after a fixture of `count` start at `document(count)`.
pub fn document(i: usize) -> Vec<(String, Value)> {
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
}

/// The stabilization fixture: integer `n` and `bucket`, a zero `counter`, a short
/// `body` for FTS, a 1 KiB `payload`, an 8-d vector `v`, and fixed IDs.
pub fn seeded(count: usize) -> Database {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    // Keep setup memory bounded as well, especially for the optional 100k run.
    for start in (0..count).step_by(128) {
        col.insert_many((start..(start + 128).min(count)).map(document).collect())
            .unwrap();
    }
    col.create_index("n").unwrap();
    col.create_index("bucket").unwrap();
    col.create_fts_index("body").unwrap();
    col.create_vector_index("v", 8, None, None).unwrap();
    db
}

pub fn bench_stabilization(s: &mut Suite) {
    if !s.enabled("stabilization") {
        return;
    }
    let mut group = s.group("stabilization");
    let samples = std::env::var("TALADB_BENCH_SAMPLES")
        .map(|s| {
            s.parse::<usize>()
                .expect("TALADB_BENCH_SAMPLES must be an integer")
        })
        .unwrap_or(10);
    group.sample_size(samples);
    if std::env::var_os("TALADB_BENCH_FLAT").is_some() {
        group.flat();
    }
    let counts = if std::env::var_os("TALADB_BENCH_LARGE").is_some() {
        suite::sizes(&[1_000, 10_000, 100_000])
    } else {
        suite::sizes(&[1_000, 10_000])
    };
    for count in counts {
        let db = seeded(count);
        let col = db.collection("docs").unwrap();
        group.throughput(Throughput::Elements(count as u64));
        group.run_with("indexed_page", count, || {
            col.find_with_options(
                Filter::Gte("n".into(), Value::Int(0)),
                FindOptions {
                    limit: Some(10),
                    ..Default::default()
                },
            )
            .unwrap()
        });
        group.run_with("sorted_page", count, || {
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
        });
        group.run_with("unindexed_selective", count, || {
            col.find(Filter::Eq("counter".into(), Value::Int(-1)))
                .unwrap()
        });
        group.run_with("bm25", count, || {
            col.search_text("body", "rust storage", 10).unwrap()
        });
        group.run_with("bm25_selective", count, || {
            col.search_text_with(
                "body",
                "rust storage",
                10,
                &Default::default(),
                Some(Filter::Eq("bucket".into(), Value::Int(42))),
            )
            .unwrap()
        });
        group.run_with("update_many", count, || {
            col.update_many(
                Filter::All,
                Update::Inc(vec![("counter".into(), Value::Int(1))]),
            )
            .unwrap()
        });
        group.run_with("index_cycle", count, || {
            col.create_index("counter").unwrap();
            col.drop_index("counter").unwrap();
            col.create_compound_index(&["bucket", "n"]).unwrap();
            col.drop_compound_index(&["bucket", "n"]).unwrap();
        });
        group.run_with("fts_index_cycle", count, || {
            col.drop_fts_index("body").unwrap();
            col.create_fts_index("body").unwrap();
        });
        // Snapshot restore is outside the timed delete; every iteration deletes
        // the same indexed corpus, including the FTS and vector side tables.
        let snapshot = db.export_snapshot().unwrap();
        group.run_batched(
            "delete_many",
            Some(&count),
            || Database::restore_from_snapshot(&snapshot).unwrap(),
            |copy| {
                copy.collection("docs")
                    .unwrap()
                    .delete_many(Filter::All)
                    .unwrap()
            },
            criterion::BatchSize::PerIteration,
        );
        if count <= 10_000 {
            group.run_with("exact_vector", count, || {
                col.find_nearest("v", &[0.5; 8], 10, None).unwrap()
            });
            group.run_with("hybrid", count, || {
                let mut query = HybridQuery::new("body", "rust storage", "v", &[0.5; 8], 10);
                query.filter = Some(Filter::Eq("bucket".into(), Value::Int(42)));
                col.hybrid_search(query).unwrap()
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
            group.run_with("hnsw", count, || {
                col.find_nearest("v", &[0.5; 8], 10, None).unwrap()
            });
        }
    }
    group.finish();
}
