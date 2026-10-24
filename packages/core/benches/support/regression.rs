//! v0.13 regression workloads (`docs/reports/V013_BENCHMARK_PLAN.md` §2): the
//! document, text, bulk-write and index-maintenance paths that had Criterion
//! numbers but no gate. Workload IDs from the plan are noted on each case.
//!
//! Everything here uses API present at v0.12.1, because the gate builds this
//! file against the baseline checkout too. Fixtures use fixed IDs and seeds.
//!
//! Sizes: `TALADB_BENCH_SIZES` overrides the defaults; `TALADB_BENCH_LARGE` adds
//! 100k to the read-only document and text groups.

use crate::stabilization::{document, seeded};
use crate::suite::{self, Group, Rows, Suite, Unordered, UnorderedBatch};
use criterion::{BatchSize, Throughput};
use std::time::Duration;
use taladb::fts::HybridQuery;
use taladb::query::options::{FindOptions, SortSpec};
use taladb::{Accumulator, Database, Filter, GroupKey, Stage, Ulid, Update, Value};

fn large() -> bool {
    std::env::var_os("TALADB_BENCH_LARGE").is_some()
}

/// The stabilization group's controls, so every timed group follows the
/// same protocol (`TALADB_BENCH_SAMPLES`, `TALADB_BENCH_FLAT`).
fn configure(group: &mut Group) {
    let samples = std::env::var("TALADB_BENCH_SAMPLES")
        .map(|s| s.parse().expect("TALADB_BENCH_SAMPLES must be an integer"))
        .unwrap_or(10);
    group.sample_size(samples);
    if std::env::var_os("TALADB_BENCH_FLAT").is_some() {
        group.flat();
    }
}

/// For cases whose per-iteration setup (a snapshot restore) costs more than the
/// routine: one iteration per sample, whatever the CLI timing says.
fn configure_batched(group: &mut Group) {
    configure(group);
    group
        .flat()
        .timing(Duration::from_millis(1), Duration::from_millis(1));
}

fn id(i: usize) -> String {
    Ulid::from(i as u128 + 1).to_string()
}

fn by_id(i: usize) -> Filter {
    Filter::Eq("_id".into(), Value::Str(id(i)))
}

