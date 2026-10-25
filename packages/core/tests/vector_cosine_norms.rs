//! Exact cosine search on a retained vector block scores with each stored
//! vector's squared norm, computed once when the block is decoded. Streamed
//! vectors still recompute it per comparison.
//!
//! Both must give the same score bits as `score_with_query_norm`, the kernel
//! exact search used before norms were cached. These tests compare
//! `f32::to_bits()` across dimensions, numerical edge cases, cache budgets,
//! filters, writes, reopening, index changes and concurrent access. They also
//! check that every norm byte is charged to the cache budget.

use taladb::collection::Update;
use taladb::vector::{l2_norm, score_with_query_norm};
use taladb::{Collection, Database, Document, Filter, Ulid, Value, VectorMetric};

const UNLIMITED: usize = 64 * 1024 * 1024;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / 16_777_216.0 - 0.5
    }
    fn vector(&mut self, dims: usize) -> Vec<f32> {
        (0..dims).map(|_| self.next()).collect()
    }
}

/// Random vectors mixed with the cases that stress a cached norm: zero and
/// negative-zero vectors (score 0), magnitudes whose squares underflow,
/// subnormal components, squares that overflow to an infinite norm, and
/// identical or opposite vectors (exact score ties, broken by ID).
fn corpus(dims: usize, count: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut rng = Rng(seed);
    let scaled = |rng: &mut Rng, s: f32| rng.vector(dims).iter().map(|x| x * s).collect();
    (0..count)
        .map(|i| match i % 16 {
            0 => vec![0.0; dims],
            1 => vec![-0.0; dims],
            2 => scaled(&mut rng, 1e-20),
            3 => scaled(&mut rng, 1e-41),
            4 => scaled(&mut rng, 1e18),
            5 => scaled(&mut rng, 1e30),
            6 => vec![1.0; dims],
            7 => vec![-1.0; dims],
            _ => rng.vector(dims),
        })
        .collect()
}

fn embedding(v: &[f32]) -> Value {
    Value::Array(v.iter().map(|f| Value::Float(f64::from(*f))).collect())
}

fn docs(vectors: &[Vec<f32>]) -> Vec<Vec<(String, Value)>> {
    vectors
        .iter()
        .enumerate()
        .map(|(i, v)| {
            vec![
                ("bucket".to_string(), Value::Int((i % 100) as i64)),
                ("embedding".to_string(), embedding(v)),
            ]
        })
        .collect()
}

fn collection(db: &Database, name: &str, dims: usize, metric: VectorMetric) -> Collection {
    let col = db.collection(name).unwrap();
    col.create_vector_index("embedding", dims, Some(metric), None)
        .unwrap();
    col
}

