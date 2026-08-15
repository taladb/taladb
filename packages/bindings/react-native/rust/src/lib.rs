//! TalaDB C FFI layer for React Native JSI.
//!
//! All functions at this boundary follow these conventions:
//!
//! - Strings in : `*const c_char` — UTF-8, null-terminated, caller-owned.
//! - Strings out: `*mut c_char` — UTF-8, null-terminated, heap-allocated;
//!   **caller must free with `taladb_free_string`**.
//! - Handles: `*mut TalaDbHandle` — opaque pointer; create with
//!   `taladb_open` / `taladb_open_with_config`, destroy with `taladb_close`.
//! - Errors: string functions return `NULL` on error;
//!   integer functions return `-1` on error.
//!
//! JSON is used at every boundary so the C++ HostObject only needs
//! `JSON.stringify` / `JSON.parse` — no complex serialisation.
//!
//! # Error reporting
//!
//! A failed call sets a thread-local message readable with
//! [`taladb_last_error`]. Two rules make that message trustworthy, and both are
//! load-bearing:
//!
//! 1. **Every exported function clears the slot on entry**
//!    ([`clear_last_error`]). Without this, `taladb_last_error` reports whatever
//!    the *previous* failing call left behind, so a caller that checks it after
//!    a success — or after a failure that forgot to set one — reads a stale
//!    message describing an unrelated operation.
//! 2. **Every failure path sets a message before returning.** The shared
//!    argument helpers below (`ptr_to_ref`, `cstr_to_string`, `parse_*_args`,
//!    `json_to_fields`) set their own, so the common "bad argument" exits are
//!    covered without each function repeating the check.
//!
//! Together these give the documented contract: after any `taladb_*` call,
//! `taladb_last_error()` describes *that* call — a message if it failed, NULL
//! if it succeeded.
//!
//! # Safety
//!
//! All exported functions follow these invariants:
//! - Raw pointer arguments must be either null or point to valid, aligned,
//!   live memory for the duration of the call.
//! - `*const c_char` arguments must be null-terminated UTF-8 C strings.
//! - `*mut TalaDbHandle` must have been obtained from `taladb_open` /
//!   `taladb_open_with_config` and not yet passed to `taladb_close`.
//! - `*mut c_char` return values must be freed with `taladb_free_string`.
//!
//! Raw pointers are dereferenced only by the helpers at the bottom of this
//! file, which are `unsafe fn` and carry their own `# Safety` sections. Every
//! exported function reaches them through an explicit `unsafe` block —
//! `unsafe_op_in_unsafe_fn` is denied below so an `unsafe extern "C" fn` body
//! cannot quietly perform an unchecked dereference.

// Safety contract is documented at the module level above; repeating it on all
// 45 exported functions, which share it verbatim, would be noise. It does not
// extend to `unsafe fn`s inside the crate — those document themselves.
#![allow(clippy::missing_safety_doc)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use taladb_core::{
    Database, Filter, HnswOptions, TalaDbConfig, Update, Value, VectorMetric,
};

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(msg: String) {
    LAST_ERROR.with(|e| {
        *e.borrow_mut() = CString::new(msg).ok();
    });
}

/// Drop any message left by an earlier call on this thread.
///
/// Called first thing by every exported function except [`taladb_last_error`]
/// (which reads the slot) and `taladb_free_string` (which a caller may invoke
/// between a failure and reading its message). See the module-level *Error
/// reporting* section for why this is required rather than merely tidy.
fn clear_last_error() {
    LAST_ERROR.with(|e| {
        *e.borrow_mut() = None;
    });
}

/// Return the last error message as a null-terminated C string, or NULL if no error.
///
/// The message always describes the most recent `taladb_*` call on this thread:
/// every entry point clears the slot before doing any work, so a NULL return
/// means that call succeeded rather than that it forgot to report.
///
/// The returned pointer is valid until the next taladb_* call on this thread.
/// Do NOT free the returned string.
#[unsafe(no_mangle)]
pub extern "C" fn taladb_last_error() -> *const c_char {
    LAST_ERROR.with(|e| {
        e.borrow()
            .as_ref()
            .map(|s| s.as_ptr())
            .unwrap_or(std::ptr::null())
    })
}

// ---------------------------------------------------------------------------
// Opaque database handle
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct TalaDbHandle {
    db: Database,
}

impl TalaDbHandle {
    fn collection(&self, name: &str) -> Result<taladb_core::Collection, taladb_core::TalaDbError> {
        self.db.collection(name)
    }
}

// ---------------------------------------------------------------------------
// Open / Close
// ---------------------------------------------------------------------------

/// Open (or create) a TalaDB database at `path`.
///
/// Returns an opaque handle, or NULL on failure.
/// The handle must be freed with `taladb_close`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_open(path: *const c_char) -> *mut TalaDbHandle {
    clear_last_error();
    if path.is_null() {
        set_last_error("database path is null".into());
        return std::ptr::null_mut();
    }
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            set_last_error("database path is not valid UTF-8".into());
            return std::ptr::null_mut();
        }
    };
    match Database::open(Path::new(path_str)) {
        Ok(db) => Box::into_raw(Box::new(TalaDbHandle { db })),
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Open (or create) a TalaDB database at `path` with an optional config.
///
/// `config_json` — JSON-serialised `TalaDbConfig` (durability, plus an optional
/// `passphrase` for encryption at rest), or NULL for defaults. Change webhooks
/// are delivered by the `taladb` TypeScript client, not by this binding.
///
/// Returns an opaque handle, or NULL on failure.
/// The handle must be freed with `taladb_close`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_open_with_config(
    path: *const c_char,
    config_json: *const c_char,
) -> *mut TalaDbHandle {
    clear_last_error();
    if path.is_null() {
        set_last_error("database path is null".into());
        return std::ptr::null_mut();
    }
    let path_str = match unsafe { CStr::from_ptr(path) }.to_str() {
        Ok(s) => s,
        Err(_) => {
            set_last_error("database path is not valid UTF-8".into());
            return std::ptr::null_mut();
        }
    };

    let mut passphrase: Option<String> = None;
    let mut durability_eventual = false;
    if !config_json.is_null() {
        match unsafe { CStr::from_ptr(config_json) }.to_str() {
            Ok(json_str) => {
                passphrase = serde_json::from_str::<serde_json::Value>(json_str)
                    .ok()
                    .and_then(|v| {
                        v.get("passphrase")
                            .and_then(|p| p.as_str())
                            .map(str::to_owned)
                    });
                match serde_json::from_str::<TalaDbConfig>(json_str) {
                    Ok(config) => {
                        durability_eventual = !config.durability.flush_every_write;
                    }
                    Err(e) => {
                        set_last_error(format!("invalid config JSON: {e}"));
                        return std::ptr::null_mut();
                    }
                }
            }
            Err(_) => {
                set_last_error("config JSON is not valid UTF-8".into());
                return std::ptr::null_mut();
            }
        }
    }

    let opened = match passphrase {
        Some(passphrase) => Database::open_encrypted(Path::new(path_str), &passphrase),
        None => Database::open(Path::new(path_str)),
    };
    match opened {
        Ok(db) => {
            db.set_durability(durability_eventual);
            Box::into_raw(Box::new(TalaDbHandle { db }))
        }
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Compact the underlying storage file for this database handle.
/// No-op on in-memory databases. Returns 1 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_compact(handle: *mut TalaDbHandle) -> i32 {
    clear_last_error();
    match unsafe { ptr_to_ref(handle) } {
        Some(h) => match h.db.compact() {
            Ok(()) => 1,
            Err(e) => {
                set_last_error(e.to_string());
                -1
            }
        },
        // Null handle — `ptr_to_ref` has already set the message.
        None => -1,
    }
}

/// Close the database and free the handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_close(handle: *mut TalaDbHandle) {
    clear_last_error();
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle) });
    }
}