/// DOC-1..13 on the stabilization fixture plus a `(bucket, n)` compound index,
/// and HYB-2 on the same data. Read-only, so one fixture serves every case.
pub fn bench_documents(s: &mut Suite) {
    if !s.enabled("doc") && !s.enabled("hyb") {
        return;
    }
    let counts = suite::sizes(if large() {
        &[1_000, 10_000, 100_000]
    } else {
        &[1_000, 10_000]
    });
    for count in counts {
        let db = seeded(count);
        let col = db.collection("docs").unwrap();
        col.create_compound_index(&["bucket", "n"]).unwrap();
        let half = (count / 2) as i64;

        if s.enabled("doc") {
            let mut group = s.group("doc");
            configure(&mut group);
            group.throughput(Throughput::Elements(count as u64));

            // DOC-1: 16 point lookups by `_id` per iteration, through the
            // planner as the bindings' `findOne({ _id })` does.
            let lookups: Vec<usize> = (0..16).map(|k| (k * 7919) % count).collect();
            group.run_with("find_by_id", count, || {
                lookups
                    .iter()
                    .map(|&i| col.find_one(by_id(i)).unwrap())
                    .collect::<Vec<_>>()
            });
            // DOC-2: indexed equality, 1% of the collection.
            group.run_with("indexed_eq", count, || {
                Unordered(
                    col.find(Filter::Eq("bucket".into(), Value::Int(42)))
                        .unwrap(),
                )
            });
            // DOC-7: compound-index equality + range.
            group.run_with("compound_eq_range", count, || {
                col.find(Filter::And(vec![
                    Filter::Eq("bucket".into(), Value::Int(42)),
                    Filter::Gte("n".into(), Value::Int(half)),
                ]))
                .unwrap()
            });
            // DOC-8: counts through an index, equality and a 50% range.
            group.run_with("count_indexed_eq", count, || {
                col.count(Filter::Eq("bucket".into(), Value::Int(42)))
                    .unwrap()
            });
            group.run_with("count_indexed_range", count, || {
                col.count(Filter::Gte("n".into(), Value::Int(half)))
                    .unwrap()
            });
            // DOC-9: `$match` (indexed, 50%) then `$group` by bucket.
            group.run_with("aggregate_group", count, || {
                Rows(
                    col.aggregate(vec![
                        Stage::Match(Filter::Lt("n".into(), Value::Int(half))),
                        Stage::Group {
                            key: GroupKey::Field("bucket".into()),
                            accumulators: vec![
                                ("total".into(), Accumulator::Sum("n".into())),
                                ("mean".into(), Accumulator::Avg("n".into())),
                                ("docs".into(), Accumulator::Count),
                            ],
                        },
                    ])
                    .unwrap(),
                )
            });
            // DOC-10: a 100-document page with and without the large fields.
            let page = |exclude: Option<Vec<String>>| FindOptions {
                limit: Some(100),
                exclude,
                ..Default::default()
            };
            group.run_with("page_full", count, || {
                col.find_with_options(Filter::Gte("n".into(), Value::Int(0)), page(None))
                    .unwrap()
            });
            group.run_with("page_projected", count, || {
                col.find_with_options(
                    Filter::Gte("n".into(), Value::Int(0)),
                    page(Some(vec!["payload".into(), "v".into()])),
                )
                .unwrap()
            });
            // DOC-11: eight two-sided windows of 100 documents at fixed offsets.
            // Hashed as sets: the result order of this query is not defined.
            let windows: Vec<i64> = (0..8)
                .map(|k| ((k * 7919) % (count - 100)) as i64)
                .collect();
            group.run_with("range_window", count, || {
                UnorderedBatch(
                    windows
                        .iter()
                        .map(|&x| {
                            col.find(Filter::And(vec![
                                Filter::Gte("n".into(), Value::Int(x)),
                                Filter::Lt("n".into(), Value::Int(x + 100)),
                            ]))
                            .unwrap()
                        })
                        .collect(),
                )
            });
            // DOC-12: the ascending counterpart of `stabilization/sorted_page`.
            group.run_with("sorted_page_asc", count, || {
                col.find_with_options(
                    Filter::All,
                    FindOptions {
                        sort: vec![SortSpec::asc("n")],
                        skip: 20,
                        limit: Some(10),
                        ..Default::default()
                    },
                )
                .unwrap()
            });
            // DOC-13: selective unindexed count (no document matches).
            group.run_with("count_unindexed", count, || {
                col.count(Filter::Eq("counter".into(), Value::Int(-1)))
                    .unwrap()
            });
            group.finish();
        }

        // HYB-2: hybrid search without a filter (HYB-1 is
        // `stabilization/hybrid`). 8-d exact vectors, as in that case.
        if s.enabled("hyb") && count <= 10_000 {
            let mut group = s.group("hyb");
            configure(&mut group);
            group.throughput(Throughput::Elements(count as u64));
            group.run_with("hybrid_unfiltered", count, || {
                col.hybrid_search(HybridQuery::new("body", "rust storage", "v", &[0.5; 8], 10))
                    .unwrap()
            });
            group.finish();
        }
    }
}

/// Deterministic Zipf-distributed text: ~5,000 terms, s = 1.1, 8–40 tokens a
/// document, so posting lists are skewed the way real text is. The stabilization
/// fixture gives every document the same three words, which BM25 cannot rank.
struct Zipf {
    cdf: Vec<f64>,
    state: u64,
}

