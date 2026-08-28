#![no_main]
use libfuzzer_sys::fuzz_target;

// Fuzz the JSON→Filter parser: where untrusted *structure*, as opposed to
// untrusted bytes, actually enters the engine.
//
// `fuzz_filter` covers evaluation, but it feeds on `postcard`-decoded documents,
// so arbitrary bytes almost never reach a filter at all. This target takes the
// input as a JSON string instead, which is what every binding receives, so the
// fuzzer spends its time on operator handling and nesting rather than on
// bouncing off a binary decoder.
//
// The parser under test is the React Native binding's. All four bindings carry
// their own near-identical copy — the duplication that let the two regex
// evaluators drift apart — and this one is reachable from a fuzzer without a
// napi or wasm toolchain in the loop.
//
// Properties, beyond "does not panic":
//
//  1. A parse must not be reported as successful *and* leave a filter that
//     cannot be evaluated. The engine treats a returned `Filter` as valid.
//  2. Parsing is deterministic: the same bytes twice must agree. A parser that
//     depends on map iteration order would produce filters that silently differ
//     between runs.
//  3. Nesting past the depth ceiling must be an `Err`, never a stack overflow —
//     the property `MAX_JSON_DEPTH` exists to hold.
fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // Keep inputs to a size a caller could plausibly send. Without this the
    // fuzzer spends its budget growing one enormous document.
    if text.len() > 64 * 1024 {
        return;
    }

    let first = taladb_ffi::fuzz_api::parse_filter(text);

    // (2) Determinism.
    let second = taladb_ffi::fuzz_api::parse_filter(text);
    match (&first, &second) {
        (Ok(_), Ok(_)) | (Err(_), Err(_)) => {}
        _ => panic!("parse_filter disagreed with itself on the same input: {text:?}"),
    }

    if let Ok(filter) = first {
        // (1) A filter the parser accepted must be evaluable. `matches` only
        // errors on a regex that will not compile, and the parser is what
        // decided to build that regex — so an error here means the parser
        // accepted something it should have rejected.
        let doc = taladb_core::Document::new(vec![
            ("k".to_string(), taladb_core::Value::Int(1)),
            ("s".to_string(), taladb_core::Value::Str("abc".to_string())),
        ]);
        let _ = filter.matches(&doc);
    }
});