/// Free a string returned by any taladb_* function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

// ---------------------------------------------------------------------------
// Insert
// ---------------------------------------------------------------------------

/// Insert a document (JSON object string).
/// Returns the new document's ULID as a C string, or NULL on error.
/// Caller must free the returned string with `taladb_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_insert(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    doc_json: *const c_char,
) -> *mut c_char {
    clear_last_error();
    let (h, col_name, json) = match unsafe { parse_handle_args(handle, collection, doc_json) } {
        Some(t) => t,
        None => return std::ptr::null_mut(),
    };
    let fields = match json_to_fields(&json) {
        Some(f) => f,
        None => {
            set_last_error("insert expects a JSON object".into());
            return std::ptr::null_mut();
        }
    };
    let col = match h.collection(&col_name) {
        Ok(c) => c,
        Err(e) => {
            set_last_error(e.to_string());
            return std::ptr::null_mut();
        }
    };
    match col.insert(fields) {
        Ok(id) => to_cstring(id.to_string()),
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Insert multiple documents (JSON array of objects).
/// Returns a JSON array of ULID strings, or NULL on error.
/// Caller must free with `taladb_free_string`.
///
/// **All or nothing.** If any element of the array is not an object, the whole
/// call fails and nothing is written. It previously skipped unparseable
/// elements and returned a shorter id array, so a caller zipping the returned
/// ids back onto its input silently mis-associated every document after the
/// first bad one — and had no way to learn which had been dropped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_insert_many(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    docs_json: *const c_char,
) -> *mut c_char {
    clear_last_error();
    let (h, col_name, json) = match unsafe { parse_handle_args(handle, collection, docs_json) } {
        Some(t) => t,
        None => return std::ptr::null_mut(),
    };
    let arr = match serde_json::from_str::<Vec<serde_json::Value>>(&json) {
        Ok(v) => v,
        Err(e) => {
            set_last_error(format!("insertMany expects a JSON array of objects: {e}"));
            return std::ptr::null_mut();
        }
    };
    let mut items: Vec<Vec<(String, Value)>> = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        match json_to_fields(&v.to_string()) {
            Some(fields) => items.push(fields),
            None => {
                set_last_error(format!(
                    "insertMany: document at index {i} is not a JSON object; \
                     no documents were inserted"
                ));
                return std::ptr::null_mut();
            }
        }
    }
    let col = match h.collection(&col_name) {
        Ok(c) => c,
        Err(e) => {
            set_last_error(e.to_string());
            return std::ptr::null_mut();
        }
    };
    match col.insert_many(items) {
        Ok(ids) => {
            let id_strs: Vec<String> = ids.iter().map(taladb_core::Ulid::to_string).collect();
            to_cstring(serde_json::to_string(&id_strs).unwrap_or_default())
        }
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Replication writes — id-addressed upsert / delete
// ---------------------------------------------------------------------------

/// Find all documents matching `filter_json`.
/// Pass `"{}"` or `"null"` to match all.
/// Returns a JSON array string, or NULL on error.
/// Caller must free with `taladb_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_find(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    filter_json: *const c_char,
) -> *mut c_char {
    clear_last_error();
    let (db, col_name, json) = match unsafe { parse_db_args(handle, collection, filter_json) } {
        Some(t) => t,
        None => return std::ptr::null_mut(),
    };
    let filter = match parse_filter(&json) {
        Ok(f) => f,
        Err(e) => {
            set_last_error(e);
            return std::ptr::null_mut();
        }
    };
    let col = match db.collection(&col_name) {
        Ok(c) => c,
        Err(e) => {
            set_last_error(e.to_string());
            return std::ptr::null_mut();
        }
    };
    match col.find(filter) {
        Ok(docs) => {
            let json_docs: Vec<serde_json::Value> = docs.iter().map(doc_to_json).collect();
            to_cstring(serde_json::to_string(&json_docs).unwrap_or_default())
        }
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Find one document matching `filter_json`, or JSON `null` if none.
/// Caller must free with `taladb_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_find_one(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    filter_json: *const c_char,
) -> *mut c_char {
    clear_last_error();
    let (db, col_name, json) = match unsafe { parse_db_args(handle, collection, filter_json) } {
        Some(t) => t,
        None => return std::ptr::null_mut(),
    };
    let filter = match parse_filter(&json) {
        Ok(f) => f,
        Err(e) => {
            set_last_error(e);
            return std::ptr::null_mut();
        }
    };
    let col = match db.collection(&col_name) {
        Ok(c) => c,
        Err(e) => {
            set_last_error(e.to_string());
            return std::ptr::null_mut();
        }
    };
    match col.find_one(filter) {
        Ok(Some(doc)) => to_cstring(doc_to_json(&doc).to_string()),
        Ok(None) => to_cstring("null".to_string()),
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Update
// ---------------------------------------------------------------------------

/// Update the first matching document.
/// Returns 1 if updated, 0 if not found, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_update_one(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    filter_json: *const c_char,
    update_json: *const c_char,
) -> i32 {
    clear_last_error();
    let handle = unsafe { ptr_to_ref(handle) };
    let col_name = unsafe { cstr_to_string(collection) };
    let filter_str = unsafe { cstr_to_string(filter_json) };
    let update_str = unsafe { cstr_to_string(update_json) };
    match (handle, col_name, filter_str, update_str) {
        (Some(h), Some(col), Some(fs), Some(us)) => {
            let filter = match parse_filter(&fs) {
                Ok(f) => f,
                Err(e) => {
                    set_last_error(e);
                    return -1;
                }
            };
            let collection = match h.collection(&col) {
                Ok(c) => c,
                Err(e) => {
                    set_last_error(e.to_string());
                    return -1;
                }
            };
            match parse_update(&us) {
                Some(update) => match collection.update_one(filter, update) {
                    Ok(true) => 1,
                    Ok(false) => 0,
                    Err(e) => {
                        set_last_error(e.to_string());
                        -1
                    }
                },
                None => {
                    set_last_error("update document could not be parsed".into());
                    -1
                }
            }
        }
        // One of the pointer arguments was rejected; the helper that rejected
        // it has already set the message.
        _ => -1,
    }
}

/// Update all matching documents.
/// Returns count updated, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_update_many(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    filter_json: *const c_char,
    update_json: *const c_char,
) -> i32 {
    clear_last_error();
    let handle = unsafe { ptr_to_ref(handle) };
    let col_name = unsafe { cstr_to_string(collection) };
    let filter_str = unsafe { cstr_to_string(filter_json) };
    let update_str = unsafe { cstr_to_string(update_json) };
    match (handle, col_name, filter_str, update_str) {
        (Some(h), Some(col), Some(fs), Some(us)) => {
            let filter = match parse_filter(&fs) {
                Ok(f) => f,
                Err(e) => {
                    set_last_error(e);
                    return -1;
                }
            };
            let collection = match h.collection(&col) {
                Ok(c) => c,
                Err(e) => {
                    set_last_error(e.to_string());
                    return -1;
                }
            };
            match parse_update(&us) {
                Some(update) => match collection.update_many(filter, update) {
                    Ok(n) => n as i32,
                    Err(e) => {
                        set_last_error(e.to_string());
                        -1
                    }
                },
                None => {
                    set_last_error("update document could not be parsed".into());
                    -1
                }
            }
        }
        // One of the pointer arguments was rejected; the helper that rejected
        // it has already set the message.
        _ => -1,
    }
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

/// Delete the first matching document.
/// Returns 1 if deleted, 0 if not found, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_delete_one(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    filter_json: *const c_char,
) -> i32 {
    clear_last_error();
    let (h, col_name, json) = match unsafe { parse_handle_args(handle, collection, filter_json) } {
        Some(t) => t,
        None => return -1,
    };
    let col = match h.collection(&col_name) {
        Ok(c) => c,
        Err(e) => {
            set_last_error(e.to_string());
            return -1;
        }
    };
    let filter = match parse_filter(&json) {
        Ok(f) => f,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    match col.delete_one(filter) {
        Ok(true) => 1,
        Ok(false) => 0,
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

/// Delete all matching documents.
/// Returns count deleted, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_delete_many(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    filter_json: *const c_char,
) -> i32 {
    clear_last_error();
    let (h, col_name, json) = match unsafe { parse_handle_args(handle, collection, filter_json) } {
        Some(t) => t,
        None => return -1,
    };
    let col = match h.collection(&col_name) {
        Ok(c) => c,
        Err(e) => {
            set_last_error(e.to_string());
            return -1;
        }
    };
    let filter = match parse_filter(&json) {
        Ok(f) => f,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    match col.delete_many(filter) {
        Ok(n) => n as i32,
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// Count
// ---------------------------------------------------------------------------

/// Count documents matching `filter_json`.
/// Returns count, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_count(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    filter_json: *const c_char,
) -> i32 {
    clear_last_error();
    let (db, col_name, json) = match unsafe { parse_db_args(handle, collection, filter_json) } {
        Some(t) => t,
        None => return -1,
    };
    let col = match db.collection(&col_name) {
        Ok(c) => c,
        Err(e) => {
            set_last_error(e.to_string());
            return -1;
        }
    };
    let filter = match parse_filter(&json) {
        Ok(f) => f,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    match col.count(filter) {
        Ok(n) => n as i32,
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

/// Run an aggregation pipeline (`pipeline_json` is a JSON array of stages).
/// Returns a JSON array of result documents, or NULL on error.
/// Caller must free with `taladb_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_aggregate(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    pipeline_json: *const c_char,
) -> *mut c_char {
    clear_last_error();
    let (db, col_name, json) = match unsafe { parse_db_args(handle, collection, pipeline_json) } {
        Some(t) => t,
        None => return std::ptr::null_mut(),
    };
    let value: serde_json::Value = match serde_json::from_str(&json) {
        Ok(v) => v,
        Err(e) => {
            set_last_error(e.to_string());
            return std::ptr::null_mut();
        }
    };
    let pipeline = match taladb_core::aggregate::parse_pipeline(&value, &|v| {
        json_to_filter(v).ok_or_else(|| "invalid filter in $match".to_string())
    }) {
        Ok(p) => p,
        Err(e) => {
            set_last_error(e);
            return std::ptr::null_mut();
        }
    };
    let col = match db.collection(&col_name) {
        Ok(c) => c,
        Err(e) => {
            set_last_error(e.to_string());
            return std::ptr::null_mut();
        }
    };
    match col.aggregate(pipeline) {
        Ok(docs) => {
            let json_docs: Vec<serde_json::Value> = docs.iter().map(doc_to_json).collect();
            to_cstring(serde_json::to_string(&json_docs).unwrap_or_default())
        }
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Bidirectional sync — changeset export / import (backs JS `db.sync()`)
/// User collection names (reserved `_`-prefixed excluded), as a JSON array
/// string. Backs the sync orchestration's "sync all collections" default.
/// NULL on error. Caller must free with `taladb_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_list_collection_names(handle: *mut TalaDbHandle) -> *mut c_char {
    clear_last_error();
    let h = match unsafe { ptr_to_ref(handle) } {
        Some(h) => h,
        None => return std::ptr::null_mut(),
    };
    match h.db.list_collection_names() {
        Ok(names) => match serde_json::to_string(&names) {
            Ok(s) => to_cstring(s),
            Err(e) => {
                set_last_error(e.to_string());
                std::ptr::null_mut()
            }
        },
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Read the current application migration version (0 if never set), or -1 on
/// error. Backs the `openDB({ migrations })` runner.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_user_version(handle: *mut TalaDbHandle) -> i64 {
    clear_last_error();
    let h = match unsafe { ptr_to_ref(handle) } {
        Some(h) => h,
        None => return -1,
    };
    match h.db.user_version() {
        Ok(v) => i64::from(v),
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

/// Persist the application migration version. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_set_user_version(handle: *mut TalaDbHandle, version: u32) -> i32 {
    clear_last_error();
    let h = match unsafe { ptr_to_ref(handle) } {
        Some(h) => h,
        None => return -1,
    };
    match h.db.set_user_version(version) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

/// Force any batched (eventual-durability) writes to disk. Returns 0 on
/// success, -1 on error. No-op under the default immediate durability.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_flush(handle: *mut TalaDbHandle) -> i32 {
    clear_last_error();
    let h = match unsafe { ptr_to_ref(handle) } {
        Some(h) => h,
        None => return -1,
    };
    match h.db.flush() {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

// ---------------------------------------------------------------------------

/// Create a secondary index on `field`. No-op if already exists.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_create_index(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    field: *const c_char,
) {
    clear_last_error();
    if let (Some(h), Some(col), Some(f)) = (
        unsafe { ptr_to_ref(handle) },
        unsafe { cstr_to_string(collection) },
        unsafe { cstr_to_string(field) },
    ) && let Ok(c) = h.db.collection(&col)
    {
        let _ = c.create_index(&f);
    }
}

/// Drop a secondary index on `field`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_drop_index(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    field: *const c_char,
) {
    clear_last_error();
    if let (Some(h), Some(col), Some(f)) = (
        unsafe { ptr_to_ref(handle) },
        unsafe { cstr_to_string(collection) },
        unsafe { cstr_to_string(field) },
    ) && let Ok(c) = h.db.collection(&col)
    {
        let _ = c.drop_index(&f);
    }
}

/// Create a compound index over `fields_json` (a JSON array of field names).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_create_compound_index(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    fields_json: *const c_char,
) {
    clear_last_error();
    if let (Some(h), Some(col), Some(fj)) = (
        unsafe { ptr_to_ref(handle) },
        unsafe { cstr_to_string(collection) },
        unsafe { cstr_to_string(fields_json) },
    ) && let Ok(fields) = serde_json::from_str::<Vec<String>>(&fj)
        && let Ok(c) = h.db.collection(&col)
    {
        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        if let Err(e) = c.create_compound_index(&refs) {
            set_last_error(e.to_string());
        }
    }
}

/// Drop a compound index by its ordered field list (`fields_json`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_drop_compound_index(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    fields_json: *const c_char,
) {
    clear_last_error();
    if let (Some(h), Some(col), Some(fj)) = (
        unsafe { ptr_to_ref(handle) },
        unsafe { cstr_to_string(collection) },
        unsafe { cstr_to_string(fields_json) },
    ) && let Ok(fields) = serde_json::from_str::<Vec<String>>(&fj)
        && let Ok(c) = h.db.collection(&col)
    {
        let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
        if let Err(e) = c.drop_compound_index(&refs) {
            set_last_error(e.to_string());
        }
    }
}

/// Create a full-text search index on `field`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_create_fts_index(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    field: *const c_char,
) {
    clear_last_error();
    if let (Some(h), Some(col), Some(f)) = (
        unsafe { ptr_to_ref(handle) },
        unsafe { cstr_to_string(collection) },
        unsafe { cstr_to_string(field) },
    ) && let Ok(c) = h.db.collection(&col)
    {
        let _ = c.create_fts_index(&f);
    }
}

/// Drop a full-text search index on `field`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_drop_fts_index(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    field: *const c_char,
) {
    clear_last_error();
    if let (Some(h), Some(col), Some(f)) = (
        unsafe { ptr_to_ref(handle) },
        unsafe { cstr_to_string(collection) },
        unsafe { cstr_to_string(field) },
    ) && let Ok(c) = h.db.collection(&col)
    {
        let _ = c.drop_fts_index(&f);
    }
}

/// Rank documents against a free-text query using BM25 (OR semantics).
///
/// `filter_json` / `options_json` may be NULL. `options_json` accepts
/// `{ k1, b }`.
///
/// Returns a JSON array string `[{document, score}, ...]`, or NULL on error.
/// Caller must free with `taladb_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_search_text(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    field: *const c_char,
    query: *const c_char,
    top_k: usize,
    filter_json: *const c_char,
    options_json: *const c_char,
) -> *mut c_char {
    clear_last_error();
    let (Some(h), Some(col), Some(fld), Some(q)) = (
        unsafe { ptr_to_ref(handle) },
        unsafe { cstr_to_string(collection) },
        unsafe { cstr_to_string(field) },
        unsafe { cstr_to_string(query) },
    ) else {
        return std::ptr::null_mut();
    };

    let pre_filter = match optional_filter(filter_json) {
        Ok(f) => f,
        Err(e) => {
            set_last_error(e);
            return std::ptr::null_mut();
        }
    };
    let options = unsafe { cstr_to_string(options_json) }.and_then(|s| serde_json::from_str(&s).ok());
    let bm25 = bm25_from_options(options.as_ref());

    let result =
        h.db.collection(&col)
            .and_then(|c| c.search_text_with(&fld, &q, top_k, &bm25, pre_filter));

    match result {
        Ok(results) => {
            let arr: Vec<serde_json::Value> = results
                .iter()
                .map(|r| {
                    serde_json::json!({ "document": doc_to_json(&r.document), "score": r.score })
                })
                .collect();
            to_cstring(serde_json::to_string(&arr).unwrap_or_default())
        }
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Hybrid retrieval — BM25 and vector similarity fused with reciprocal rank
/// fusion.
///
/// `vector_ptr` must point to `vector_len` consecutive `f32` values.
/// `options_json` accepts `{ rrfK, textWeight, vectorWeight, candidates, k1, b }`.
///
/// Returns a JSON array string `[{document, score, textRank, vectorRank}, ...]`,
/// or NULL on error. Caller must free with `taladb_free_string`.
///
/// # Safety
/// `vector_ptr` must be valid for `vector_len` `f32` reads.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn taladb_hybrid_search(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    text_field: *const c_char,
    text: *const c_char,
    vector_field: *const c_char,
    vector_ptr: *const f32,
    vector_len: usize,
    top_k: usize,
    filter_json: *const c_char,
    options_json: *const c_char,
) -> *mut c_char {
    clear_last_error();
    let (Some(h), Some(col), Some(tf), Some(t), Some(vf)) = (
        unsafe { ptr_to_ref(handle) },
        unsafe { cstr_to_string(collection) },
        unsafe { cstr_to_string(text_field) },
        unsafe { cstr_to_string(text) },
        unsafe { cstr_to_string(vector_field) },
    ) else {
        return std::ptr::null_mut();
    };

    if vector_ptr.is_null() && vector_len != 0 {
        set_last_error("null vector pointer".to_string());
        return std::ptr::null_mut();
    }
    let vector: &[f32] = if vector_len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(vector_ptr, vector_len) }
    };

    let pre_filter = match optional_filter(filter_json) {
        Ok(f) => f,
        Err(e) => {
            set_last_error(e);
            return std::ptr::null_mut();
        }
    };
    let options: Option<serde_json::Value> =
        unsafe { cstr_to_string(options_json) }.and_then(|s| serde_json::from_str(&s).ok());

    let mut query = taladb_core::fts::HybridQuery::new(&tf, &t, &vf, vector, top_k);
    query.filter = pre_filter;
    query.bm25 = bm25_from_options(options.as_ref());
    query.rrf = rrf_from_options(options.as_ref());
    query.candidates = options
        .as_ref()
        .and_then(|o| o.get("candidates"))
        .and_then(serde_json::Value::as_u64)
        .map(|v| v as usize);

    match h.db.collection(&col).and_then(|c| c.hybrid_search(query)) {
        Ok(results) => {
            let arr: Vec<serde_json::Value> = results
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "document": doc_to_json(&r.document),
                        "score": r.score,
                        "textRank": r.text_rank,
                        "vectorRank": r.vector_rank,
                    })
                })
                .collect();
            to_cstring(serde_json::to_string(&arr).unwrap_or_default())
        }
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Parse an optional filter argument shared by the search entry points.
///
/// A malformed filter is an error rather than "no filter" — silently
/// widening a query is how a scoped search turns into a full scan.
fn optional_filter(filter_json: *const c_char) -> Result<Option<Filter>, String> {
    let Some(s) = (unsafe { cstr_to_string(filter_json) }) else {
        return Ok(None);
    };
    if s.is_empty() || s == "null" || s == "{}" {
        return Ok(None);
    }
    match parse_filter(&s) {
        Ok(Filter::All) => Ok(None),
        Ok(f) => Ok(Some(f)),
        Err(e) => Err(e),
    }
}

fn bm25_from_options(options: Option<&serde_json::Value>) -> taladb_core::bm25::Bm25Params {
    let mut params = taladb_core::bm25::Bm25Params::default();
    if let Some(o) = options {
        if let Some(v) = o.get("k1").and_then(serde_json::Value::as_f64) {
            params.k1 = v as f32;
        }
        if let Some(v) = o.get("b").and_then(serde_json::Value::as_f64) {
            params.b = v as f32;
        }
    }
    params
}

fn rrf_from_options(options: Option<&serde_json::Value>) -> taladb_core::bm25::RrfParams {
    let mut params = taladb_core::bm25::RrfParams::default();
    if let Some(o) = options {
        if let Some(v) = o.get("rrfK").and_then(serde_json::Value::as_f64) {
            params.k = v as f32;
        }
        if let Some(v) = o.get("textWeight").and_then(serde_json::Value::as_f64) {
            params.text_weight = v as f32;
        }
        if let Some(v) = o.get("vectorWeight").and_then(serde_json::Value::as_f64) {
            params.vector_weight = v as f32;
        }
    }
    params
}

/// Borrow a handle produced by `taladb_open`, or `None` if it is null.
///
/// Sets the last-error message on failure, so callers can return `NULL`/`-1`
/// directly without reporting separately.
///
/// # Safety
///
/// - `handle` must be null, or point to a live `TalaDbHandle` obtained from
///   `taladb_open` / `taladb_open_with_config` and not yet passed to
///   `taladb_close`.
/// - The caller chooses the returned lifetime `'a`, which is therefore
///   unbounded: it must not outlive the `taladb_close` that frees `handle`.
///   Every caller here is a single exported function that returns before its
///   handle can be closed by another call on the same thread.
unsafe fn ptr_to_ref<'a>(handle: *mut TalaDbHandle) -> Option<&'a TalaDbHandle> {
    if handle.is_null() {
        set_last_error("database handle is null".into());
        return None;
    }
    // SAFETY: non-null, and the caller guarantees it is a live handle from
    // `taladb_open` per this function's contract.
    Some(unsafe { &*handle })
}

/// Copy a C string argument into an owned `String`.
///
/// Sets the last-error message when the pointer is null or the bytes are not
/// valid UTF-8.
///
/// # Safety
///
/// `s` must be null, or point to a null-terminated C string that stays valid
/// and unmodified for the duration of the call.
unsafe fn cstr_to_string(s: *const c_char) -> Option<String> {
    if s.is_null() {
        set_last_error("required string argument is null".into());
        return None;
    }
    // SAFETY: non-null, and the caller guarantees null-termination and validity
    // for the duration of this call per this function's contract.
    match unsafe { CStr::from_ptr(s) }.to_str() {
        Ok(s) => Some(s.to_owned()),
        Err(_) => {
            set_last_error("string argument is not valid UTF-8".into());
            None
        }
    }
}

fn to_cstring(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(cs) => cs.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Convenience for write-path functions: returns `(&TalaDbHandle, col_name, extra_str)`.
///
/// The underlying helpers set the last-error message, so a `None` here is
/// already reported.
///
/// # Safety
///
/// Carries the same requirements as [`ptr_to_ref`] for `handle` and
/// [`cstr_to_string`] for `collection` and `extra`.
unsafe fn parse_handle_args<'a>(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    extra: *const c_char,
) -> Option<(&'a TalaDbHandle, String, String)> {
    // SAFETY: this function's contract is the union of the callees' contracts,
    // so the caller has already guaranteed exactly what they require.
    unsafe {
        Some((
            ptr_to_ref(handle)?,
            cstr_to_string(collection)?,
            cstr_to_string(extra)?,
        ))
    }
}

/// Convenience for read-path functions: returns `(&Database, col_name, extra_str)`.
/// Read-path operations don't need the sync hook.
///
/// # Safety
///
/// Carries the same requirements as [`parse_handle_args`].
unsafe fn parse_db_args<'a>(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    extra: *const c_char,
) -> Option<(&'a taladb_core::Database, String, String)> {
    // SAFETY: as above — the caller has guaranteed the callees' requirements.
    unsafe {
        Some((
            &ptr_to_ref(handle)?.db,
            cstr_to_string(collection)?,
            cstr_to_string(extra)?,
        ))
    }
}

// ---------------------------------------------------------------------------
// JSON ↔ taladb-core type converters
// ---------------------------------------------------------------------------

fn json_to_value(j: &serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::Str(s.clone()),
        serde_json::Value::Array(arr) => Value::Array(arr.iter().map(json_to_value).collect()),
        serde_json::Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect(),
        ),
    }
}

fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int(n) => serde_json::Value::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Str(s) => serde_json::Value::String(s.clone()),
        Value::Bytes(b) => serde_json::Value::String(format!("<bytes:{}>", b.len())),
        Value::Array(arr) => serde_json::Value::Array(arr.iter().map(value_to_json).collect()),
        Value::Object(obj) => serde_json::Value::Object(
            obj.iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect(),
        ),
    }
}

fn doc_to_json(doc: &taladb_core::Document) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "_id".to_string(),
        serde_json::Value::String(doc.id.to_string()),
    );
    for (k, v) in &doc.fields {
        map.insert(k.clone(), value_to_json(v));
    }
    serde_json::Value::Object(map)
}

/// Parse a document JSON string into insert fields, `_id` included.
///
/// The filter that used to drop `_id` here made supplying one a silent no-op —
/// the engine now takes it, validates it, and refuses to overwrite an existing
/// document, identically on all three runtimes.
fn json_to_fields(json: &str) -> Option<Vec<(String, Value)>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = v.as_object()?;
    Some(
        obj.iter()
            .map(|(k, v)| (k.clone(), json_to_value(v)))
            .collect(),
    )
}

/// Parse a filter JSON string.
///
/// `"null"` and `"{}"` mean match-all. Anything unparseable is an **error**,
/// never a silent match-all: degrading a malformed filter to `Filter::All`
/// would make a typo'd operator in `deleteMany`/`updateMany` hit every
/// document in the collection.
fn parse_filter(json: &str) -> Result<Filter, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("invalid filter JSON: {e}"))?;
    if v.is_null() || (v.is_object() && v.as_object().is_some_and(serde_json::Map::is_empty)) {
        return Ok(Filter::All);
    }
    json_to_filter(&v).ok_or_else(|| format!("invalid filter: {json}"))
}