impl Zipf {
    fn new() -> Self {
        const TERMS: usize = 5_000;
        let weights: Vec<f64> = (1..=TERMS).map(|r| 1.0 / (r as f64).powf(1.1)).collect();
        let total: f64 = weights.iter().sum();
        let mut acc = 0.0;
        let cdf = weights
            .iter()
            .map(|w| {
                acc += w / total;
                acc
            })
            .collect();
        Self {
            cdf,
            state: 0x9E37_79B9_7F4A_7C15,
        }
    }
    fn next(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }
    fn body(&mut self) -> String {
        let tokens = 8 + (self.next() % 33) as usize;
        (0..tokens)
            .map(|_| {
                let u = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
                let rank = self.cdf.partition_point(|&c| c < u).min(self.cdf.len() - 1) + 1;
                format!("w{rank}")
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn document(&mut self, i: usize) -> Vec<(String, Value)> {
        vec![
            ("_id".into(), Value::Str(id(i))),
            ("n".into(), Value::Int(i as i64)),
            ("body".into(), Value::Str(self.body())),
        ]
    }
}

/// FTS-3 and FTS-5 on the Zipf corpus.
pub fn bench_text(s: &mut Suite) {
    if !s.enabled("fts") {
        return;
    }
    let counts = suite::sizes(if large() {
        &[10_000, 100_000]
    } else {
        &[10_000]
    });
    for count in counts {
        let mut zipf = Zipf::new();
        let db = Database::open_in_memory().unwrap();
        let col = db.collection("docs").unwrap();
        for start in (0..count).step_by(128) {
            col.insert_many(
                (start..(start + 128).min(count))
                    .map(|i| zipf.document(i))
                    .collect(),
            )
            .unwrap();
        }
        col.create_fts_index("body").unwrap();
        // The same generator continues, so inserted bodies follow the corpus.
        let extra: Vec<_> = (count..count + 1_000).map(|i| zipf.document(i)).collect();

        let mut group = s.group("fts");
        configure(&mut group);
        group.throughput(Throughput::Elements(count as u64));
        // FTS-3: one frequent term and one mid-frequency term.
        group.run_with("bm25_zipf", count, || {
            col.search_text("body", "w3 w120", 10).unwrap()
        });
        group.finish();

        // FTS-5: 1,000 documents into the FTS-indexed collection.
        if count <= 10_000 {
            let snapshot = db.export_snapshot().unwrap();
            let mut group = s.group("fts");
            configure_batched(&mut group);
            group.run_batched(
                "insert_indexed",
                Some(&count),
                || {
                    (
                        Database::restore_from_snapshot(&snapshot).unwrap(),
                        extra.clone(),
                    )
                },
                |(copy, docs)| copy.collection("docs").unwrap().insert_many(docs).unwrap(),
                BatchSize::PerIteration,
            );
            group.finish();
        }
    }
}

/// BULK-2 and BULK-5 on the stabilization fixture with every index kind
/// (scalar, compound, FTS, vector).
pub fn bench_bulk(s: &mut Suite) {
    if !s.enabled("bulk") {
        return;
    }
    for count in suite::sizes(&[10_000]) {
        let db = seeded(count);
        db.collection("docs")
            .unwrap()
            .create_compound_index(&["bucket", "n"])
            .unwrap();
        let snapshot = db.export_snapshot().unwrap();

        // BULK-2: insert 1,000 documents into the fully indexed collection.
        let mut group = s.group("bulk");
        configure_batched(&mut group);
        group.run_batched(
            "insert_many_indexed",
            Some(&count),
            || {
                let docs: Vec<_> = (count..count + 1_000).map(document).collect();
                (Database::restore_from_snapshot(&snapshot).unwrap(), docs)
            },
            |(copy, docs)| copy.collection("docs").unwrap().insert_many(docs).unwrap(),
            BatchSize::PerIteration,
        );
        group.finish();

        // BULK-5: single-document writes by `_id`, as `updateOne`/`deleteOne`
        // with an `_id` filter issue them.
        let col = db.collection("docs").unwrap();
        let mut group = s.group("bulk");
        configure(&mut group);
        let target = count / 2;
        group.run_with("update_one_by_id", count, || {
            col.update_one(
                by_id(target),
                Update::Inc(vec![("counter".into(), Value::Int(1))]),
            )
            .unwrap()
        });
        // The document is re-inserted, untimed, before each delete.
        let extra = count + 1_000;
        group.run_batched(
            "delete_one_by_id",
            Some(&count),
            || {
                col.insert(document(extra)).unwrap();
            },
            |()| col.delete_one(by_id(extra)).unwrap(),
            BatchSize::PerIteration,
        );
        group.finish();
    }
}

/// IDX-2: write amplification — `insert_many` of 1,000 documents into a 10k
/// collection carrying more and more indexes. IDX-1 is
/// `stabilization/index_cycle`.
pub fn bench_index_maintenance(s: &mut Suite) {
    if !s.enabled("idx") {
        return;
    }
    let count = 10_000;
    let configurations: [(&str, &[&str], bool, bool); 5] = [
        ("none", &[], false, false),
        ("scalar1", &["n"], false, false),
        ("scalar3", &["n", "bucket", "counter"], false, false),
        ("scalar3_fts", &["n", "bucket", "counter"], true, false),
        (
            "scalar3_fts_vector",
            &["n", "bucket", "counter"],
            true,
            true,
        ),
    ];
    let mut group = s.group("idx");
    configure_batched(&mut group);
    for (name, fields, fts, vector) in configurations {
        let db = Database::open_in_memory().unwrap();
        let col = db.collection("docs").unwrap();
        for start in (0..count).step_by(128) {
            col.insert_many((start..(start + 128).min(count)).map(document).collect())
                .unwrap();
        }
        for field in fields {
            col.create_index(field).unwrap();
        }
        if fts {
            col.create_fts_index("body").unwrap();
        }
        if vector {
            col.create_vector_index("v", 8, None, None).unwrap();
        }
        let snapshot = db.export_snapshot().unwrap();
        group.run_batched(
            "insert_amplification",
            Some(&name),
            || {
                let docs: Vec<_> = (count..count + 1_000).map(document).collect();
                (Database::restore_from_snapshot(&snapshot).unwrap(), docs)
            },
            |(copy, docs)| copy.collection("docs").unwrap().insert_many(docs).unwrap(),
            BatchSize::PerIteration,
        );
    }
    group.finish();
}
