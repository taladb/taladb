//! BM25 ranking over the full-text index.
//!
//! The scoring maths is unit-tested in `bm25.rs`; what matters here is that
//! term frequencies, document lengths, and corpus statistics stay correct
//! across inserts, updates, and deletes — the accounting, not the formula.

use taladb_core::Database;
use taladb_core::document::{Document, Value};
use taladb_core::error::TalaDbError;

fn body(doc: &Document) -> &str {
    match doc.get("body") {
        Some(Value::Str(s)) => s,
        _ => "",
    }
}

fn titles(results: &[taladb_core::fts::TextSearchResult]) -> Vec<String> {
    results
        .iter()
        .map(|r| match r.document.get("title") {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        })
        .collect()
}

fn db_with_articles(rows: &[(&str, &str)]) -> Database {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("articles").unwrap();
    for (title, text) in rows {
        col.insert(vec![
            ("title".into(), Value::Str((*title).into())),
            ("body".into(), Value::Str((*text).into())),
        ])
        .unwrap();
    }
    col.create_fts_index("body").unwrap();
    db
}

#[test]
fn ranks_by_relevance_not_insertion_order() {
    let db = db_with_articles(&[
        (
            "passing",
            "the word appears once here among many other filler words",
        ),
        ("dense", "rust rust rust"),
        ("absent", "nothing relevant in this document at all"),
    ]);
    let col = db.collection("articles").unwrap();

    let results = col.search_text("body", "rust", 10).unwrap();
    assert_eq!(titles(&results), vec!["dense"]);
    assert!(results[0].score > 0.0);
}

#[test]
fn matching_more_query_terms_scores_higher() {
    // OR semantics: `$contains` would require every token, BM25 rewards
    // covering more of the query.
    let db = db_with_articles(&[
        ("both", "database vector storage"),
        ("one", "database only here"),
    ]);
    let col = db.collection("articles").unwrap();

    let results = col.search_text("body", "database vector", 10).unwrap();
    assert_eq!(titles(&results), vec!["both", "one"]);
    assert!(results[0].score > results[1].score);
}

#[test]
fn shorter_documents_outrank_longer_ones_at_equal_frequency() {
    let filler = "filler ".repeat(200);
    let db = db_with_articles(&[("short", "taladb"), ("long", &format!("taladb {filler}"))]);
    let col = db.collection("articles").unwrap();

    let results = col.search_text("body", "taladb", 10).unwrap();
    assert_eq!(titles(&results), vec!["short", "long"]);
}

#[test]
fn common_terms_never_push_a_document_below_zero() {
    // Every document contains "the", so its IDF is ~0 — but it must not
    // subtract score from documents that also match a rare term.
    let db = db_with_articles(&[("rare", "the quasar"), ("plain", "the the the")]);
    let col = db.collection("articles").unwrap();

    let results = col.search_text("body", "the quasar", 10).unwrap();
    assert!(results.iter().all(|r| r.score >= 0.0));
    assert_eq!(titles(&results)[0], "rare");
}

#[test]
fn top_k_truncates_and_zero_returns_nothing() {
    let db = db_with_articles(&[
        ("a", "alpha match"),
        ("b", "alpha match"),
        ("c", "alpha match"),
    ]);
    let col = db.collection("articles").unwrap();

    assert_eq!(col.search_text("body", "alpha", 2).unwrap().len(), 2);
    assert_eq!(col.search_text("body", "alpha", 0).unwrap().len(), 0);
}

#[test]
fn a_query_with_no_matches_returns_empty() {
    let db = db_with_articles(&[("a", "alpha beta")]);
    let col = db.collection("articles").unwrap();

    assert!(
        col.search_text("body", "nonexistent", 10)
            .unwrap()
            .is_empty()
    );
    // Tokens below the minimum length tokenize away to nothing.
    assert!(col.search_text("body", "x", 10).unwrap().is_empty());
    assert!(col.search_text("body", "", 10).unwrap().is_empty());
}

