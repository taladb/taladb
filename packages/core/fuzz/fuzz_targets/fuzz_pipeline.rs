#![no_main]
use libfuzzer_sys::fuzz_target;

// Fuzz the aggregation pipeline parser.
//
// `parse_pipeline` takes caller-supplied JSON directly and walks it: stage
// objects, `$group` accumulators, `$sort` keys, and a `$match` filter handed to
// a caller-supplied parser. It also runs `validate_regexes`, which is where the
// 1 MiB pattern ceiling is enforced — the check that used to disagree with the
// executor's.
fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if text.len() > 64 * 1024 {
        return;
    }
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return;
    };

    // The depth guard runs at the binding boundary before a pipeline reaches
    // here, so mirror that ordering rather than fuzzing a state the engine
    // never sees.
    if taladb_core::json_depth::check_json_depth(&json).is_err() {
        return;
    }

    let _ = taladb_core::aggregate::parse_pipeline(&json, &|v| {
        taladb_ffi::fuzz_api::parse_filter(&v.to_string())
    });
});
