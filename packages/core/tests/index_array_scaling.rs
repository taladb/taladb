//! Index-key encoding must stay linear in the length of an indexed array.
//!
//! `encode_index_keys` and `encode_compound_keys` deduplicate the keys an array
//! field contributes. Both used `Vec::contains` inside the loop over elements — a
//! linear scan inside a linear loop — so a single insert cost time proportional
//! to the *square* of the array length, and that length comes from the caller.
//! On a debug build one document went from 170 ms at 2,000 elements to 4.5 s at
//! 16,000: in a browser or on a phone, a frozen UI produced by one write.
//!
//! These tests call the encoders directly rather than timing `insert`. Going
//! through storage adds a large fixed cost that dilutes the ratio — measured
//! end-to-end, the quadratic version scored only ~8x on a 4x input growth, close
//! enough to linear's ~4x that no honest threshold separates them. Isolating the
//! encoder removes the I/O and leaves the growth curve itself, where the two are
//! far apart.

use taladb_core::index::{encode_compound_keys, encode_index_keys};
use taladb_core::{Database, Filter, Ulid, document::Value};

fn array_of(n: usize) -> Value {
    Value::Array((0..n).map(|i| Value::Int(i as i64)).collect())
}

/// Wall-clock for encoding one array of `n` distinct elements. Repeated so a
/// single scheduling hiccup cannot decide the outcome.
fn encode_cost(n: usize, compound: bool) -> std::time::Duration {
    let id = Ulid::new();
    let arr = array_of(n);
    let tenant = Value::Str("acme".into());
    let start = std::time::Instant::now();
    for _ in 0..3 {
        if compound {
            let values: Vec<&Value> = vec![&tenant, &arr];
            let keys = encode_compound_keys(&values, id).expect("encode");
            assert_eq!(keys.len(), n);
        } else {
            let keys = encode_index_keys(&arr, id);
            assert_eq!(keys.len(), n);
        }
    }
    start.elapsed()
}

/// 8x the elements must cost roughly 8x, not roughly 64x.
fn assert_linear(compound: bool, label: &str) {
    // Warm any lazy allocator behaviour so the first measurement isn't special.
    let _ = encode_cost(1_000, compound);

    let small = encode_cost(2_000, compound);
    let large = encode_cost(16_000, compound);
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(1e-9);
    println!("{label}: 2k={small:?}  16k={large:?}  ratio={ratio:.1}x  (linear≈8x, quadratic≈64x)");

    // The midpoint between 8 and 64 on a log scale is ~23; 20 leaves generous
    // headroom for a loaded runner while staying far below quadratic.
    assert!(
        ratio < 20.0,
        "{label}: 8x the elements cost {ratio:.1}x the time — key dedup has gone \
         quadratic in the array length again (2k={small:?}, 16k={large:?})"
    );
}

#[test]
fn single_field_index_keys_scale_linearly() {
    assert_linear(false, "single-field");
}

#[test]
fn compound_index_keys_scale_linearly() {
    assert_linear(true, "compound");
}

/// The dedup itself must still work. Repeated elements encode identically, and
/// `count_plan_entries` relies on one index entry per *distinct* element, so
/// losing this would double-count documents rather than merely slow them down.
#[test]
fn duplicate_elements_still_collapse_to_one_key() {
    let id = Ulid::new();
    let dupes = Value::Array(vec![
        Value::Str("a".into()),
        Value::Str("a".into()),
        Value::Str("b".into()),
        Value::Str("a".into()),
    ]);
    assert_eq!(
        encode_index_keys(&dupes, id).len(),
        2,
        "['a','a','b','a'] must yield exactly the keys for 'a' and 'b'"
    );

    // And the same rule through the compound encoder.
    let tenant = Value::Str("acme".into());
    let values: Vec<&Value> = vec![&tenant, &dupes];
    assert_eq!(
        encode_compound_keys(&values, id).expect("encode").len(),
        2,
        "compound keys must deduplicate on the array member too"
    );
}

/// Element order is preserved — the output is a `Vec`, not a set, and keeping it
/// deterministic is what makes an index diff readable when debugging.
#[test]
fn key_order_follows_element_order() {
    let id = Ulid::new();
    let ascending = encode_index_keys(&array_of(64), id);
    let mut sorted = ascending.clone();
    sorted.sort();
    assert_eq!(
        ascending, sorted,
        "keys for an ascending Int array must come back in ascending byte order"
    );
}

/// End-to-end: dedup still means one document per matching element, and `count`
/// (which reads index entries directly) agrees with `find`.
#[test]
fn a_repeated_element_matches_its_document_once() {
    let db = Database::open_in_memory().expect("open");
    let col = db.collection("items").expect("collection");
    col.create_index("tags").expect("index");
    col.insert(vec![(
        "tags".to_string(),
        Value::Array(vec![
            Value::Str("a".into()),
            Value::Str("a".into()),
            Value::Str("b".into()),
        ]),
    )])
    .expect("insert");

    let needle = Filter::Eq("tags".into(), Value::Str("a".into()));
    assert_eq!(col.find(needle.clone()).expect("find").len(), 1);
    assert_eq!(
        col.count(needle).expect("count"),
        1,
        "count reads index entries directly, so it must agree with find"
    );
}
