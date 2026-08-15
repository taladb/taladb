//! Querying array fields by element.
//!
//! `$push`/`$pull` have always maintained array fields, but no query could look
//! inside one: comparisons ran against the array as a single value, so a field
//! holding `['rust', 'db']` matched nothing except an identical list. The
//! failure was silent — an empty result set, never an error.
//!
//! The rule, as in MongoDB: a scalar compared against an array field matches
//! when *any element* satisfies the comparison, and the whole-array comparison
//! is still tried first so exact-list filters keep working.
//!
//! Each test runs twice, unindexed and indexed. That pairing is the point: the
//! b-tree stores one entry per element (`encode_index_keys`), and if it did not,
//! adding an index would silently shrink the result set.

use taladb_core::document::Value;
use taladb_core::{Database, Filter};

fn arr(items: &[&str]) -> Value {
    Value::Array(items.iter().map(|s| Value::Str((*s).into())).collect())
}

fn nums(items: &[i64]) -> Value {
    Value::Array(items.iter().map(|n| Value::Int(*n)).collect())
}

/// Seed a collection, optionally indexing `tags` and `scores`.
fn seeded(indexed: bool) -> taladb_core::Collection {
    let db = Box::leak(Box::new(Database::open_in_memory().unwrap()));
    let col = db.collection("posts").unwrap();
    if indexed {
        col.create_index("tags").unwrap();
        col.create_index("scores").unwrap();
    }
    col.insert(vec![
        ("title".into(), Value::Str("a".into())),
        ("tags".into(), arr(&["rust", "db"])),
        ("scores".into(), nums(&[1, 5, 9])),
    ])
    .unwrap();
    col.insert(vec![
        ("title".into(), Value::Str("b".into())),
        ("tags".into(), arr(&["db", "wasm"])),
        ("scores".into(), nums(&[2, 3])),
    ])
    .unwrap();
    col.insert(vec![
        ("title".into(), Value::Str("c".into())),
        ("tags".into(), Value::Str("rust".into())), // a scalar, not a list
        ("scores".into(), Value::Int(7)),
    ])
    .unwrap();
    col.insert(vec![("title".into(), Value::Str("d".into()))]) // no tags at all
        .unwrap();
    col
}

fn titles(docs: &[taladb_core::Document]) -> Vec<String> {
    let mut out: Vec<String> = docs
        .iter()
        .map(|d| match d.get("title") {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        })
        .collect();
    out.sort();
    out
}

/// Run `f` against both an unindexed and an indexed collection, requiring the
/// same answer from each.
fn both(f: impl Fn(&taladb_core::Collection) -> Vec<String>, expected: &[&str]) {
    let want: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(f(&seeded(false)), want, "unindexed");
    assert_eq!(f(&seeded(true)), want, "indexed");
}

#[test]
fn equality_matches_an_element() {
    both(
        |col| titles(&col.find(Filter::Eq("tags".into(), Value::Str("rust".into()))).unwrap()),
        // 'a' has it in a list, 'c' has it as a scalar.
        &["a", "c"],
    );
}

#[test]
fn equality_still_matches_the_whole_array() {
    both(
        |col| titles(&col.find(Filter::Eq("tags".into(), arr(&["rust", "db"]))).unwrap()),
        &["a"],
    );
}

#[test]
fn in_matches_any_element_against_any_candidate() {
    both(
        |col| {
            titles(
                &col.find(Filter::In(
                    "tags".into(),
                    vec![Value::Str("wasm".into()), Value::Str("swift".into())],
                ))
                .unwrap(),
            )
        },
        &["b"],
    );
}

#[test]
fn ne_excludes_a_document_any_element_of_which_matches() {
    both(
        |col| titles(&col.find(Filter::Ne("tags".into(), Value::Str("db".into()))).unwrap()),
        // 'a' and 'b' both carry 'db'; 'd' has no field at all, which `$ne`
        // has always matched.
        &["c", "d"],
    );
}

#[test]
fn range_comparisons_match_any_element() {
    both(
        |col| titles(&col.find(Filter::Gte("scores".into(), Value::Int(7))).unwrap()),
        // 'a' has a 9, 'c' is the scalar 7. 'b' tops out at 3.
        &["a", "c"],
    );
}

