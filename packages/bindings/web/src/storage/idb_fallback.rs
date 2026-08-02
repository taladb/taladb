//! IndexedDB fallback storage for browsers without OPFS (e.g. cross-origin iframes).
//!
//! Strategy
//! --------
//! Serialize the entire redb in-memory database to a `Uint8Array` (via
//! `WorkerDB::export_snapshot`) and store it in an IndexedDB object store named
//! `"snapshots"` under a key equal to the `db_name`.  On next open, call
//! `idb_load_snapshot` to restore the bytes and pass them to
//! `WorkerDB::open_with_snapshot`.
//!
//! IDB schema
//! ----------
//! Database name : `"taladb"`
//! Object store  : `"snapshots"`  (out-of-line keys — key == db_name string)
//! Version       : 1
//!
//! Implementation note
//! --------------------
//! All IndexedDB calls are made through `js_sys::Reflect` rather than typed
//! `web-sys` IDB bindings.  This keeps the `web-sys` feature list small and
//! avoids the verbose callback-wrapping required by the typed API.
//! The trade-off is less compile-time type safety for the IDB calls, which is
//! acceptable given the narrow, well-tested surface here.

use js_sys::{Promise, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

const IDB_DB_NAME: &str = "taladb";
const IDB_STORE: &str = "snapshots";
const IDB_VERSION: f64 = 1.0;

// ---------------------------------------------------------------------------
// Public WASM exports
// ---------------------------------------------------------------------------

/// Load a previous database snapshot from IndexedDB.
/// Returns `None` if no snapshot exists yet (first open) or IDB is unavailable.
#[wasm_bindgen]
pub async fn idb_load_snapshot(db_name: &str) -> Option<Vec<u8>> {
    let result = load_inner(db_name).await.ok()?;
    if result.is_null() || result.is_undefined() {
        return None;
    }
    let arr = Uint8Array::new(&result);
    Some(arr.to_vec())
}

/// Persist a database snapshot to IndexedDB.
/// Returns `true` on success, `false` on any failure.
#[wasm_bindgen]
pub async fn idb_save_snapshot(db_name: &str, data: &[u8]) -> bool {
    save_inner(db_name, data).await.is_ok()
}

// ---------------------------------------------------------------------------
// Internal async helpers
// ---------------------------------------------------------------------------

async fn load_inner(db_name: &str) -> Result<JsValue, JsValue> {
    let idb = open_idb().await?;

    // tx = idb.transaction("snapshots", "readonly")
    let tx = call_method(
        &idb,
        "transaction",
        &[
            &JsValue::from_str(IDB_STORE),
            &JsValue::from_str("readonly"),
        ],
    )?;
    // store = tx.objectStore("snapshots")
    let store = call_method(&tx, "objectStore", &[&JsValue::from_str(IDB_STORE)])?;
    // req = store.get(db_name)
    let req = call_method(&store, "get", &[&JsValue::from_str(db_name)])?;

    // Await the request (resolves with req.result)
    JsFuture::from(request_promise(&req)).await
}

async fn save_inner(db_name: &str, data: &[u8]) -> Result<(), JsValue> {
    let idb = open_idb().await?;

    // tx = idb.transaction("snapshots", "readwrite")
    let tx = call_method(
        &idb,
        "transaction",
        &[
            &JsValue::from_str(IDB_STORE),
            &JsValue::from_str("readwrite"),
        ],
    )?;
    let store = call_method(&tx, "objectStore", &[&JsValue::from_str(IDB_STORE)])?;

    // store.put(Uint8Array, db_name)
    let arr = Uint8Array::from(data);
    call_method(&store, "put", &[&arr, &JsValue::from_str(db_name)])?;

    // Await transaction completion
    JsFuture::from(transaction_promise(&tx)).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Open the IDB database, creating the object store on first run
// ---------------------------------------------------------------------------

async fn open_idb() -> Result<JsValue, JsValue> {
    let global = js_sys::global();

    // indexedDB may be on the global (Worker) or on window (main thread).
    // Use Reflect::get to avoid requiring the IdbFactory web-sys feature flag.
    let idb_factory = js_sys::Reflect::get(&global, &"indexedDB".into())
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null())
        .or_else(|| {
            // Fallback: look up window.indexedDB for main-thread contexts
            js_sys::Reflect::get(&global, &"window".into())
                .ok()
                .filter(|v| !v.is_undefined() && !v.is_null())
                .and_then(|win| js_sys::Reflect::get(&win, &"indexedDB".into()).ok())
                .filter(|v| !v.is_undefined() && !v.is_null())
        })
        .ok_or_else(|| JsValue::from_str("IndexedDB not available in this context"))?;

    // open_req = indexedDB.open("taladb", 1)
    let open_req = call_method(
        &idb_factory,
        "open",
        &[
            &JsValue::from_str(IDB_DB_NAME),
            &JsValue::from_f64(IDB_VERSION),
        ],
    )?;

    // onupgradeneeded: create the "snapshots" object store on first open
    let open_req_c = open_req.clone();
    let on_upgrade = Closure::once(move |_: JsValue| {
        if let Ok(result) = js_sys::Reflect::get(&open_req_c, &"result".into()) {
            let _ = call_method(
                &result,
                "createObjectStore",
                &[&JsValue::from_str(IDB_STORE)],
            );
        }
    });
    js_sys::Reflect::set(&open_req, &"onupgradeneeded".into(), on_upgrade.as_ref())?;
    on_upgrade.forget();

    // Await the open request (resolves with the IdbDatabase)
    JsFuture::from(request_promise(&open_req)).await
}

// ---------------------------------------------------------------------------
// Promise wrappers for IDB request / transaction callbacks
// ---------------------------------------------------------------------------

/// Wrap an IDB request's onsuccess/onerror as a Promise that resolves with
/// `request.result`.
fn request_promise(req: &JsValue) -> Promise {
    let req = req.clone();
    Promise::new(&mut |resolve, reject| {
        let req_s = req.clone();
        let resolve_c = resolve;

        let on_success = Closure::once(move |_: JsValue| {
            let result =
                js_sys::Reflect::get(&req_s, &"result".into()).unwrap_or(JsValue::UNDEFINED);
            let _ = resolve_c.call1(&JsValue::UNDEFINED, &result);
        });
        let on_error = Closure::once(move |_: JsValue| {
            let _ = reject.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str("IDB request failed"),
            );
        });

        js_sys::Reflect::set(&req, &"onsuccess".into(), on_success.as_ref()).unwrap();
        js_sys::Reflect::set(&req, &"onerror".into(), on_error.as_ref()).unwrap();
        on_success.forget();
        on_error.forget();
    })
}

