//! `$match` regex handling in the aggregation pipeline.
//!
//! `$match` used to evaluate through `Filter::matches`, which builds a fresh
//! automaton per document and ends in `unwrap_or(false)`. The performance cost
//! was the obvious half; the silent half mattered more: a pattern the regex
//! crate refuses to compile matched *nothing* here, while the identical filter
//! through `find()` returned `InvalidFilter`. Same query, two answers, and the
//! wrong one looks like a legitimately empty result set.

use taladb_core::Database;
use taladb_core::aggregate::Stage;
use taladb_core::document::Value;
use taladb_core::query::Filter;

fn db_with_emails() -> Database {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("users").unwrap();
    for addr in [
        "alice@example.com",
        "bob@example.com",
        "carol@other.org",
        "dave@elsewhere.net",
    ] {
        col.insert(vec![("email".into(), Value::Str(addr.into()))])
            .unwrap();
    }
    db
}

/// A pattern the regex crate rejects must surface as an error, not as "no
/// documents matched".
#[test]
fn invalid_regex_in_match_errors_instead_of_matching_nothing() {
    let db = db_with_emails();
    let col = db.collection("users").unwrap();

    // Unclosed character class — a compile failure, not a pattern that matches
    // nothing.
    let bad = Filter::Regex("email".into(), "[unclosed".into());

    let via_find = col.find(bad.clone());
    assert!(
        via_find.is_err(),
        "find() must reject an uncompilable pattern"
    );

    let via_aggregate = col.aggregate(vec![
        // A leading $match is consumed by the index planner, so put a second
        // stage first to force the regex through `execute_pipeline`.
        Stage::Skip(0),
        Stage::Match(bad),
    ]);
    assert!(
        via_aggregate.is_err(),
        "$match must reject the same pattern find() rejects, not return an empty result"
    );
}

/// The two paths must agree on which documents match.
#[test]
fn match_regex_agrees_with_find() {
    let db = db_with_emails();
    let col = db.collection("users").unwrap();
    let filter = Filter::Regex("email".into(), r"@example\.com$".into());

    let via_find = col.find(filter.clone()).unwrap();
    let via_aggregate = col
        .aggregate(vec![Stage::Skip(0), Stage::Match(filter)])
        .unwrap();

    assert_eq!(via_find.len(), 2, "two @example.com addresses");
    assert_eq!(
        via_find.len(),
        via_aggregate.len(),
        "$match and find() must select the same documents"
    );
}

/// Several `$match` stages in one pipeline each get their patterns compiled.
#[test]
fn multiple_match_stages_each_have_their_regex_compiled() {
    let db = db_with_emails();
    let col = db.collection("users").unwrap();

    let results = col
        .aggregate(vec![
            Stage::Skip(0),
            Stage::Match(Filter::Regex("email".into(), r"\.com$".into())),
            Stage::Match(Filter::Regex("email".into(), r"^alice".into())),
        ])
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].get("email"),
        Some(&Value::Str("alice@example.com".into()))
    );
}

/// An invalid pattern in a *later* stage must fail the pipeline too — the cache
/// is built from every stage, not just the first.
#[test]
fn invalid_regex_in_a_later_match_stage_still_errors() {
    let db = db_with_emails();
    let col = db.collection("users").unwrap();

    let result = col.aggregate(vec![
        Stage::Skip(0),
        Stage::Match(Filter::Regex("email".into(), r"\.com$".into())),
        Stage::Match(Filter::Regex("email".into(), "(unbalanced".into())),
    ]);

    assert!(
        result.is_err(),
        "a bad pattern anywhere must fail the pipeline"
    );
}