fn vector_of(doc: &Document) -> Option<Vec<f32>> {
    match doc.get("embedding")? {
        Value::Array(items) => items
            .iter()
            .map(|v| match v {
                Value::Float(f) => Some(*f as f32),
                Value::Int(n) => Some(*n as f32),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// The stored vectors, read back through the document table.
fn stored(col: &Collection, filter: Filter) -> Vec<(Ulid, Vec<f32>)> {
    col.find(filter)
        .unwrap()
        .iter()
        .filter_map(|d| Some((d.id, vector_of(d)?)))
        .collect()
}

type Signature = Vec<(Ulid, u32)>;

fn signature(results: &[taladb::VectorSearchResult]) -> Signature {
    results
        .iter()
        .map(|r| (r.document.id, r.score.to_bits()))
        .collect()
}

/// Top-k by the pre-cache kernel: every stored vector scored with
/// `score_with_query_norm`, non-finite scores dropped, best score first, then
/// lowest ID, as exact search ranks.
fn oracle(metric: VectorMetric, query: &[f32], stored: &[(Ulid, Vec<f32>)], k: usize) -> Signature {
    let norm = l2_norm(query);
    let mut scored: Vec<(Ulid, f32)> = stored
        .iter()
        .map(|(id, v)| (*id, score_with_query_norm(&metric, query, norm, v)))
        .filter(|(_, s)| s.is_finite())
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.truncate(k);
    scored
        .into_iter()
        .map(|(id, s)| (id, s.to_bits()))
        .collect()
}

fn search(col: &Collection, query: &[f32], k: usize, filter: Option<Filter>) -> Signature {
    signature(&col.find_nearest("embedding", query, k, filter).unwrap())
}

/// Streams with retention disabled, then builds the block (cold), then reuses
/// it (warm). Returns the three signatures and the bytes retained afterwards.
fn three_ways(db: &Database, col: &Collection, query: &[f32], k: usize) -> ([Signature; 3], usize) {
    db.set_vector_cache_budget(0);
    let streamed = search(col, query, k, None);
    assert_eq!(db.vector_cache_stats().retained_bytes, 0);
    db.set_vector_cache_budget(UNLIMITED);
    let cold = search(col, query, k, None);
    let warm = search(col, query, k, None);
    (
        [streamed, cold, warm],
        db.vector_cache_stats().retained_bytes,
    )
}

fn queries(dims: usize, data: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03 ^ dims as u64);
    vec![
        rng.vector(dims),
        rng.vector(dims).iter().map(|x| x * 1e-20).collect(),
        rng.vector(dims).iter().map(|x| x * 1e18).collect(),
        vec![0.0; dims],
        // A stored vector: scores exactly 1.0 against itself.
        data[9].clone(),
    ]
}

#[test]
fn cosine_scores_on_a_retained_block_match_streaming_and_the_kernel_bit_for_bit() {
    for dims in [1, 7, 8, 9, 13, 128, 383, 384, 385, 768, 1536] {
        let count = 300;
        let data = corpus(dims, count, 0x2545_F491_4F6C_DD1D ^ dims as u64);
        let db = Database::open_in_memory().unwrap();
        let col = collection(&db, "docs", dims, VectorMetric::Cosine);
        col.insert_many(docs(&data)).unwrap();
        let all = stored(&col, Filter::All);
        assert_eq!(all.len(), count);

        for query in queries(dims, &data) {
            for k in [1, 10, count + 5] {
                let ([streamed, cold, warm], retained) = three_ways(&db, &col, &query, k);
                let expected = oracle(VectorMetric::Cosine, &query, &all, k);
                assert_eq!(streamed, expected, "streamed, {dims}-d, k {k}");
                assert_eq!(cold, expected, "cold block, {dims}-d, k {k}");
                assert_eq!(warm, expected, "warm block, {dims}-d, k {k}");
                // 16 B ID + 4 B per component + 4 B cached norm.
                assert_eq!(retained, count * (16 + 4 * dims + 4), "{dims}-d");
            }
        }
    }
}

#[test]
fn dot_and_euclidean_retain_no_norms_and_score_as_before() {
    for metric in [VectorMetric::Dot, VectorMetric::Euclidean] {
        for dims in [8, 9, 384] {
            let count = 200;
            let data = corpus(dims, count, 0x9E37_79B9_7F4A_7C15 ^ dims as u64);
            let db = Database::open_in_memory().unwrap();
            let col = collection(&db, "docs", dims, metric);
            col.insert_many(docs(&data)).unwrap();
            let all = stored(&col, Filter::All);
            for query in queries(dims, &data) {
                let ([streamed, cold, warm], retained) = three_ways(&db, &col, &query, 10);
                let expected = oracle(metric, &query, &all, 10);
                assert_eq!(streamed, expected, "{metric:?} {dims}-d");
                assert_eq!(cold, expected, "{metric:?} {dims}-d");
                assert_eq!(warm, expected, "{metric:?} {dims}-d");
                assert_eq!(retained, count * (16 + 4 * dims), "{metric:?} {dims}-d");
            }
        }
    }
}

#[test]
fn filtered_searches_over_a_retained_block_match_streaming() {
    // 1,000 vectors: `bucket == 1` matches 10 (sparse point reads), `bucket <
    // 20` 200 (an ID set checked against the block), `bucket < 50` 500 (more
    // than the 256-ID cap, so the filter is visited against the block).
    for dims in [9, 384] {
        let data = corpus(dims, 1000, 0xA076_1D64_78BD_642F ^ dims as u64);
        let db = Database::open_in_memory().unwrap();
        let col = collection(&db, "docs", dims, VectorMetric::Cosine);
        col.insert_many(docs(&data)).unwrap();
        let filters = [
            Filter::Eq("bucket".into(), Value::Int(1)),
            Filter::Lt("bucket".into(), Value::Int(20)),
            Filter::Lt("bucket".into(), Value::Int(50)),
        ];
        for query in queries(dims, &data) {
            db.set_vector_cache_budget(0);
            let streamed: Vec<_> = filters
                .iter()
                .map(|f| search(&col, &query, 10, Some(f.clone())))
                .collect();
            db.set_vector_cache_budget(UNLIMITED);
            search(&col, &query, 10, None); // builds the block
            assert!(db.vector_cache_stats().retained_bytes > 0);
            for (filter, streamed) in filters.iter().zip(&streamed) {
                let cached = search(&col, &query, 10, Some(filter.clone()));
                let expected = oracle(
                    VectorMetric::Cosine,
                    &query,
                    &stored(&col, filter.clone()),
                    10,
                );
                assert_eq!(*streamed, expected, "{dims}-d {filter:?}");
                assert_eq!(cached, expected, "{dims}-d {filter:?}");
            }
        }
    }
}

#[test]
fn a_block_whose_norms_do_not_fit_keeps_its_vectors_alone() {
    // 1,000 × 8-d: 48,000 B without norms, 52,000 B with them. Between the two,
    // the vectors are retained without norms, as before norms were cached, and
    // score from recomputed norms. Below 48,000 B the table streams.
    let data = corpus(8, 1000, 7);
    let db = Database::open_in_memory().unwrap();
    let cosine = collection(&db, "cosine", 8, VectorMetric::Cosine);
    let dot = collection(&db, "dot", 8, VectorMetric::Dot);
    cosine.insert_many(docs(&data)).unwrap();
    dot.insert_many(docs(&data)).unwrap();
    let query = Rng(11).vector(8);
    let expected = oracle(
        VectorMetric::Cosine,
        &query,
        &stored(&cosine, Filter::All),
        10,
    );

    for (budget, retained) in [
        (47_999, 0),
        (48_000, 48_000),
        (51_999, 48_000),
        (52_000, 52_000),
        (52_001, 52_000),
    ] {
        // Evict first: a block that still fits is kept when the budget grows.
        db.set_vector_cache_budget(0);
        db.set_vector_cache_budget(budget);
        for pass in ["cold", "warm"] {
            assert_eq!(
                search(&cosine, &query, 10, None),
                expected,
                "budget {budget}, {pass}"
            );
        }
        let stats = db.vector_cache_stats();
        assert_eq!(stats.retained_bytes, retained, "budget {budget}");
        assert_eq!(stats.active_bytes, 0);
        assert!(stats.peak_bytes <= stats.memory_budget_bytes);
    }
    // Dot retains no norms, so its boundary is where it always was.
    db.set_vector_cache_budget(0);
    db.set_vector_cache_budget(48_000);
    search(&dot, &query, 10, None);
    assert_eq!(db.vector_cache_stats().retained_bytes, 48_000);

    // The same band at 1 MiB and 384-d: 675 vectors take 1,047,600 B without
    // norms and 1,050,300 B with them.
    let data = corpus(384, 675, 13);
    let db = Database::open_in_memory().unwrap();
    let col = collection(&db, "docs", 384, VectorMetric::Cosine);
    col.insert_many(docs(&data)).unwrap();
    let query = Rng(17).vector(384);
    let expected = oracle(VectorMetric::Cosine, &query, &stored(&col, Filter::All), 10);
    for (budget, retained) in [(1024 * 1024, 1_047_600), (1_050_300, 1_050_300)] {
        db.set_vector_cache_budget(0);
        db.set_vector_cache_budget(budget);
        assert_eq!(search(&col, &query, 10, None), expected, "budget {budget}");
        assert_eq!(search(&col, &query, 10, None), expected, "budget {budget}");
        assert_eq!(db.vector_cache_stats().retained_bytes, retained);
    }
}

#[test]
fn blocks_of_several_indexes_are_charged_and_evicted_with_their_norms() {
    let data = corpus(8, 1000, 19);
    let db = Database::open_in_memory().unwrap();
    let a = collection(&db, "a", 8, VectorMetric::Cosine);
    let b = collection(&db, "b", 8, VectorMetric::Cosine);
    a.insert_many(docs(&data)).unwrap();
    b.insert_many(docs(&data[..500])).unwrap();
    let query = Rng(23).vector(8);
    let expect_a = oracle(VectorMetric::Cosine, &query, &stored(&a, Filter::All), 10);
    let expect_b = oracle(VectorMetric::Cosine, &query, &stored(&b, Filter::All), 10);

    db.set_vector_cache_budget(52_000 + 26_000);
    assert_eq!(search(&a, &query, 10, None), expect_a);
    assert_eq!(search(&b, &query, 10, None), expect_b);
    let stats = db.vector_cache_stats();
    assert_eq!((stats.retained_bytes, stats.vector_indexes), (78_000, 2));

    // One byte short: retaining `a` evicts `b`, the least recently used.
    db.set_vector_cache_budget(78_000 - 1);
    assert_eq!(search(&b, &query, 10, None), expect_b);
    assert_eq!(search(&a, &query, 10, None), expect_a);
    let stats = db.vector_cache_stats();
    assert_eq!((stats.retained_bytes, stats.vector_indexes), (52_000, 1));
    assert_eq!(search(&b, &query, 10, None), expect_b);
    assert_eq!(search(&a, &query, 10, None), expect_a);
    let stats = db.vector_cache_stats();
    assert!(stats.retained_bytes <= stats.budget_bytes);
    assert!(stats.peak_bytes <= stats.memory_budget_bytes);
    assert_eq!(stats.active_bytes, 0);
}

#[test]
fn inserts_updates_and_deletes_rebuild_the_norms() {
    for dims in [13, 384] {
        let data = corpus(dims, 400, 29 ^ dims as u64);
        let db = Database::open_in_memory().unwrap();
        let col = collection(&db, "docs", dims, VectorMetric::Cosine);
        // A second handle shares the cache; its writes must invalidate it too.
        let other = db.collection("docs").unwrap();
        let ids = col.insert_many(docs(&data)).unwrap();
        let query = Rng(31).vector(dims);
        db.set_vector_cache_budget(UNLIMITED);
        let check = |label: &str| {
            let expected = oracle(VectorMetric::Cosine, &query, &stored(&col, Filter::All), 10);
            assert_eq!(
                search(&col, &query, 10, None),
                expected,
                "{label}, {dims}-d"
            );
            assert_eq!(
                search(&other, &query, 10, None),
                expected,
                "{label}, {dims}-d"
            );
            let n = stored(&col, Filter::All).len();
            assert_eq!(
                db.vector_cache_stats().retained_bytes,
                n * (20 + 4 * dims),
                "{label}"
            );
        };
        check("initial");

        let scaled: Vec<f32> = query.iter().map(|x| x * 3.0).collect();
        col.insert(vec![("embedding".into(), embedding(&scaled))])
            .unwrap();
        check("insert");

        // Point a stored vector at the query: its old norm would misscore it.
        let id_of = |id: Ulid| Filter::Eq("_id".into(), Value::Str(id.to_string()));
        other
            .update_one(
                id_of(ids[100]),
                Update::Set(vec![("embedding".into(), embedding(&query))]),
            )
            .unwrap();
        check("update");
        assert!(
            search(&col, &query, 2, None)
                .iter()
                .any(|(id, _)| *id == ids[100])
        );

        col.update_one(
            id_of(ids[101]),
            Update::Set(vec![("embedding".into(), Value::Null)]),
        )
        .unwrap();
        check("embedding removed");

        other.delete_one(id_of(ids[100])).unwrap();
        check("delete");
        col.delete_many(Filter::Lt("bucket".into(), Value::Int(50)))
            .unwrap();
        check("delete_many");
    }
}

#[test]
fn reopening_a_database_rebuilds_identical_norms() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vectors.db");
    let data = corpus(385, 300, 37);
    let query = Rng(41).vector(385);
    let before;
    {
        let db = Database::open(&path).unwrap();
        let col = collection(&db, "docs", 385, VectorMetric::Cosine);
        col.insert_many(docs(&data)).unwrap();
        search(&col, &query, 10, None);
        before = search(&col, &query, 10, None);
        assert!(db.vector_cache_stats().retained_bytes > 0);
    }
    let db = Database::open(&path).unwrap();
    let col = db.collection("docs").unwrap();
    assert_eq!(db.vector_cache_stats().retained_bytes, 0);
    let expected = oracle(VectorMetric::Cosine, &query, &stored(&col, Filter::All), 10);
    assert_eq!(before, expected);
    assert_eq!(
        search(&col, &query, 10, None),
        expected,
        "cold after reopen"
    );
    assert_eq!(
        search(&col, &query, 10, None),
        expected,
        "warm after reopen"
    );
    col.insert(vec![("embedding".into(), embedding(&query))])
        .unwrap();
    let expected = oracle(VectorMetric::Cosine, &query, &stored(&col, Filter::All), 10);
    assert_eq!(
        search(&col, &query, 10, None),
        expected,
        "write after reopen"
    );
}

#[test]
fn recreating_the_index_with_another_metric_never_reuses_its_norms() {
    let data = corpus(9, 300, 43);
    let query = Rng(47).vector(9);
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    col.insert_many(docs(&data)).unwrap();
    for metric in [
        VectorMetric::Cosine,
        VectorMetric::Dot,
        VectorMetric::Cosine,
        VectorMetric::Euclidean,
        VectorMetric::Cosine,
    ] {
        col.create_vector_index("embedding", 9, Some(metric), None)
            .unwrap();
        let expected = oracle(metric, &query, &stored(&col, Filter::All), 10);
        assert_eq!(search(&col, &query, 10, None), expected, "{metric:?} cold");
        assert_eq!(search(&col, &query, 10, None), expected, "{metric:?} warm");
        col.drop_vector_index("embedding").unwrap();
    }
}

#[test]
fn concurrent_searches_score_every_hit_from_its_own_vector_during_writes() {
    let dims = 384;
    let data = corpus(dims, 2000, 53);
    let db = Database::open_in_memory().unwrap();
    let col = collection(&db, "docs", dims, VectorMetric::Cosine);
    let ids = col.insert_many(docs(&data)).unwrap();
    let query = Rng(59).vector(dims);
    let norm = l2_norm(&query);
    db.set_vector_cache_budget(UNLIMITED);

    std::thread::scope(|s| {
        for reader in 0..4 {
            let col = db.collection("docs").unwrap();
            let query = &query;
            s.spawn(move || {
                for round in 0..40 {
                    let k = if (reader + round) % 2 == 0 { 10 } else { 50 };
                    let hits = col.find_nearest("embedding", query, k, None).unwrap();
                    for pair in hits.windows(2) {
                        assert!(pair[0].score >= pair[1].score);
                    }
                    // Each hit was scored in the same snapshot as its document,
                    // so a norm left over from another vector would show here.
                    for hit in &hits {
                        let v = vector_of(&hit.document).unwrap();
                        let expected =
                            score_with_query_norm(&VectorMetric::Cosine, query, norm, &v);
                        assert_eq!(hit.score.to_bits(), expected.to_bits());
                    }
                }
            });
        }
        let writer = db.collection("docs").unwrap();
        let mut rng = Rng(61);
        for i in 0..60 {
            let id = Filter::Eq("_id".into(), Value::Str(ids[i * 7].to_string()));
            match i % 3 {
                0 => {
                    let v: Vec<f32> = query.iter().map(|x| x + rng.next() * 0.01).collect();
                    writer
                        .update_one(id, Update::Set(vec![("embedding".into(), embedding(&v))]))
                        .unwrap();
                }
                1 => {
                    writer.delete_one(id).unwrap();
                }
                _ => {
                    let v = rng.vector(dims);
                    writer
                        .insert(vec![("embedding".into(), embedding(&v))])
                        .unwrap();
                }
            }
        }
    });

    let expected = oracle(VectorMetric::Cosine, &query, &stored(&col, Filter::All), 50);
    assert_eq!(search(&col, &query, 50, None), expected);
    assert_eq!(search(&col, &query, 50, None), expected);
    let stats = db.vector_cache_stats();
    assert_eq!(stats.active_bytes, 0);
    assert!(stats.retained_bytes <= stats.budget_bytes);
    assert!(stats.peak_bytes <= stats.memory_budget_bytes);
}
