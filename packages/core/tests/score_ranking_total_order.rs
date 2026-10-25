//! Text and hybrid rankings must not panic on NaN scores.
//!
//! Both rankers sorted with `partial_cmp(..).unwrap_or(Equal)`, which calls NaN
//! equal to every score. That is not a total order, and the standard sort can
//! panic on it. Caller-chosen parameters reach NaN: `k1 = 3e38`, a finite value
//! the JS bindings pass through, overflows `term_score` to `inf / inf`;
//! RRF weights of `±1e39` arrive as `±inf` in `f32` and add to `inf - inf`.
//! Reproduced on v0.12.1 and `dev`. NaN scores now rank after every other.
use taladb::Database;
use taladb::bm25::{Bm25Params, RrfParams};
use taladb::document::Value;
use taladb::fts::HybridQuery;
use ulid::Ulid;

/// 200 documents of varied length and term frequency, so huge `k1` values
/// give finite, infinite and NaN scores side by side.
fn corpus() -> (Database, taladb::Collection) {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("t").unwrap();
    col.create_fts_index("body").unwrap();
    let docs = (0..200usize)
        .map(|i| {
            let mut words = vec!["kettle"; 1 + i % 4];
            words.extend(std::iter::repeat_n("other", (i * 7) % 23));
            let mut id = [0u8; 16];
            id[0] = 1;
            id[15] = i as u8;
            let embedding = [1.0, (i % 5) as f64, 0.0, (i % 3) as f64];
            vec![
                (
                    "_id".to_string(),
                    Value::Str(Ulid::from_bytes(id).to_string()),
                ),
                ("body".to_string(), Value::Str(words.join(" "))),
                (
                    "embedding".to_string(),
                    Value::Array(embedding.into_iter().map(Value::Float).collect()),
                ),
            ]
        })
        .collect();
    col.insert_many(docs).unwrap();
    col.create_vector_index("embedding", 4, Default::default(), None)
        .unwrap();
    (db, col)
}

/// Highest score first, NaN last, ties on id ascending.
fn assert_ranked(hits: &[(Ulid, f32)], label: &str) {
    for pair in hits.windows(2) {
        let ((ia, a), (ib, b)) = (pair[0], pair[1]);
        let ordered = match (a.is_nan(), b.is_nan()) {
            (false, false) => a > b || (a == b && ia < ib),
            (false, true) => true,
            (true, true) => ia < ib,
            (true, false) => false,
        };
        assert!(ordered, "{label}: {ia} ({a}) before {ib} ({b})");
    }
}

fn text_hits(col: &taladb::Collection, params: &Bm25Params, top_k: usize) -> Vec<(Ulid, f32)> {
    col.search_text_with("body", "kettle", top_k, params, None)
        .unwrap()
        .into_iter()
        .map(|h| (h.document.id, h.score))
        .collect()
}

#[test]
fn overflowing_bm25_parameters_rank_nan_last() {
    let (_db, col) = corpus();
    let mut saw_nan = false;
    for k1 in [3e38f32, f32::MAX, f32::INFINITY] {
        for b in [0.0f32, 0.75, 1.0, 2.0, 5.0] {
            let params = Bm25Params {
                k1,
                b,
                stopwords: true,
            };
            for top_k in [1000, 50, 7] {
                let label = format!("k1={k1:e} b={b} top_k={top_k}");
                let hits = text_hits(&col, &params, top_k);
                assert_eq!(hits.len(), top_k.min(200), "{label}");
                saw_nan |= hits.iter().any(|(_, s)| s.is_nan());
                assert_ranked(&hits, &label);
                // A bounded page is the head of the full ranking. Scores are
                // compared by bits: NaN is not equal to itself.
                let bits = |h: &[(Ulid, f32)]| -> Vec<(Ulid, u32)> {
                    h.iter().map(|(id, s)| (*id, s.to_bits())).collect()
                };
                let full = text_hits(&col, &params, 1000);
                assert_eq!(bits(&hits), bits(&full[..hits.len()]), "{label}");
                let rerun = text_hits(&col, &params, top_k);
                assert_eq!(bits(&hits), bits(&rerun), "{label} rerun");
            }
        }
    }
    assert!(saw_nan, "the fixture must produce NaN scores");
}

#[test]
fn ordinary_bm25_rankings_are_unchanged() {
    let (_db, col) = corpus();
    for params in [
        Bm25Params::default(),
        Bm25Params {
            k1: 2.0,
            b: 0.0,
            stopwords: false,
        },
    ] {
        let hits = text_hits(&col, &params, 1000);
        assert!(hits.iter().all(|(_, s)| s.is_finite()));
        assert_ranked(&hits, "default");
    }
}

#[test]
fn opposite_infinite_rrf_weights_rank_nan_last() {
    let (_db, col) = corpus();
    let query = [1.0f32, 2.0, 0.0, 1.0];
    for (text_weight, vector_weight) in [
        (f32::INFINITY, f32::NEG_INFINITY),
        (f32::NEG_INFINITY, f32::INFINITY),
        (1.0, 1.0),
    ] {
        for top_k in [200, 30] {
            let mut q = HybridQuery::new("body", "kettle", "embedding", &query, top_k);
            q.rrf = RrfParams {
                k: 60.0,
                text_weight,
                vector_weight,
            };
            // Each retriever keeps its own 60 best, so some documents are
            // found by one (an infinite score) and some by both (inf - inf).
            q.candidates = Some(60);
            let label = format!("weights {text_weight}/{vector_weight} top_k={top_k}");
            let hits: Vec<(Ulid, f32)> = col
                .hybrid_search(q)
                .unwrap()
                .into_iter()
                .map(|h| (h.document.id, h.score))
                .collect();
            assert!(!hits.is_empty(), "{label}");
            if text_weight.is_infinite() && top_k == 200 {
                let kinds = |f: fn(&f32) -> bool| hits.iter().filter(|(_, s)| f(s)).count();
                assert!(kinds(|s| s.is_nan()) > 0, "{label}: no NaN");
                assert!(kinds(|s| *s == f32::INFINITY) > 0, "{label}: no +inf");
                assert!(kinds(|s| *s == f32::NEG_INFINITY) > 0, "{label}: no -inf");
            }
            assert_ranked(&hits, &label);
        }
    }
}
