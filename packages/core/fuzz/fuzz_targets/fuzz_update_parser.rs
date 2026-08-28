#![no_main]
use libfuzzer_sys::fuzz_target;

// Fuzz the JSON→Update parser.
//
// Updates are the other direction untrusted structure travels, and they carry
// the arithmetic operators (`$inc`) and the array operators (`$push`/`$pull`)
// that filters do not. A malformed update that parses into something the engine
// then applies is worse than one that fails to parse.
fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if text.len() > 64 * 1024 {
        return;
    }

    let first = taladb_ffi::fuzz_api::parse_update(text);
    let second = taladb_ffi::fuzz_api::parse_update(text);
    assert_eq!(
        first.is_some(),
        second.is_some(),
        "parse_update disagreed with itself on the same input: {text:?}"
    );

    // Documents travel the same boundary and share `json_to_value`.
    let _ = taladb_ffi::fuzz_api::json_to_fields(text);
});