/// Wrap an IDB transaction's oncomplete/onerror as a Promise.
fn transaction_promise(tx: &JsValue) -> Promise {
    let tx = tx.clone();
    Promise::new(&mut |resolve, reject| {
        let on_complete = Closure::once(move |_: JsValue| {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        });
        let on_error = Closure::once(move |_: JsValue| {
            let _ = reject.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str("IDB transaction failed"),
            );
        });

        js_sys::Reflect::set(&tx, &"oncomplete".into(), on_complete.as_ref()).unwrap();
        js_sys::Reflect::set(&tx, &"onerror".into(), on_error.as_ref()).unwrap();
        on_complete.forget();
        on_error.forget();
    })
}

// ---------------------------------------------------------------------------
// Helper: invoke a method on a JS object via Reflect
// ---------------------------------------------------------------------------

fn call_method(obj: &JsValue, method: &str, args: &[&JsValue]) -> Result<JsValue, JsValue> {
    let func: js_sys::Function = js_sys::Reflect::get(obj, &method.into())?
        .dyn_into()
        .map_err(|_| JsValue::from_str(&format!("IDB: '{method}' is not a function")))?;
    let js_args = js_sys::Array::new();
    for arg in args {
        js_args.push(arg);
    }
    func.apply(obj, &js_args)
}
