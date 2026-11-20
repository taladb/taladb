//! Depth-bounded conversion of JS arguments into `serde_json::Value`.
//!
//! # Why this exists
//!
//! napi-rs's `FromNapiValue for serde_json::Value` (feature `serde-json`) walks
//! the live JS value recursively, one native stack frame group per level, with
//! no depth limit. A JS value nested about 10,000 levels deep — one
//! `JSON.parse` of a ~50 KB request body — overflows the main-thread stack
//! inside that walk and kills the process with `SIGSEGV`. It happens while napi
//! is still building the argument, before any TalaDB code runs, so the 64-level
//! [`taladb_core::json_depth::check_json_depth`] guard behind it never gets the
//! chance to refuse the input. A self-referencing object (`a.self = a`) never
//! ends at all and crashes the same way.
//!
//! [`JsonValue`] replaces `serde_json::Value` in every exported argument
//! position. Its conversion makes the same N-API calls in the same order as
//! napi-rs's (so key order, `undefined` handling, number conversion and every
//! error message for a value it accepts are unchanged) and counts the depth on
//! the way down, refusing the value at the first object or array past
//! [`MAX_INPUT_DEPTH`] instead of recursing into it.
//!
//! # Why the limit is not 64
//!
//! [`MAX_INPUT_DEPTH`] bounds the *conversion*; it is not the engine's input
//! rule. The engine's own limits still apply afterwards, with their existing
//! errors: 64 levels for a filter or document (`check_json_depth`) and 128 for
//! a stored value (`taladb_core::value`). Some arguments legitimately wrap a
//! 64-level filter in a few levels of envelope — an `aggregate` pipeline holds a
//! `$match` inside a stage inside an array, an update holds a value inside
//! `$set` — so the conversion limit has to sit above every depth that is
//! accepted today, or valid calls would start failing. 256 is twice the deepest
//! storable value; at that depth the conversion uses well under 1 MiB of stack,
//! so it is safe on a Node worker thread's 4 MiB default as well as the main
//! thread.
//!
//! # The name
//!
//! The type is called `JsonValue`, like the `serde_json::Value` alias it
//! replaces, because napi-derive writes the Rust type's name into the generated
//! `index.d.ts`. Keeping the name keeps the published TypeScript declarations
//! byte-for-byte identical.

use std::ops::Deref;

use napi::bindgen_prelude::{Array, FromNapiValue, Object};
use napi::{JsUnknown, NapiRaw, Status, ValueType, sys};

/// Deepest object/array nesting a JS argument may have before conversion
/// refuses it. See the module docs for why this is 256 and not 64.
pub const MAX_INPUT_DEPTH: usize = 256;

/// A JS argument converted to `serde_json::Value` with bounded recursion.
///
/// Dereferences to the converted `serde_json::Value`.
pub struct JsonValue(serde_json::Value);

impl JsonValue {
    /// The converted value.
    pub fn into_inner(self) -> serde_json::Value {
        self.0
    }
}

/// For Rust callers (and tests) that already hold a `serde_json::Value`: no
/// JS conversion happens, so no depth limit applies here.
impl From<serde_json::Value> for JsonValue {
    fn from(value: serde_json::Value) -> Self {
        Self(value)
    }
}

impl Deref for JsonValue {
    type Target = serde_json::Value;

    fn deref(&self) -> &serde_json::Value {
        &self.0
    }
}

impl FromNapiValue for JsonValue {
    unsafe fn from_napi_value(env: sys::napi_env, napi_val: sys::napi_value) -> napi::Result<Self> {
        // SAFETY: forwarded unchanged from this function's own contract: napi
        // calls it with the env of the current callback and a handle that is
        // valid for that callback's handle scope.
        unsafe { convert(env, napi_val, 1) }.map(Self)
    }
}

fn too_deep() -> napi::Error {
    napi::Error::new(
        Status::InvalidArg,
        format!("invalid argument: value nested deeper than {MAX_INPUT_DEPTH} levels"),
    )
}

