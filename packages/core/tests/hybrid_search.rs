//! Reciprocal rank fusion over the text and vector retrievers.
//!
//! The point of hybrid retrieval is that the two halves fail differently:
//! BM25 misses paraphrases, vectors miss exact identifiers. These tests set
//! up documents where exactly one retriever can find the answer, and check
//! that fusion recovers it.

use taladb_core::Database;
use taladb_core::bm25::RrfParams;
use taladb_core::document::{Document, Value};
use taladb_core::fts::HybridQuery;
use taladb_core::query::Filter;

/// Deliberately crude 4-dimensional "embeddings" so similarity is obvious
/// by inspection rather than hidden behind a model.
const TOPIC_A: [f32; 4] = [1.0, 0.0, 0.0, 0.0];
const TOPIC_B: [f32; 4] = [0.0, 1.0, 0.0, 0.0];

fn title(doc: &Document) -> String {
    match doc.get("title") {
        Some(Value::Str(s)) => s.clone(),
        _ => String::new(),
    }
}

fn titles(results: &[taladb_core::fts::HybridSearchResult]) -> Vec<String> {
    results.iter().map(|r| title(&r.document)).collect()
}

/// `rows` = (title, body, vector, locale)
fn db_with(rows: &[(&str, &str, [f32; 4], &str)]) -> Database {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("docs").unwrap();
    for (t, body, vec, locale) in rows {
        col.insert(vec![
            ("title".into(), Value::Str((*t).into())),
            ("body".into(), Value::Str((*body).into())),
            ("locale".into(), Value::Str((*locale).into())),
            (
                "embedding".into(),
                Value::Array(vec.iter().map(|f| Value::Float(f64::from(*f))).collect()),
            ),
        ])
        .unwrap();
    }
    col.create_fts_index("body").unwrap();
    col.create_vector_index("embedding", 4, Default::default(), None)
        .unwrap();
    db
}

#[test]
fn recovers_an_exact_term_the_vector_retriever_would_miss() {
    // "SKU-9931" is a rare token with no semantic neighbourhood — classic
    // vector-search blind spot, classic BM25 strength.
    let db = db_with(&[
        ("target", "order SKU-9931 shipped", TOPIC_B, "en"),
        (
            "semantic",
            "shipping and delivery information",
            TOPIC_A,
            "en",
        ),
    ]);
    let col = db.collection("docs").unwrap();

    let results = col
        .hybrid_search(HybridQuery::new(
            "body",
            "SKU-9931",
            "embedding",
            &TOPIC_A,
            5,
        ))
        .unwrap();

    // The query vector points at "semantic", yet the exact term wins.
    assert_eq!(titles(&results)[0], "target");
}

#[test]
fn recovers_a_paraphrase_the_text_retriever_would_miss() {
    // No shared tokens with the query at all — only the vector can find it.
    let db = db_with(&[
        ("paraphrase", "zzzz yyyy xxxx", TOPIC_A, "en"),
        (
            "unrelated",
            "completely different subject matter",
            TOPIC_B,
            "en",
        ),
    ]);
    let col = db.collection("docs").unwrap();

    let results = col
        .hybrid_search(HybridQuery::new(
            "body",
            "nonmatching query terms",
            "embedding",
            &TOPIC_A,
            5,
        ))
        .unwrap();

    assert_eq!(titles(&results)[0], "paraphrase");
}

#[test]
fn agreement_between_retrievers_wins() {
    let db = db_with(&[
        ("both", "vector database search", TOPIC_A, "en"),
        ("text_only", "vector database search", TOPIC_B, "en"),
        ("vector_only", "irrelevant words here", TOPIC_A, "en"),
    ]);
    let col = db.collection("docs").unwrap();

    let results = col
        .hybrid_search(HybridQuery::new(
            "body",
            "vector database",
            "embedding",
            &TOPIC_A,
            5,
        ))
        .unwrap();

    assert_eq!(
        titles(&results)[0],
        "both",
        "a document both retrievers rank should outrank either alone"
    );
}