#[test]
fn updates_replace_term_frequencies_rather_than_accumulating() {
    let db = db_with_articles(&[("doc", "rust rust rust rust rust")]);
    let col = db.collection("articles").unwrap();

    let before = col.search_text("body", "rust", 10).unwrap();
    assert_eq!(before.len(), 1);

    // Rewrite the body so the term now occurs once.
    let id = before[0].document.id;
    col.update_one(
        taladb_core::query::Filter::Eq("_id".into(), Value::Str(id.to_string())),
        taladb_core::collection::Update::Set(vec![(
            "body".into(),
            Value::Str("rust appears once now".into()),
        )]),
    )
    .unwrap();

    let after = col.search_text("body", "rust", 10).unwrap();
    assert_eq!(
        after.len(),
        1,
        "stale postings would duplicate the document"
    );
    assert_eq!(body(&after[0].document), "rust appears once now");
    assert!(
        after[0].score < before[0].score,
        "score should fall with term frequency: {} -> {}",
        before[0].score,
        after[0].score
    );
}

#[test]
fn deletes_remove_documents_from_the_ranking() {
    let db = db_with_articles(&[("keep", "shared term"), ("drop", "shared term")]);
    let col = db.collection("articles").unwrap();

    let all = col.search_text("body", "shared", 10).unwrap();
    assert_eq!(all.len(), 2);

    let doomed = all
        .iter()
        .find(|r| matches!(r.document.get("title"), Some(Value::Str(s)) if s == "drop"))
        .unwrap()
        .document
        .id;
    col.delete_one(taladb_core::query::Filter::Eq(
        "_id".into(),
        Value::Str(doomed.to_string()),
    ))
    .unwrap();

    let remaining = col.search_text("body", "shared", 10).unwrap();
    assert_eq!(titles(&remaining), vec!["keep"]);
}

#[test]
fn documents_inserted_after_the_index_are_ranked_too() {
    // The backfill path and the incremental write path must agree.
    let db = db_with_articles(&[("original", "alpha")]);
    let col = db.collection("articles").unwrap();

    col.insert(vec![
        ("title".into(), Value::Str("later".into())),
        ("body".into(), Value::Str("alpha alpha alpha".into())),
    ])
    .unwrap();

    let results = col.search_text("body", "alpha", 10).unwrap();
    assert_eq!(
        titles(&results),
        vec!["later", "original"],
        "a document indexed incrementally must rank against a backfilled one"
    );
}

#[test]
fn searching_a_field_with_no_index_is_an_error() {
    let db = db_with_articles(&[("a", "alpha")]);
    let col = db.collection("articles").unwrap();

    assert!(matches!(
        col.search_text("title", "alpha", 10),
        Err(TalaDbError::IndexNotFound(_))
    ));
}

#[test]
fn dropping_the_index_clears_its_statistics() {
    let db = db_with_articles(&[("a", "alpha beta"), ("b", "alpha gamma")]);
    let col = db.collection("articles").unwrap();
    assert_eq!(col.search_text("body", "alpha", 10).unwrap().len(), 2);

    col.drop_fts_index("body").unwrap();
    col.create_fts_index("body").unwrap();

    // Rebuilt from scratch: still two documents, not four, and scores are
    // finite (a leaked doc_count would skew every IDF).
    let results = col.search_text("body", "alpha", 10).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.score.is_finite()));
}

#[test]
fn contains_filter_still_requires_every_token() {
    // Ranking is additive OR; the existing filter semantics must not change.
    let db = db_with_articles(&[("both", "alpha beta"), ("one", "alpha only")]);
    let col = db.collection("articles").unwrap();

    let filtered = col
        .find(taladb_core::query::Filter::Contains(
            "body".into(),
            "alpha beta".into(),
        ))
        .unwrap();
    assert_eq!(filtered.len(), 1, "$contains is AND across tokens");

    let ranked = col.search_text("body", "alpha beta", 10).unwrap();
    assert_eq!(ranked.len(), 2, "search_text is OR across tokens");
}
