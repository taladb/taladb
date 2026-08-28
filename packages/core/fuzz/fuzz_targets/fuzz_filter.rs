#![no_main]
use libfuzzer_sys::fuzz_target;

// Fuzz filter evaluation against a decoded document.
//
// `Filter::matches` now returns a `Result`, so the interesting property is
// sharper than "does not panic": an infallible filter — one with no `Regex`
// arm — must never report an error. Only a pattern that will not compile can
// produce one, so an `Err` here means evaluation invented a failure, and a
// panic means it found an input the type system did not rule out.
//
// This target is narrow by construction: arbitrary bytes rarely decode into a
// valid `Document`, so most inputs exercise only postcard. That is why the
// parsers get their own targets — `fuzz_filter_parser` and `fuzz_update_parser`
// take a JSON string, which is what the bindings actually receive, so the
// fuzzer spends its budget on operator handling instead of on a binary decoder.
fuzz_target!(|data: &[u8]| {
    if let Ok(doc) = postcard::from_bytes::<taladb_core::Document>(data) {
        use taladb_core::{Filter, Value};

        for filter in [
            Filter::All,
            Filter::Eq("_id".into(), Value::Str("x".into())),
            Filter::Exists("_id".into(), true),
            // Composition, so the And/Or/Not recursion is exercised rather than
            // just the leaves.
            Filter::Not(Box::new(Filter::Eq("k".into(), Value::Int(0)))),
            Filter::And(vec![
                Filter::Exists("k".into(), true),
                Filter::Not(Box::new(Filter::Eq("k".into(), Value::Null))),
            ]),
        ] {
            match filter.matches(&doc) {
                Ok(_) => {}
                Err(e) => panic!("a regex-free filter must not fail to evaluate: {e}"),
            }
        }
    }
});