#[test]
fn reports_which_retriever_found_each_document() {
    let db = db_with(&[
        ("text_hit", "unmistakable keyword", TOPIC_B, "en"),
        ("vector_hit", "nothing in common", TOPIC_A, "en"),
    ]);
    let col = db.collection("docs").unwrap();

    let results = col
        .hybrid_search(HybridQuery::new(
            "body",
            "unmistakable",
            "embedding",
            &TOPIC_A,
            5,
        ))
        .unwrap();

    let text_hit = results
        .iter()
        .find(|r| title(&r.document) == "text_hit")
        .unwrap();
    assert!(text_hit.text_rank.is_some(), "found by BM25");

    let vector_hit = results
        .iter()
        .find(|r| title(&r.document) == "vector_hit")
        .unwrap();
    assert!(
        vector_hit.vector_rank.is_some(),
        "found by the vector index"
    );
    assert!(
        vector_hit.text_rank.is_none(),
        "shares no tokens with the query"
    );
}

#[test]
fn weights_can_disable_a_retriever() {
    let db = db_with(&[
        ("text_side", "distinctive phrase", TOPIC_B, "en"),
        ("vector_side", "nothing alike", TOPIC_A, "en"),
    ]);
    let col = db.collection("docs").unwrap();

    let mut query = HybridQuery::new("body", "distinctive", "embedding", &TOPIC_A, 5);
    query.rrf = RrfParams {
        k: 60.0,
        text_weight: 1.0,
        vector_weight: 0.0,
    };
    let text_only = col.hybrid_search(query).unwrap();
    assert_eq!(text_only[0].text_rank, Some(0));
    assert_eq!(titles(&text_only)[0], "text_side");

    let mut query = HybridQuery::new("body", "distinctive", "embedding", &TOPIC_A, 5);
    query.rrf = RrfParams {
        k: 60.0,
        text_weight: 0.0,
        vector_weight: 1.0,
    };
    let vector_only = col.hybrid_search(query).unwrap();
    assert_eq!(titles(&vector_only)[0], "vector_side");
}

#[test]
fn the_filter_applies_to_both_retrievers() {
    let db = db_with(&[
        ("english", "shared term here", TOPIC_A, "en"),
        ("tagalog", "shared term here", TOPIC_A, "tl"),
    ]);
    let col = db.collection("docs").unwrap();

    let query = HybridQuery::new("body", "shared term", "embedding", &TOPIC_A, 10)
        .filter(Filter::Eq("locale".into(), Value::Str("en".into())));
    let results = col.hybrid_search(query).unwrap();

    assert_eq!(
        titles(&results),
        vec!["english"],
        "a filtered-out document must not slip in via either retriever"
    );
}

#[test]
fn a_filter_matching_nothing_returns_nothing() {
    let db = db_with(&[("a", "term", TOPIC_A, "en")]);
    let col = db.collection("docs").unwrap();

    let query = HybridQuery::new("body", "term", "embedding", &TOPIC_A, 10)
        .filter(Filter::Eq("locale".into(), Value::Str("xx".into())));
    assert!(col.hybrid_search(query).unwrap().is_empty());
}

#[test]
fn results_are_deduplicated_across_retrievers() {
    // One document found by both sides must appear exactly once.
    let db = db_with(&[("only", "matching term", TOPIC_A, "en")]);
    let col = db.collection("docs").unwrap();

    let results = col
        .hybrid_search(HybridQuery::new(
            "body",
            "matching term",
            "embedding",
            &TOPIC_A,
            10,
        ))
        .unwrap();

    assert_eq!(results.len(), 1);
    assert!(results[0].text_rank.is_some() && results[0].vector_rank.is_some());
}