#[test]
fn a_document_is_returned_once_however_many_elements_match() {
    // Three of `a`'s scores are in range. Each is its own index entry, so
    // without deduplication the document comes back three times.
    both(
        |col| titles(&col.find(Filter::Gte("scores".into(), Value::Int(0))).unwrap()),
        &["a", "b", "c"],
    );
}

#[test]
fn count_agrees_with_find() {
    for indexed in [false, true] {
        let col = seeded(indexed);
        let filter = Filter::Eq("tags".into(), Value::Str("db".into()));
        assert_eq!(
            col.count(filter.clone()).unwrap(),
            col.find(filter).unwrap().len() as u64,
            "indexed = {indexed}"
        );
    }
}

#[test]
fn updates_keep_the_index_in_step_with_the_document() {
    let col = seeded(true);
    col.update_one(
        Filter::Eq("title".into(), Value::Str("b".into())),
        taladb_core::Update::Set(vec![("tags".into(), arr(&["swift"]))]),
    )
    .unwrap();

    // The old elements must no longer be reachable...
    assert_eq!(
        titles(&col.find(Filter::Eq("tags".into(), Value::Str("wasm".into()))).unwrap()),
        Vec::<String>::new(),
    );
    // ...and the new one must be.
    assert_eq!(
        titles(&col.find(Filter::Eq("tags".into(), Value::Str("swift".into()))).unwrap()),
        vec!["b".to_string()],
    );
    // A document still carrying 'db' is untouched.
    assert_eq!(
        titles(&col.find(Filter::Eq("tags".into(), Value::Str("db".into()))).unwrap()),
        vec!["a".to_string()],
    );
}

#[test]
fn deletes_remove_every_element_entry() {
    let col = seeded(true);
    col.delete_one(Filter::Eq("title".into(), Value::Str("a".into())))
        .unwrap();
    assert_eq!(
        titles(&col.find(Filter::Eq("tags".into(), Value::Str("rust".into()))).unwrap()),
        vec!["c".to_string()],
        "a deleted document must not linger under any of its elements"
    );
}

#[test]
fn an_index_created_after_the_writes_backfills_every_element() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("posts").unwrap();
    col.insert(vec![
        ("title".into(), Value::Str("a".into())),
        ("tags".into(), arr(&["rust", "db"])),
    ])
    .unwrap();
    col.create_index("tags").unwrap();

    assert_eq!(
        titles(&col.find(Filter::Eq("tags".into(), Value::Str("db".into()))).unwrap()),
        vec!["a".to_string()],
    );
}

#[test]
fn nested_arrays_are_not_flattened() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("posts").unwrap();
    col.insert(vec![
        ("title".into(), Value::Str("a".into())),
        ("tags".into(), Value::Array(vec![arr(&["deep"])])),
    ])
    .unwrap();

    // One level only — the same depth `encode_index_keys` indexes, so an
    // indexed and an unindexed collection cannot disagree.
    assert!(
        col.find(Filter::Eq("tags".into(), Value::Str("deep".into())))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn compound_index_expands_one_array_member() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("posts").unwrap();
    col.create_compound_index(&["author", "tags"])
        .unwrap();
    col.insert(vec![
        ("title".into(), Value::Str("a".into())),
        ("author".into(), Value::Str("ana".into())),
        ("tags".into(), arr(&["rust", "db"])),
    ])
    .unwrap();

    let hits = col
        .find(Filter::And(vec![
            Filter::Eq("author".into(), Value::Str("ana".into())),
            Filter::Eq("tags".into(), Value::Str("db".into())),
        ]))
        .unwrap();
    assert_eq!(
        titles(&hits),
        vec!["a".to_string()],
        "a compound plan must reach an array member, or adding the index would \
         silently shrink the result set"
    );
}

#[test]
fn a_compound_index_refuses_two_array_members() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("posts").unwrap();
    col.create_compound_index(&["tags", "scores"])
        .unwrap();

    let err = col
        .insert(vec![
            ("tags".into(), arr(&["rust", "db"])),
            ("scores".into(), nums(&[1, 2])),
        ])
        .unwrap_err();
    assert!(
        matches!(err, taladb_core::TalaDbError::CompoundIndexMultipleArrays),
        "the key count would be the product of the two lists, got {err:?}"
    );
}
