//! A regex that will not compile must be an error, never a silent non-match.
//!
//! A pattern's *compiled* size is caller-controlled — `(?:[a-z0-9]{64}){64}` is
//! a few dozen source bytes and megabytes of automaton — so the engine caps it
//! at 1 MiB. The question is what happens when a pattern hits that cap.
//!
//! It used to be `return false`, in two places. And `false` is the one answer
//! that cannot be safely guessed, because `Filter::Not` inverts it: "matched
//! nothing" becomes "matched everything", so an exclusion filter silently
//! returned the entire collection instead of erroring.
//!
//! The query path was already safe — `find()` validates the whole filter tree
//! through `compile_regex_cache()?` before scanning — but `Filter::matches` is
//! public API and had no such protection. These tests pin both.

use std::collections::HashMap;

use taladb_core::aggregate::Stage;
use taladb_core::document::Value;
use taladb_core::{Database, Filter};

/// Valid syntax; compiles to an automaton far past the 1 MiB ceiling.
fn oversized_pattern() -> String {
    "(?:[a-z0-9]{64}){64}".repeat(40)
}

fn db_with_two_docs() -> Database {
    let db = Database::open_in_memory().expect("open");
    let col = db.collection("t").expect("collection");
    for name in ["alpha", "beta"] {
        col.insert(vec![("name".to_string(), Value::Str(name.into()))])
            .expect("insert");
    }
    db
}

// ---------------------------------------------------------------------------
// The public evaluator
// ---------------------------------------------------------------------------

#[test]
fn matches_reports_an_oversized_pattern_instead_of_returning_false() {
    let db = db_with_two_docs();
    let col = db.collection("t").expect("collection");
    let doc = &col.find(Filter::All).expect("find")[0];

    let err = Filter::Regex("name".into(), oversized_pattern())
        .matches(doc)
        .expect_err("an uncompilable pattern must be an error");
    assert!(
        err.to_string().contains("regex"),
        "the message should say it was the regex: {err}"
    );
}

/// The regression that motivated all of this: `!false` is `true`.
#[test]
fn a_failed_regex_under_not_does_not_match_everything() {
    let db = db_with_two_docs();
    let col = db.collection("t").expect("collection");
    let doc = &col.find(Filter::All).expect("find")[0];

    let excluded = Filter::Not(Box::new(Filter::Regex("name".into(), oversized_pattern())));
    assert!(
        excluded.matches(doc).is_err(),
        "a $not around a failing regex must error, not report a match — \
         silently returning `false` inside `Not` inverts to `true` and turns an \
         exclusion filter into 'match everything'"
    );
}

/// A cache built from a *different* filter is a programming error, and used to
/// read as "this document does not match".
#[test]
fn a_mismatched_regex_cache_is_an_error_not_a_non_match() {
    let db = db_with_two_docs();
    let col = db.collection("t").expect("collection");
    let doc = &col.find(Filter::All).expect("find")[0];

    // Type inferred from `matches_with_cache`'s signature, so this test needs no
    // direct dependency on `regex`.
    let empty = HashMap::new();
    let filter = Filter::Regex("name".into(), "^alpha$".into());
    assert!(
        filter.matches_with_cache(doc, &empty).is_err(),
        "a cache missing the filter's own pattern must be reported"
    );

    // And the same shape under Not, where a silent false was actively wrong.
    let negated = Filter::Not(Box::new(filter));
    assert!(negated.matches_with_cache(doc, &empty).is_err());
}

/// The compiling and cached evaluators must agree.
///
/// They were two separate implementations of the same logic until now, which is
/// how they came to disagree about failure in the first place. `find()` goes
/// through the cached path; `Filter::matches` is the compiling one. Comparing
/// their answers over the same documents exercises both without needing to build
/// a cache by hand.
#[test]
fn the_cached_and_compiling_evaluators_agree() {
    let db = db_with_two_docs();
    let col = db.collection("t").expect("collection");
    let all = col.find(Filter::All).expect("find all");

    for filter in [
        Filter::Regex("name".into(), "^alpha$".into()),
        Filter::Not(Box::new(Filter::Regex("name".into(), "^alpha$".into()))),
        Filter::And(vec![
            Filter::Regex("name".into(), "^a".into()),
            Filter::Exists("name".into(), true),
        ]),
        Filter::Or(vec![
            Filter::Regex("name".into(), "^zzz".into()),
            Filter::Regex("name".into(), "beta".into()),
        ]),
    ] {
        // Cached path, via the query engine.
        let mut via_find: Vec<String> = col
            .find(filter.clone())
            .expect("find")
            .iter()
            .map(|d| d.id.to_string())
            .collect();
        via_find.sort();

        // Compiling path, evaluated directly.
        let mut via_matches: Vec<String> = all
            .iter()
            .filter(|d| filter.matches(d).expect("matches"))
            .map(|d| d.id.to_string())
            .collect();
        via_matches.sort();

        assert_eq!(
            via_find, via_matches,
            "the cached and compiling evaluators disagreed on {filter:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The query path — already safe, pinned so it stays that way
// ---------------------------------------------------------------------------

#[test]
fn find_rejects_an_oversized_pattern() {
    let db = db_with_two_docs();
    let col = db.collection("t").expect("collection");
    assert!(
        col.find(Filter::Regex("name".into(), oversized_pattern()))
            .is_err(),
        "find() must refuse a filter it cannot honour"
    );
}

#[test]
fn find_rejects_an_oversized_pattern_under_not() {
    let db = db_with_two_docs();
    let col = db.collection("t").expect("collection");
    let excluded = Filter::Not(Box::new(Filter::Regex("name".into(), oversized_pattern())));
    let result = col.find(excluded);
    assert!(
        result.is_err(),
        "find() must not answer 'everything' for an exclusion it could not evaluate; \
         got {:?} documents",
        result.map(|docs| docs.len())
    );
}

/// Both `$match` and `find()` must reject a pattern neither can compile.
///
/// Note what this does *not* prove. `parse_pipeline` validation and executor
/// compilation used different ceilings — 10 MiB against 1 MiB — so a pattern
/// between them passed validation and then failed at execution. Both endings are
/// an `Err`, so no black-box test distinguishes them; routing both through the
/// same `build_regex` is defence against future drift, not a behaviour change
/// this test can observe. What it does pin is that the two entry points agree
/// on rejection at all.
#[test]
fn aggregate_match_rejects_what_execution_would_reject() {
    let db = db_with_two_docs();
    let col = db.collection("t").expect("collection");
    // A leading $match is consumed by the index planner, so a second stage goes
    // first to force the pattern through `execute_pipeline`.
    let bad = Filter::Regex("name".into(), oversized_pattern());
    assert!(
        col.aggregate(vec![Stage::Skip(0), Stage::Match(bad.clone())])
            .is_err(),
        "an aggregation stage must refuse a pattern the executor cannot compile"
    );
    // And find() must agree — the divergence was that validation used a 10 MiB
    // ceiling while execution used 1 MiB.
    assert!(col.find(bad).is_err(), "find() must refuse it too");
}

#[test]
fn a_valid_regex_query_still_works() {
    let db = db_with_two_docs();
    let col = db.collection("t").expect("collection");
    let hits = col
        .find(Filter::Regex("name".into(), "^alpha$".into()))
        .expect("find");
    assert_eq!(hits.len(), 1);

    let excluded = col
        .find(Filter::Not(Box::new(Filter::Regex(
            "name".into(),
            "^alpha$".into(),
        ))))
        .expect("find");
    assert_eq!(excluded.len(), 1, "$not must still exclude exactly one");
}