#[test]
fn top_k_bounds_the_result_set() {
    let rows: Vec<(String, String, [f32; 4], String)> = (0..10)
        .map(|i| {
            (
                format!("doc{i}"),
                "common term".to_string(),
                TOPIC_A,
                "en".to_string(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str, [f32; 4], &str)> = rows
        .iter()
        .map(|(a, b, c, d)| (a.as_str(), b.as_str(), *c, d.as_str()))
        .collect();
    let db = db_with(&borrowed);
    let col = db.collection("docs").unwrap();

    let results = col
        .hybrid_search(HybridQuery::new(
            "body",
            "common term",
            "embedding",
            &TOPIC_A,
            3,
        ))
        .unwrap();
    assert_eq!(results.len(), 3);

    let none = col
        .hybrid_search(HybridQuery::new(
            "body",
            "common term",
            "embedding",
            &TOPIC_A,
            0,
        ))
        .unwrap();
    assert!(none.is_empty());
}

#[test]
fn ranking_is_stable_across_identical_queries() {
    let rows: Vec<(String, String, [f32; 4], String)> = (0..6)
        .map(|i| {
            (
                format!("doc{i}"),
                "identical body text".to_string(),
                TOPIC_A,
                "en".to_string(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str, [f32; 4], &str)> = rows
        .iter()
        .map(|(a, b, c, d)| (a.as_str(), b.as_str(), *c, d.as_str()))
        .collect();
    let db = db_with(&borrowed);
    let col = db.collection("docs").unwrap();

    let run = || {
        titles(
            &col.hybrid_search(HybridQuery::new(
                "body",
                "identical body",
                "embedding",
                &TOPIC_A,
                6,
            ))
            .unwrap(),
        )
    };
    assert_eq!(run(), run(), "hash iteration must not leak into the order");
}

#[test]
fn candidates_bounds_how_much_each_retriever_contributes() {
    // Eight documents every retriever can match. `candidates` caps how many
    // each side proposes before fusion, so a pool of 1 can only ever produce
    // the single best text hit plus the single best vector hit.
    let rows: Vec<(String, String, [f32; 4], String)> = (0..8)
        .map(|i| {
            (
                format!("doc{i}"),
                "shared term".to_string(),
                TOPIC_A,
                "en".to_string(),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str, [f32; 4], &str)> = rows
        .iter()
        .map(|(a, b, c, d)| (a.as_str(), b.as_str(), *c, d.as_str()))
        .collect();
    let db = db_with(&borrowed);
    let col = db.collection("docs").unwrap();

    let mut narrow = HybridQuery::new("body", "shared term", "embedding", &TOPIC_A, 8);
    narrow.candidates = Some(1);
    let narrow_hits = col.hybrid_search(narrow).unwrap();
    assert!(
        narrow_hits.len() <= 2,
        "a pool of 1 per retriever cannot yield more than 2 documents, got {}",
        narrow_hits.len()
    );

    let mut wide = HybridQuery::new("body", "shared term", "embedding", &TOPIC_A, 8);
    wide.candidates = Some(8);
    let wide_hits = col.hybrid_search(wide).unwrap();
    assert_eq!(wide_hits.len(), 8, "a wider pool reaches every document");
    assert!(wide_hits.len() > narrow_hits.len());
}

#[test]
fn a_document_only_one_retriever_finds_still_ranks() {
    // The recall case fusion exists for: `lonely` shares no tokens with the
    // query, so only the vector side proposes it — and it must still appear
    // rather than being dropped for missing from the other ranking.
    let db = db_with(&[
        ("keyword", "shared term", TOPIC_B, "en"),
        ("lonely", "zzz", TOPIC_A, "en"),
    ]);
    let col = db.collection("docs").unwrap();

    let results = col
        .hybrid_search(HybridQuery::new(
            "body",
            "shared term",
            "embedding",
            &TOPIC_A,
            5,
        ))
        .unwrap();

    let lonely = results
        .iter()
        .find(|r| title(&r.document) == "lonely")
        .expect("vector-only match must survive fusion");
    assert!(lonely.text_rank.is_none());
    assert_eq!(lonely.vector_rank, Some(0));
}

/// `top_k` crosses into the engine straight from a caller — a `u32` from JS
/// through the wasm and napi bindings, a raw `usize` through the React Native
/// FFI — and the candidate pool is derived from it by multiplication. That
/// multiply used to be a plain `*`, so a large `top_k` panicked with "attempt to
/// multiply with overflow" in debug and, with overflow checks off, wrapped in
/// release into a 20-document pool that returned a quietly under-recalled
/// result set. On wasm32 a `usize` is 32 bits, so `top_k` only had to exceed
/// `u32::MAX / 4` to get there.
#[test]
fn a_huge_top_k_saturates_the_candidate_pool_instead_of_overflowing() {
    let db = db_with(&[
        ("keyword", "shared term", TOPIC_B, "en"),
        ("lonely", "zzz", TOPIC_A, "en"),
    ]);
    let col = db.collection("docs").unwrap();

    // The smallest `top_k` for which `top_k * 4` leaves the address space.
    let top_k = usize::MAX / 4 + 1;
    let results = col
        .hybrid_search(HybridQuery::new(
            "body",
            "shared term",
            "embedding",
            &TOPIC_A,
            top_k,
        ))
        .expect("a top_k past the overflow boundary must still answer");

    // Asking for more than the collection holds returns the collection.
    assert_eq!(titles(&results).len(), 2);
}
