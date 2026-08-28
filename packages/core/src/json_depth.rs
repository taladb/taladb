//! Depth guard for untrusted JSON, applied before any recursive parser walks it.
//!
//! Every binding converts caller-supplied JSON into a [`crate::Filter`], an
//! [`crate::collection::Update`] or a [`crate::Value`], and every one of those
//! converters is recursive — `$and`/`$or`/`$not` nest, and so do arrays and
//! objects. Recursion driven by caller-controlled nesting is a stack overflow,
//! which aborts the process and cannot be caught: no `catch_unwind`, no
//! `Result`, and in wasm an unrecoverable trap that takes the database handle
//! with it.
//!
//! # Why this exists when serde_json already has a limit
//!
//! `serde_json` caps *parsing* at 128 layers, so any path that starts from a
//! string is already covered. The gap is the paths that receive a value someone
//! else built:
//!
//! - the browser binding, where `serde_wasm_bindgen::from_value` walks a live JS
//!   object graph and imposes no limit of its own;
//! - the Node binding, where napi converts a JS value into `serde_json::Value`.
//!
//! Those two hand a fully-formed `serde_json::Value` to the same recursive
//! converters, having never been through `serde_json`'s parser.
//!
//! # Why it is iterative
//!
//! A recursive depth-checker would overflow on exactly the input it is meant to
//! reject, so this walks an explicit stack. It cannot overflow regardless of
//! input, which is the only way the guard is worth having.
//!
//! # What this does not fix
//!
//! It bounds what reaches the *converters*. It does not make a binding immune to
//! a stack overflow, because two recursions sit outside it and neither is ours:
//!
//! - **Building the value.** `serde_wasm_bindgen` and napi deserialize a nested
//!   JS object by recursing into it, so a sufficiently deep input overflows
//!   before this function is ever called.
//! - **Dropping the value.** `serde_json::Value` has a recursive `Drop`, so
//!   returning the error below drops the offending value and can overflow there
//!   instead. Measured on a default 8 MiB main-thread stack: a 30,000-deep value
//!   builds, checks and drops cleanly, while 50,000 survives construction and the
//!   check and then dies in the drop.
//!
//! Both thresholds are tens of thousands of levels deep, far above anything this
//! guard rejects at 64 — so in the range a caller can plausibly reach by accident
//! or by hand-written query, the guard is the thing that fires, and it fires as a
//! clean error. Closing the extreme case means not materialising a
//! `serde_json::Value` at the boundary at all, which is a larger change to the
//! browser and Node bindings than this module.

use crate::error::TalaDbError;

/// Maximum nesting accepted in caller-supplied JSON.
///
/// Matches the 64-level ceiling `crate::document::value_get_nested_depth` already
/// applies when resolving a dotted field path: a document nested deeper than that
/// has parts the engine will not reach anyway, so accepting it would only store
/// something unqueryable. Real filters and documents are a handful of levels
/// deep; anything approaching this is a generated payload, not a query someone
/// wrote.
pub const MAX_JSON_DEPTH: usize = 64;

