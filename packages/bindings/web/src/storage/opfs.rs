//! OPFS (Origin Private File System) persistent storage for TalaDB WASM.
//!
//! Strategy
//! --------
//! TalaDB's Rust core (redb) runs in-memory inside the WASM module. To persist
//! data across page reloads we serialise the entire in-memory database snapshot
//! to a `Uint8Array` and write it to an OPFS file.
//!
//! Snapshot lifecycle:
//!   1. `opfs_open(db_name)`  — load the last snapshot from OPFS into memory.
//!   2. After each write      — call `opfs_flush(db_name, snapshot_bytes)`.
//!   3. On next page load     — repeat step 1.
//!
//! Why not SharedWorker / FileSystemSyncAccessHandle?
//! ---------------------------------------------------
//! `FileSystemSyncAccessHandle` must run on a dedicated worker thread, which
//! requires a DedicatedWorker and `postMessage` round-trips. For most client
//! apps the snapshot-flush approach (this file) is simpler and fast enough —
//! a 1 MB snapshot flushes in < 5 ms on modern hardware.
//!
//! Large databases (> 50 MB) should migrate to the worker-based approach.

use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

// ---------------------------------------------------------------------------
// Availability check
// ---------------------------------------------------------------------------

/// Returns true if OPFS is available in the current browser context.
/// Always returns false in Workers without storage access.
#[wasm_bindgen]
pub async fn is_opfs_available() -> bool {
    let window = match web_sys::window() {
        Some(w) => w,
        None => return false,
    };
    let navigator = window.navigator();
    let storage = navigator.storage();
    JsFuture::from(storage.get_directory()).await.is_ok()
}

// ---------------------------------------------------------------------------
// OPFS open — load a snapshot
// ---------------------------------------------------------------------------

/// Load the last persisted database snapshot from OPFS.
/// Returns `None` if the file does not exist yet (first open).
#[wasm_bindgen]
pub async fn opfs_load_snapshot(db_name: &str) -> Option<Vec<u8>> {
    let dir = opfs_root().await?;
    let file_name = opfs_file_name(db_name);

    // getFileHandle with create:false — returns an error if not found
    let opts = js_sys::Object::new();
    js_sys::Reflect::set(&opts, &"create".into(), &JsValue::FALSE).ok()?;
    let file_handle: web_sys::FileSystemFileHandle =
        JsFuture::from(dir.get_file_handle_with_options(&file_name, &opts.unchecked_into()))
            .await
            .ok()?
            .dyn_into()
            .ok()?;

    let file: web_sys::File = JsFuture::from(file_handle.get_file())
        .await
        .ok()?
        .dyn_into()
        .ok()?;

    let array_buffer: ArrayBuffer = JsFuture::from(file.array_buffer())
        .await
        .ok()?
        .dyn_into()
        .ok()?;

    let uint8 = Uint8Array::new(&array_buffer);
    Some(uint8.to_vec())
}

// ---------------------------------------------------------------------------
// OPFS flush — persist a snapshot
// ---------------------------------------------------------------------------

/// Persist a database snapshot to OPFS.
/// Creates the file on first call. Subsequent calls overwrite atomically.
#[wasm_bindgen]
pub async fn opfs_flush_snapshot(db_name: &str, data: &[u8]) -> bool {
    opfs_flush_inner(db_name, data).await.is_ok()
}

async fn opfs_flush_inner(db_name: &str, data: &[u8]) -> Result<(), JsValue> {
    let dir = opfs_root()
        .await
        .ok_or(JsValue::from_str("opfs unavailable"))?;
    let file_name = opfs_file_name(db_name);

    // getFileHandle with create:true — creates if missing
    let opts = web_sys::FileSystemGetFileOptions::new();
    opts.set_create(true);
    let file_handle: web_sys::FileSystemFileHandle =
        JsFuture::from(dir.get_file_handle_with_options(&file_name, &opts))
            .await?
            .dyn_into()?;

    // createWritable — async writable stream
    let writable: web_sys::FileSystemWritableFileStream =
        JsFuture::from(file_handle.create_writable())
            .await?
            .dyn_into()?;

    // Write via Reflect to avoid web-sys overload naming fragility.
    let uint8 = Uint8Array::from(data);
    let writable_js: &JsValue = writable.as_ref();
    let write_fn: js_sys::Function = js_sys::Reflect::get(writable_js, &"write".into())?
        .dyn_into()
        .map_err(|_| JsValue::from_str("writable.write is not a function"))?;
    JsFuture::from(
        write_fn
            .call1(writable_js, &uint8)?
            .dyn_into::<js_sys::Promise>()?,
    )
    .await?;

    // Close flushes and commits atomically
    JsFuture::from(writable.close()).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// OPFS delete — wipe a database file
// ---------------------------------------------------------------------------

/// Delete the OPFS snapshot file for `db_name`.
/// No-op if the file does not exist.
#[wasm_bindgen]
pub async fn opfs_delete_snapshot(db_name: &str) -> bool {
    let dir = match opfs_root().await {
        Some(d) => d,
        None => return false,
    };
    JsFuture::from(dir.remove_entry(&opfs_file_name(db_name)))
        .await
        .is_ok()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn opfs_root() -> Option<web_sys::FileSystemDirectoryHandle> {
    let window = web_sys::window()?;
    let storage = window.navigator().storage();
    JsFuture::from(storage.get_directory())
        .await
        .ok()?
        .dyn_into()
        .ok()
}

fn opfs_file_name(db_name: &str) -> String {
    // Sanitise: replace slashes with underscores so it's always a flat filename
    format!("taladb_{}.bin", db_name.replace(['/', '\\', ':'], "_"))
}