fn json_to_filter(v: &serde_json::Value) -> Option<Filter> {
    let obj = v.as_object()?;
    let mut filters: Vec<Filter> = Vec::new();
    for (field, expr) in obj {
        if field.starts_with('$') {
            let logical = match field.as_str() {
                "$and" => Filter::And(
                    expr.as_array()?
                        .iter()
                        .map(json_to_filter)
                        .collect::<Option<_>>()?,
                ),
                "$or" => Filter::Or(
                    expr.as_array()?
                        .iter()
                        .map(json_to_filter)
                        .collect::<Option<_>>()?,
                ),
                "$not" => Filter::Not(Box::new(json_to_filter(expr)?)),
                _ => return None,
            };
            filters.push(logical);
            continue;
        }
        if !expr.is_object() {
            filters.push(Filter::Eq(field.clone(), json_to_value(expr)));
            continue;
        }
        let ops = expr.as_object()?;
        if ops.is_empty() {
            // `{field: {}}` is ambiguous (error on node, match-all historically
            // on web/RN) — rejected everywhere as of 0.8.1.
            return None;
        }
        for (op, val) in ops {
            let v = json_to_value(val);
            let f = match op.as_str() {
                "$eq" => Filter::Eq(field.clone(), v),
                "$ne" => Filter::Ne(field.clone(), v),
                "$gt" => Filter::Gt(field.clone(), v),
                "$gte" => Filter::Gte(field.clone(), v),
                "$lt" => Filter::Lt(field.clone(), v),
                "$lte" => Filter::Lte(field.clone(), v),
                "$exists" => Filter::Exists(field.clone(), val.as_bool()?),
                "$in" => Filter::In(
                    field.clone(),
                    val.as_array()?.iter().map(json_to_value).collect(),
                ),
                "$nin" => Filter::Nin(
                    field.clone(),
                    val.as_array()?.iter().map(json_to_value).collect(),
                ),
                "$contains" => Filter::Contains(field.clone(), val.as_str()?.to_string()),
                "$regex" => Filter::Regex(field.clone(), val.as_str()?.to_string()),
                _ => return None,
            };
            filters.push(f);
        }
    }
    match filters.len() {
        0 => Some(Filter::All),
        1 => Some(filters.remove(0)),
        _ => Some(Filter::And(filters)),
    }
}