/// Reject `value` if its arrays and objects nest deeper than [`MAX_JSON_DEPTH`].
///
/// Call this at the boundary where untrusted JSON arrives, before handing it to a
/// recursive converter.
///
/// # Errors
///
/// Returns [`TalaDbError::InvalidFilter`] naming the limit when the value is too
/// deeply nested.
///
/// # Examples
///
/// ```
/// use taladb_core::json_depth::{MAX_JSON_DEPTH, check_json_depth};
///
/// let shallow = serde_json::json!({ "a": [1, { "b": 2 }] });
/// assert!(check_json_depth(&shallow).is_ok());
///
/// // Build something past the ceiling without recursing.
/// let mut deep = serde_json::Value::Null;
/// for _ in 0..MAX_JSON_DEPTH + 1 {
///     deep = serde_json::Value::Array(vec![deep]);
/// }
/// assert!(check_json_depth(&deep).is_err());
/// ```
pub fn check_json_depth(value: &serde_json::Value) -> Result<(), TalaDbError> {
    // (node, depth-of-that-node). Depth 1 is the value itself.
    let mut stack: Vec<(&serde_json::Value, usize)> = vec![(value, 1)];

    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_JSON_DEPTH {
            return Err(TalaDbError::InvalidFilter(format!(
                "JSON nested deeper than {MAX_JSON_DEPTH} levels"
            )));
        }
        match node {
            serde_json::Value::Array(items) => {
                for item in items {
                    stack.push((item, depth + 1));
                }
            }
            serde_json::Value::Object(map) => {
                for (_, v) in map {
                    stack.push((v, depth + 1));
                }
            }
            // Scalars end a branch.
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build `depth` levels of nesting iteratively — a recursive builder would
    /// overflow constructing the very input these tests need.
    fn nest_arrays(depth: usize) -> serde_json::Value {
        let mut v = serde_json::Value::Null;
        for _ in 1..depth {
            v = serde_json::Value::Array(vec![v]);
        }
        v
    }

    fn nest_objects(depth: usize) -> serde_json::Value {
        let mut v = serde_json::Value::Null;
        for _ in 1..depth {
            let mut map = serde_json::Map::new();
            map.insert("k".to_string(), v);
            v = serde_json::Value::Object(map);
        }
        v
    }

    #[test]
    fn accepts_ordinary_shapes() {
        for v in [
            serde_json::json!(null),
            serde_json::json!(1),
            serde_json::json!("s"),
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!({ "status": "active", "tags": ["a", "b"] }),
            serde_json::json!({ "$or": [{ "a": 1 }, { "$not": { "b": 2 } }] }),
        ] {
            assert!(check_json_depth(&v).is_ok(), "rejected {v}");
        }
    }

    #[test]
    fn the_boundary_is_exact() {
        assert!(check_json_depth(&nest_arrays(MAX_JSON_DEPTH)).is_ok());
        assert!(check_json_depth(&nest_arrays(MAX_JSON_DEPTH + 1)).is_err());
        assert!(check_json_depth(&nest_objects(MAX_JSON_DEPTH)).is_ok());
        assert!(check_json_depth(&nest_objects(MAX_JSON_DEPTH + 1)).is_err());
    }

    /// The whole point: the checker must not consume stack proportional to the
    /// input's depth, or it overflows on exactly the input it exists to reject.
    ///
    /// Run on a deliberately small stack rather than by piling on depth. A
    /// depth-based test has to guess where the default 8 MB stack gives out — and
    /// that boundary moves with the build profile and the platform, so it is
    /// either flaky or so extreme it breaks the *construction* of the test input.
    /// 10,000 frames cannot fit in 256 KiB, so a recursive implementation fails
    /// here deterministically while an iterative one is unaffected.
    #[test]
    fn does_not_consume_stack_proportional_to_depth() {
        let deep = nest_arrays(10_000);
        let checked = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let rejected = check_json_depth(&deep).is_err();
                // Dismantle iteratively on this small stack too: `serde_json`'s
                // own `Drop` is recursive, and letting `deep` fall out of scope
                // here would overflow on the drop rather than the check.
                let mut node = deep;
                while let serde_json::Value::Array(mut items) = node {
                    node = items.pop().unwrap_or(serde_json::Value::Null);
                }
                rejected
            })
            .expect("spawn")
            .join();

        match checked {
            Ok(rejected) => assert!(rejected, "a 10,000-deep value must be rejected"),
            Err(_) => panic!(
                "the depth check overflowed a 256 KiB stack — it is recursing over \
                 the input instead of walking an explicit stack"
            ),
        }
    }

    #[test]
    fn depth_is_measured_down_a_branch_not_across_siblings() {
        // 10,000 siblings, each shallow: wide is not deep.
        let wide = serde_json::Value::Array((0..10_000).map(serde_json::Value::from).collect());
        assert!(
            check_json_depth(&wide).is_ok(),
            "a wide-but-shallow value must be accepted"
        );
    }

    #[test]
    fn the_error_names_the_limit() {
        let err = check_json_depth(&nest_arrays(MAX_JSON_DEPTH + 1)).expect_err("must reject");
        assert!(
            err.to_string().contains(&MAX_JSON_DEPTH.to_string()),
            "the message should say what the limit is: {err}"
        );
    }
}