/// Convert one JS value at nesting `depth` (1 = the argument itself).
///
/// Mirrors `<serde_json::Value as FromNapiValue>::from_napi_value` in napi 2.16
/// call for call, except that objects and arrays recurse through this function
/// with `depth + 1` and stop at [`MAX_INPUT_DEPTH`].
///
/// # Safety
///
/// `env` must be the env of the current napi callback and `napi_val` a handle
/// that is valid in that callback's current handle scope.
unsafe fn convert(
    env: sys::napi_env,
    napi_val: sys::napi_value,
    depth: usize,
) -> napi::Result<serde_json::Value> {
    use serde_json::Value as J;

    // Every `unsafe` call below passes `env` and either `napi_val` (valid by
    // this function's contract) or a handle napi has just returned for one of
    // its elements or properties, which lives in the same handle scope. That
    // is the whole precondition of each napi function called here.
    let ty = napi::type_of!(env, napi_val)?;
    let val = match ty {
        // SAFETY: see above; `ty` confirms the JS type each leaf conversion expects.
        ValueType::Boolean => J::Bool(unsafe { bool::from_napi_value(env, napi_val) }?),
        // SAFETY: as above.
        ValueType::Number => {
            J::Number(unsafe { serde_json::Number::from_napi_value(env, napi_val) }?)
        }
        // SAFETY: as above.
        ValueType::String => J::String(unsafe { String::from_napi_value(env, napi_val) }?),
        ValueType::Null => J::Null,
        ValueType::Object => {
            // Refuse before reading any property: the check has to come
            // *before* the recursion, not after it.
            if depth > MAX_INPUT_DEPTH {
                return Err(too_deep());
            }
            // `depth <= MAX_INPUT_DEPTH` here, so this cannot overflow.
            let child_depth = depth + 1;

            let mut is_arr = false;
            napi::check_status!(
                // SAFETY: as above; `is_arr` is a live, writable `bool`.
                unsafe { sys::napi_is_array(env, napi_val, &mut is_arr) },
                "Failed to detect whether given js is an array"
            )?;

            if is_arr {
                // SAFETY: as above; `napi_is_array` confirmed it is an array.
                let arr = unsafe { Array::from_napi_value(env, napi_val) }?;
                let mut vec = vec![];
                for i in 0..arr.len() {
                    // `get` returns `None` only for an index past the length,
                    // which this loop never asks for; kept for parity with
                    // napi's `Vec<T>` conversion.
                    let Some(item) = arr.get::<JsUnknown>(i)? else {
                        return Err(napi::Error::new(
                            Status::InvalidArg,
                            "Found inconsistent data type in Array<T> when converting to Rust Vec<T>"
                                .to_owned(),
                        ));
                    };
                    // SAFETY: `item` was just returned by `napi_get_element`
                    // in this scope, so its raw handle is valid.
                    vec.push(unsafe { convert(env, item.raw(), child_depth) }?);
                }
                J::Array(vec)
            } else {
                // SAFETY: as above; the type is `Object`.
                let obj = unsafe { Object::from_napi_value(env, napi_val) }?;
                let mut map = serde_json::Map::new();
                for key in Object::keys(&obj)? {
                    // `None` means the property is `undefined`: napi skips the
                    // key, and so does this.
                    if let Some(item) = obj.get::<_, JsUnknown>(&key)? {
                        // SAFETY: `item` was just returned by
                        // `napi_get_named_property` in this scope.
                        map.insert(key, unsafe { convert(env, item.raw(), child_depth) }?);
                    }
                }
                J::Object(map)
            }
        }
        // Function, undefined, symbol, external, bigint, …: napi's own
        // conversion refuses each of these without recursing, so delegate to
        // it for the identical error.
        // SAFETY: as above.
        _ => unsafe { <serde_json::Value as FromNapiValue>::from_napi_value(env, napi_val) }?,
    };
    Ok(val)
}