// ---------------------------------------------------------------------------
// Vector index management
// ---------------------------------------------------------------------------

fn parse_vector_metric(m: *const c_char) -> Option<VectorMetric> {
    if m.is_null() {
        return None;
    }
    let s = match unsafe { CStr::from_ptr(m) }.to_str() {
        Ok(s) => s,
        Err(_) => return None,
    };
    match s {
        "cosine" => Some(VectorMetric::Cosine),
        "dot" => Some(VectorMetric::Dot),
        "euclidean" => Some(VectorMetric::Euclidean),
        _ => None,
    }
}

fn parse_hnsw_opts(json: *const c_char) -> Option<HnswOptions> {
    if json.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(json) }.to_str().ok()?;
    serde_json::from_str::<HnswOptions>(s).ok()
}

/// Create a vector index. `metric` and `hnsw_json` may be NULL.
/// Returns 1 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_create_vector_index(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    field: *const c_char,
    dimensions: usize,
    metric: *const c_char,
    hnsw_json: *const c_char,
) -> i32 {
    clear_last_error();
    let (h, col, fld) = match (
        unsafe { ptr_to_ref(handle) },
        unsafe { cstr_to_string(collection) },
        unsafe { cstr_to_string(field) },
    ) {
        (Some(h), Some(c), Some(f)) => (h, c, f),
        // A helper rejected one of the pointers and set the message.
        _ => return -1,
    };
    let m = parse_vector_metric(metric);
    let hnsw = parse_hnsw_opts(hnsw_json);
    let c = match h.db.collection(&col) {
        Ok(c) => c,
        Err(e) => {
            set_last_error(e.to_string());
            return -1;
        }
    };
    match c.create_vector_index(&fld, dimensions, m, hnsw) {
        Ok(()) => 1,
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

/// Drop a vector index. Returns 1 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_drop_vector_index(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    field: *const c_char,
) -> i32 {
    clear_last_error();
    let (h, col, fld) = match (
        unsafe { ptr_to_ref(handle) },
        unsafe { cstr_to_string(collection) },
        unsafe { cstr_to_string(field) },
    ) {
        (Some(h), Some(c), Some(f)) => (h, c, f),
        // A helper rejected one of the pointers and set the message.
        _ => return -1,
    };
    match h
        .db
        .collection(&col)
        .and_then(|c| c.drop_vector_index(&fld))
    {
        Ok(()) => 1,
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

/// Rebuild the HNSW graph for a vector index. No-op when HNSW is disabled or
/// the index is flat-only. Returns 1 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_upgrade_vector_index(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    field: *const c_char,
) -> i32 {
    clear_last_error();
    let (h, col, fld) = match (
        unsafe { ptr_to_ref(handle) },
        unsafe { cstr_to_string(collection) },
        unsafe { cstr_to_string(field) },
    ) {
        (Some(h), Some(c), Some(f)) => (h, c, f),
        // A helper rejected one of the pointers and set the message.
        _ => return -1,
    };
    match h
        .db
        .collection(&col)
        .and_then(|c| c.upgrade_vector_index(&fld))
    {
        Ok(()) => 1,
        Err(e) => {
            set_last_error(e.to_string());
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// findNearest — Float32 raw-pointer fast path
// ---------------------------------------------------------------------------

/// Internal: run a find_nearest given a borrowed Database + raw query pointer.
///
/// # Safety
/// `query_ptr` must point to `query_len` consecutive `f32` values.
fn run_find_nearest(
    db: &Database,
    collection: &str,
    field: &str,
    query: &[f32],
    top_k: usize,
    filter_json: Option<&str>,
) -> Result<String, taladb_core::TalaDbError> {
    let pre_filter = match filter_json {
        None => None,
        Some(s) => match parse_filter(s) {
            Ok(Filter::All) => None,
            Ok(f) => Some(f),
            // A malformed pre-filter must not silently become an
            // unfiltered search.
            Err(e) => return Err(taladb_core::TalaDbError::InvalidFilter(e)),
        },
    };
    let col = db.collection(collection)?;
    let results = col.find_nearest(field, query, top_k, pre_filter)?;
    let arr: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            serde_json::json!({
                "document": doc_to_json(&r.document),
                "score": r.score,
            })
        })
        .collect();
    Ok(serde_json::to_string(&arr).unwrap_or_default())
}

/// Synchronous `find_nearest` with a zero-copy Float32 query vector.
///
/// `query_ptr` — pointer to `query_len` consecutive f32 values (caller-owned).
/// `filter_json` — optional pre-filter JSON (may be NULL / "{}" / "null").
///
/// Returns a JSON array string `[{document, score}, ...]`, or NULL on error.
/// Caller must free with `taladb_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_find_nearest(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    field: *const c_char,
    query_ptr: *const f32,
    query_len: usize,
    top_k: usize,
    filter_json: *const c_char,
) -> *mut c_char {
    clear_last_error();
    let h = match unsafe { ptr_to_ref(handle) } {
        Some(h) => h,
        None => return std::ptr::null_mut(),
    };
    let col = match unsafe { cstr_to_string(collection) } {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    let fld = match unsafe { cstr_to_string(field) } {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    if query_ptr.is_null() && query_len != 0 {
        set_last_error("null query pointer".to_string());
        return std::ptr::null_mut();
    }
    let query: &[f32] = if query_len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(query_ptr, query_len) }
    };
    let filter = if filter_json.is_null() {
        None
    } else {
        match unsafe { CStr::from_ptr(filter_json) }.to_str() {
            Ok(s) => Some(s),
            Err(_) => {
                set_last_error("filter JSON is not valid UTF-8".into());
                return std::ptr::null_mut();
            }
        }
    };
    match run_find_nearest(&h.db, &col, &fld, query, top_k, filter) {
        Ok(json) => to_cstring(json),
        Err(e) => {
            set_last_error(e.to_string());
            std::ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Async job API — background-thread dispatch for heavy queries
//
// The JS wrapper calls `*_start` to kick a job, then polls `taladb_job_poll`
// (non-blocking) via `setImmediate` until the job reports done. It then
// takes the result with `taladb_job_take_result`, which also frees the job.
//
// Each worker clones the Arc-backed database state before returning, so the
// caller may close the original `TalaDbHandle` while a job is in flight.
// ---------------------------------------------------------------------------

/// A background job handle. Opaque to the caller.
pub struct TalaDbJob {
    done: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<String, String>>>>,
    // Join handle kept so we can join on take_result to avoid races.
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn spawn_job<F>(handle: *mut TalaDbHandle, work: F) -> *mut TalaDbJob
where
    F: FnOnce(&TalaDbHandle) -> Result<String, String> + Send + 'static,
{
    if handle.is_null() {
        set_last_error("database handle is null".into());
        return std::ptr::null_mut();
    }
    // Give the worker owned, Arc-backed database state. The opaque FFI handle
    // may then be closed as soon as the job starts without invalidating the
    // worker's database or sync hook.
    let owned_handle = unsafe { &*handle }.clone();
    let done = Arc::new(AtomicBool::new(false));
    let result = Arc::new(Mutex::new(None));

    let done_thread = Arc::clone(&done);
    let result_thread = Arc::clone(&result);
    let t = thread::spawn(move || {
        let r = work(&owned_handle);
        if let Ok(mut slot) = result_thread.lock() {
            *slot = Some(r);
        }
        done_thread.store(true, Ordering::Release);
    });

    Box::into_raw(Box::new(TalaDbJob {
        done,
        result,
        thread: Mutex::new(Some(t)),
    }))
}

/// Non-blocking poll. Returns 1 if the job has finished, 0 if still running.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_job_poll(job: *mut TalaDbJob) -> i32 {
    clear_last_error();
    if job.is_null() {
        set_last_error("job handle is null".into());
        return -1;
    }
    let job = unsafe { &*job };
    if job.done.load(Ordering::Acquire) {
        1
    } else {
        0
    }
}

/// Wait for the job to complete, take its result, and free the job.
/// Returns the result JSON string on success, or NULL on error (see
/// `taladb_last_error`). Always consumes and frees the job.
/// Caller must free the returned string with `taladb_free_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_job_take_result(job: *mut TalaDbJob) -> *mut c_char {
    clear_last_error();
    if job.is_null() {
        set_last_error("job handle is null".into());
        return std::ptr::null_mut();
    }
    let job = unsafe { Box::from_raw(job) };
    // Join the worker thread so the result is definitely present.
    // Take + drop the guard in its own scope so it doesn't outlive `job`.
    let thread_opt = {
        let mut g = match job.thread.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        g.take()
    };
    if let Some(t) = thread_opt {
        let _ = t.join();
    }
    let r = {
        let mut g = match job.result.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        g.take()
    };
    let r = match r {
        Some(r) => r,
        None => {
            set_last_error("job produced no result".to_string());
            return std::ptr::null_mut();
        }
    };
    match r {
        Ok(json) => to_cstring(json),
        Err(e) => {
            set_last_error(e);
            std::ptr::null_mut()
        }
    }
}

/// Cancel (detach) the job and free its handle. The worker owns a clone of all
/// database state it needs, so it can safely finish after the caller closes the
/// original database handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_job_cancel(job: *mut TalaDbJob) {
    clear_last_error();
    if job.is_null() {
        return;
    }
    let job = unsafe { Box::from_raw(job) };
    let detached = {
        let mut g = match job.thread.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        g.take()
    };
    drop(detached);
}

/// Start a `find_nearest` in a background thread. Returns a job handle, or
/// NULL on immediate error (bad args). The Float32 query vector is copied
/// into the thread before the call returns, so `query_ptr` may be freed
/// immediately after this function returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_find_nearest_start(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    field: *const c_char,
    query_ptr: *const f32,
    query_len: usize,
    top_k: usize,
    filter_json: *const c_char,
) -> *mut TalaDbJob {
    clear_last_error();
    if handle.is_null() {
        set_last_error("database handle is null".into());
        return std::ptr::null_mut();
    }
    let col = match unsafe { cstr_to_string(collection) } {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    let fld = match unsafe { cstr_to_string(field) } {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    if query_ptr.is_null() && query_len != 0 {
        set_last_error("query vector pointer is null but its length is non-zero".into());
        return std::ptr::null_mut();
    }
    // Copy the query vector so the caller doesn't have to keep it alive.
    let query_owned: Vec<f32> = if query_len == 0 {
        Vec::new()
    } else {
        unsafe { slice::from_raw_parts(query_ptr, query_len) }.to_vec()
    };
    let filter_owned: Option<String> = if filter_json.is_null() {
        None
    } else {
        unsafe { cstr_to_string(filter_json) }
    };

    spawn_job(handle, move |h| {
        run_find_nearest(
            &h.db,
            &col,
            &fld,
            &query_owned,
            top_k,
            filter_owned.as_deref(),
        )
        .map_err(|e| e.to_string())
    })
}

/// Start a `find` in a background thread. Returns a job handle, or NULL on
/// immediate error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn taladb_find_start(
    handle: *mut TalaDbHandle,
    collection: *const c_char,
    filter_json: *const c_char,
) -> *mut TalaDbJob {
    clear_last_error();
    if handle.is_null() {
        set_last_error("database handle is null".into());
        return std::ptr::null_mut();
    }
    let col = match unsafe { cstr_to_string(collection) } {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    let filter_owned = unsafe { cstr_to_string(filter_json) }.unwrap_or_else(|| "{}".to_string());

    spawn_job(handle, move |h| {
        let collection = h.db.collection(&col).map_err(|e| e.to_string())?;
        let filter = parse_filter(&filter_owned)?;
        let docs = collection.find(filter).map_err(|e| e.to_string())?;
        let json_docs: Vec<serde_json::Value> = docs.iter().map(doc_to_json).collect();
        serde_json::to_string(&json_docs).map_err(|e| e.to_string())
    })
}

// ---------------------------------------------------------------------------
// Update helper (existing)
// ---------------------------------------------------------------------------

fn parse_update(json: &str) -> Option<Update> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = v.as_object()?;
    let mut updates = Vec::new();
    if let Some(set) = obj.get("$set") {
        let pairs = set
            .as_object()?
            .iter()
            .map(|(k, v)| (k.clone(), json_to_value(v)))
            .collect();
        updates.push(Update::Set(pairs));
    }
    if let Some(unset) = obj.get("$unset") {
        let keys = unset.as_object()?.keys().cloned().collect();
        updates.push(Update::Unset(keys));
    }
    if let Some(inc) = obj.get("$inc") {
        let pairs = inc
            .as_object()?
            .iter()
            .map(|(k, v)| (k.clone(), json_to_value(v)))
            .collect();
        updates.push(Update::Inc(pairs));
    }
    if let Some(push) = obj.get("$push") {
        let map = push.as_object()?;
        updates.extend(
            map.iter()
                .map(|(k, v)| Update::Push(k.clone(), json_to_value(v))),
        );
    }
    if let Some(pull) = obj.get("$pull") {
        let map = pull.as_object()?;
        updates.extend(
            map.iter()
                .map(|(k, v)| Update::Pull(k.clone(), json_to_value(v))),
        );
    }
    if obj
        .keys()
        .any(|k| !matches!(k.as_str(), "$set" | "$unset" | "$inc" | "$push" | "$pull"))
    {
        return None;
    }
    match updates.len() {
        0 => None,
        1 => Some(updates.remove(0)),
        _ => Some(Update::Many(updates)),
    }
}

// ---------------------------------------------------------------------------
// Tests (host-side; pure parsing logic, no JSI/FFI required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_filter_null_and_empty_object_match_all() {
        assert!(matches!(parse_filter("null"), Ok(Filter::All)));
        assert!(matches!(parse_filter("{}"), Ok(Filter::All)));
    }

    #[test]
    fn parse_filter_invalid_json_is_error_not_match_all() {
        // Regression: this used to degrade to Filter::All, so a malformed
        // filter passed to deleteMany would wipe the whole collection.
        assert!(parse_filter("{not json").is_err());
    }

    #[test]
    fn parse_filter_unknown_operator_is_error() {
        assert!(parse_filter(r#"{"status":{"$qe":"x"}}"#).is_err());
    }

    #[test]
    fn parse_filter_empty_operator_object_is_error() {
        assert!(parse_filter(r#"{"a":{}}"#).is_err());
    }

    #[test]
    fn parse_filter_valid_operators_parse() {
        assert!(matches!(
            parse_filter(r#"{"age":{"$gte":18}}"#),
            Ok(Filter::Gte(..))
        ));
        assert!(matches!(
            parse_filter(r#"{"name":"Alice"}"#),
            Ok(Filter::Eq(..))
        ));
    }

    #[test]
    fn logical_operator_keeps_sibling_predicates() {
        let filter = parse_filter(r#"{"tenant":"a","$or":[{"x":1},{"x":2}]}"#).unwrap();
        assert!(matches!(filter, Filter::And(parts) if parts.len() == 2));
    }

    #[test]
    fn update_keeps_all_operators_and_array_fields() {
        let update = parse_update(r#"{"$set":{"a":1},"$push":{"x":1,"y":2}}"#).unwrap();
        assert!(matches!(update, Update::Many(parts) if parts.len() == 3));
    }

    #[test]
    fn open_with_config_ignores_unknown_blocks() {
        // The `webhook` block belongs to the TypeScript client, which reads the
        // same config object. This binding must skip it, not reject the open.
        let dir = tempfile::tempdir().unwrap();
        let path = CString::new(dir.path().join("cfg.db").to_str().unwrap()).unwrap();
        let config = CString::new(
            r#"{"durability":{"flush_every_write":false},"webhook":{"enabled":true,"endpoint":"https://x.test/h"}}"#,
        )
        .unwrap();
        let handle = unsafe { taladb_open_with_config(path.as_ptr(), config.as_ptr()) };
        assert!(!handle.is_null());
        unsafe { taladb_close(handle) };
    }

    #[test]
    fn open_with_config_rejects_malformed_json() {
        let path = CString::new("unused.db").unwrap();
        let config = CString::new("{not json").unwrap();
        let handle = unsafe { taladb_open_with_config(path.as_ptr(), config.as_ptr()) };
        assert!(handle.is_null());
        let error = unsafe { CStr::from_ptr(taladb_last_error()) }.to_string_lossy();
        assert!(error.contains("invalid config JSON"), "{error}");
    }

    // -----------------------------------------------------------------------
    // Error-reporting contract
    // -----------------------------------------------------------------------

    /// Open a scratch database, returning the handle and the tempdir that must
    /// outlive it.
    fn open_temp_db() -> (*mut TalaDbHandle, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = CString::new(dir.path().join("t.db").to_str().unwrap()).unwrap();
        let handle = unsafe { taladb_open(path.as_ptr()) };
        assert!(!handle.is_null(), "test database should open");
        (handle, dir)
    }

    fn last_error() -> Option<String> {
        let ptr = taladb_last_error();
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned())
        }
    }

    fn cstr(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    /// The bug: `LAST_ERROR` was only ever written, never cleared, so after any
    /// successful call `taladb_last_error()` still returned the message from
    /// whatever failed earlier on this thread.
    #[test]
    fn success_clears_the_previous_error() {
        let (handle, _dir) = open_temp_db();
        let col = cstr("books");

        // Fail once to put a message in the slot.
        let bad = cstr("not a json object");
        assert!(unsafe { taladb_insert(handle, col.as_ptr(), bad.as_ptr()) }.is_null());
        assert!(last_error().is_some(), "failure must report");

        // Now succeed. The stale message must be gone.
        let doc = cstr(r#"{"title":"Noli Me Tangere"}"#);
        let id = unsafe { taladb_insert(handle, col.as_ptr(), doc.as_ptr()) };
        assert!(!id.is_null(), "insert should succeed");
        assert_eq!(
            last_error(),
            None,
            "a successful call must not leave the previous call's error readable"
        );

        unsafe {
            taladb_free_string(id);
            taladb_close(handle);
        }
    }

    /// Reading the error must not itself clear it — JS calls it once and may
    /// read the pointer twice.
    #[test]
    fn reading_the_error_is_idempotent() {
        let (handle, _dir) = open_temp_db();
        let col = cstr("books");
        let bad = cstr("[]");
        assert!(unsafe { taladb_insert(handle, col.as_ptr(), bad.as_ptr()) }.is_null());

        let first = last_error().expect("error expected");
        let second = last_error().expect("error must still be readable");
        assert_eq!(first, second);

        unsafe { taladb_close(handle) };
    }

    /// Every argument-rejection path used to return NULL/-1 with no message at
    /// all, so JS saw a failure it could not explain.
    #[test]
    fn null_arguments_report_a_message() {
        let col = cstr("books");
        let doc = cstr("{}");

        assert!(
            unsafe { taladb_insert(std::ptr::null_mut(), col.as_ptr(), doc.as_ptr()) }.is_null()
        );
        assert!(
            last_error().unwrap().contains("handle is null"),
            "a null handle must say so"
        );

        let (handle, _dir) = open_temp_db();
        assert!(
            unsafe { taladb_insert(handle, std::ptr::null(), doc.as_ptr()) }.is_null()
        );
        assert!(last_error().unwrap().contains("null"));

        // Integer-returning functions take the same path.
        assert_eq!(unsafe { taladb_compact(std::ptr::null_mut()) }, -1);
        assert!(last_error().unwrap().contains("handle is null"));

        unsafe { taladb_close(handle) };
    }

    /// The bug: `insert_many` used `filter_map`, so a document that failed to
    /// parse was dropped and the returned id array came back short — with no
    /// indication which input it corresponded to.
    #[test]
    fn insert_many_rejects_the_batch_and_names_the_bad_index() {
        let (handle, _dir) = open_temp_db();
        let col = cstr("books");
        let docs = cstr(r#"[{"title":"a"},"not an object",{"title":"c"}]"#);

        let result = unsafe { taladb_insert_many(handle, col.as_ptr(), docs.as_ptr()) };
        assert!(result.is_null(), "a bad element must fail the whole call");

        let err = last_error().expect("must report");
        assert!(err.contains("index 1"), "must name the offending index: {err}");

        // And nothing was written — no partial batch.
        let filter = cstr("{}");
        let count = unsafe { taladb_count(handle, col.as_ptr(), filter.as_ptr()) };
        assert_eq!(count, 0, "a rejected batch must not insert anything");

        unsafe { taladb_close(handle) };
    }

    /// The all-or-nothing rule must not cost the happy path.
    #[test]
    fn insert_many_still_inserts_a_valid_batch() {
        let (handle, _dir) = open_temp_db();
        let col = cstr("books");
        let docs = cstr(r#"[{"title":"a"},{"title":"b"},{"title":"c"}]"#);

        let result = unsafe { taladb_insert_many(handle, col.as_ptr(), docs.as_ptr()) };
        assert!(!result.is_null());
        let ids: Vec<String> =
            serde_json::from_str(&unsafe { CStr::from_ptr(result) }.to_string_lossy()).unwrap();
        assert_eq!(ids.len(), 3, "every input document gets an id back");
        assert_eq!(last_error(), None);

        unsafe {
            taladb_free_string(result);
            taladb_close(handle);
        }
    }
}
