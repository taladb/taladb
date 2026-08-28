//! OPFS redb StorageBackend — uses `FileSystemSyncAccessHandle` for byte-level I/O.
//!
//! `FileSystemSyncAccessHandle` provides **synchronous** read/write/truncate/flush
//! operations and is only available in **dedicated** or **shared** worker threads.
//! It is the correct primitive for running redb on top of OPFS: no async, no
//! round-trips to the main thread, byte-addressable random I/O.
//!
//! Safety
//! ------
//! `FileSystemSyncAccessHandle` is a JS object and is not `Send`, and this type
//! also holds a `RefCell`, whose borrow counter is not atomic. Both are fine on
//! wasm32 without threads, where there is exactly one thread and no two borrows
//! can overlap — so `unsafe impl Send + Sync` below is sound *there*.
//!
//! That is a claim about the target, not about the code, so it is enforced by the
//! target: the impls are gated on `target_arch = "wasm32"` and
//! `not(target_feature = "atomics")`, and enabling wasm threads is a
//! `compile_error!` rather than a silent race. The gate is what keeps the
//! justification honest — it used to be a comment, which meant building this
//! module for any other target, or for wasm-with-threads, would have compiled a
//! `Sync` impl over a non-atomic `RefCell`.

use std::cell::RefCell;
use std::io;

use js_sys::Uint8Array;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::FileSystemSyncAccessHandle;

// ---------------------------------------------------------------------------
// OpfsBackend
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct OpfsBackend {
    handle: FileSystemSyncAccessHandle,
    /// `handle.read` / `handle.write`, resolved once at construction.
    ///
    /// redb issues many small reads while walking a B-tree, and every one of
    /// them used to do two `Reflect` property lookups (each allocating a JS
    /// string for the property name) before the call could even be made. Since
    /// this is the *entire* storage layer in the browser, that overhead landed
    /// on both cold start and every query afterwards.
    read_fn: js_sys::Function,
    write_fn: js_sys::Function,
    /// The `"at"` property key, allocated once rather than per I/O.
    at_key: JsValue,
    /// Reusable `{ at: offset }` options object — mutated in place per call
    /// instead of allocating a fresh JS object for every read and write.
    opts: js_sys::Object,
    /// Scratch JS buffer for reads, grown on demand and reused. Avoids a
    /// `Uint8Array` allocation per page read.
    scratch: RefCell<Uint8Array>,
}

// FileSystemSyncAccessHandle is a JS object and doesn't implement Debug.
impl std::fmt::Debug for OpfsBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpfsBackend").finish_non_exhaustive()
    }
}

// If wasm threads are ever enabled, the single-thread argument below stops
// holding and this module needs real synchronisation around `scratch`. Fail
// loudly here rather than let the reader hunt a missing-trait error.
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
compile_error!(
    "OpfsBackend's Send/Sync impls assume a single-threaded wasm runtime, which      `target_feature = \"atomics\"` breaks: `scratch` is a RefCell with a      non-atomic borrow counter, and `FileSystemSyncAccessHandle` is not Send.      Replace the RefCell with a real lock and re-justify both impls before      enabling wasm threads."
);

// SAFETY: on wasm32 without the `atomics` target feature the runtime has exactly
// one thread, so no two references to an `OpfsBackend` can be used concurrently:
// the JS handle is never touched from another thread, and `scratch.borrow_mut()`
// can never race. Both conditions are enforced by the `cfg` rather than assumed
// — see the module-level Safety section.
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
unsafe impl Send for OpfsBackend {}
// SAFETY: as above.
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
unsafe impl Sync for OpfsBackend {}

#[allow(dead_code)]
impl OpfsBackend {
    /// Wrap an existing `FileSystemSyncAccessHandle`.
    /// Call `OpfsBackend::open(db_name)` instead of constructing directly.
    ///
    /// Panics only if the handle is not a `FileSystemSyncAccessHandle` — the
    /// caller obtained it from `createSyncAccessHandle`, so `read`/`write` are
    /// guaranteed present; resolving them here rather than per I/O is the whole
    /// point.
    pub fn from_handle(handle: FileSystemSyncAccessHandle) -> Self {
        // Reflect (not the typed web-sys methods) because the method-name
        // overloads for these two shift between web-sys versions.
        let handle_js: &JsValue = handle.as_ref();
        let read_fn: js_sys::Function = js_sys::Reflect::get(handle_js, &"read".into())
            .expect("opfs: sync access handle has no read method")
            .dyn_into()
            .expect("opfs: read is not a function");
        let write_fn: js_sys::Function = js_sys::Reflect::get(handle_js, &"write".into())
            .expect("opfs: sync access handle has no write method")
            .dyn_into()
            .expect("opfs: write is not a function");
        Self {
            handle,
            read_fn,
            write_fn,
            at_key: JsValue::from_str("at"),
            opts: js_sys::Object::new(),
            scratch: RefCell::new(Uint8Array::new_with_length(0)),
        }
    }

    // ------------------------------------------------------------------
    // Low-level helpers
    // ------------------------------------------------------------------

    /// Point the shared options object at `offset` and hand it back.
    fn opts_at(&self, offset: u64) -> Result<&js_sys::Object, io::Error> {
        js_sys::Reflect::set(&self.opts, &self.at_key, &JsValue::from_f64(offset as f64))
            .map_err(|_| io_err("reflect set at"))?;
        Ok(&self.opts)
    }

    /// Fill `out` from `offset`, or fail.
    ///
    /// redb 4 requires the buffer be filled completely — a short read is an
    /// error, not a partial success — which removes the per-read `Vec` the redb
    /// 2 signature forced and makes the validation below sharper: there is now
    /// exactly one acceptable answer from JavaScript.
    fn js_read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        let len = out.len();
        if len == 0 {
            return Ok(());
        }
        // `Uint8Array` lengths are `u32`. On wasm32 `usize` is 32 bits so this is
        // lossless, but the struct also compiles for the host, where it is not.
        let len_u32 =
            u32::try_from(len).map_err(|_| io_err("opfs read: request longer than u32::MAX"))?;

        // The borrow is scoped to growing the buffer and taking the view, and
        // released before the JS call below. `subarray` hands back its own
        // `Uint8Array` handle rather than borrowing from `scratch`, so nothing
        // needs the guard afterwards — and holding a `RefCell` borrow across a
        // call out into JS is how a re-entrant caller (a polyfilled OPFS handle
        // that calls back into the module) turns into a `BorrowMutError` panic.
        let view = {
            // Grow the scratch buffer only when a read outgrows it; steady state
            // reuses the same JS allocation for every page.
            let mut scratch = self.scratch.borrow_mut();
            if scratch.length() < len_u32 {
                *scratch = Uint8Array::new_with_length(len_u32);
            }
            // `read` fills from index 0, so a longer scratch buffer would report
            // a read longer than requested — subarray keeps the length exact.
            scratch.subarray(0, len_u32)
        };

        let opts = self.opts_at(offset)?;
        let result = self
            .read_fn
            .call2(self.handle.as_ref(), &view, opts)
            .map_err(|e| io_err(&format!("opfs read: {e:?}")))?;

        // The byte count comes back from JavaScript, so it is validated rather
        // than trusted. redb has asked for exactly `len` bytes and treats
        // anything less as an error, so a short read must be reported here
        // rather than papered over: a well-behaved
        // `FileSystemSyncAccessHandle` returns `len`, and a polyfill or shimmed
        // handle returning NaN, a negative, or an over-long count would
        // otherwise become a silent truncation, a heap-exhausting allocation, or
        // a `copy_to` length-mismatch panic. A trap in the storage layer is the
        // worst place to take one.
        let raw = result
            .as_f64()
            .ok_or_else(|| io_err("opfs read: non-numeric return"))?;
        if raw != f64::from(len_u32) {
            return Err(io_err(&format!(
                "opfs read: handle returned {raw} bytes for a {len}-byte read"
            )));
        }
        view.copy_to(out);
        Ok(())
    }

    fn js_write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        // SAFETY: `Uint8Array::view` aliases WASM linear memory rather than
        // copying. That is sound only while nothing can grow or reallocate the
        // heap, which holds here: the view is handed straight to a synchronous
        // JS call and dropped immediately after, with no allocation in between.
        let buf = unsafe { Uint8Array::view(data) };

        let opts = self.opts_at(offset)?;
        self.write_fn
            .call2(self.handle.as_ref(), &buf, opts)
            .map_err(|e| io_err(&format!("opfs write: {e:?}")))?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// redb StorageBackend impl
// ---------------------------------------------------------------------------

// `redb::StorageBackend` is declared `Send + Sync`, so this impl only exists
// where the impls above do. On any other target the struct and its inherent
// methods still compile — which keeps the workspace's host-target clippy run
// looking at `js_read`/`js_write` — it simply cannot be handed to redb.
#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
impl redb::StorageBackend for OpfsBackend {
    fn len(&self) -> Result<u64, io::Error> {
        self.handle
            .get_size()
            .map(|s| s as u64)
            .map_err(|e| io_err(&format!("opfs get_size: {e:?}")))
    }

    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        self.js_read(offset, out)
    }

    fn set_len(&self, new_len: u64) -> Result<(), io::Error> {
        self.handle
            .truncate_with_f64(new_len as f64)
            .map_err(|e| io_err(&format!("opfs truncate: {e:?}")))
    }

    // redb 4 dropped the `eventual` argument; durability is chosen per commit
    // through `Durability` rather than per sync. The body was already ignoring it.
    fn sync_data(&self) -> Result<(), io::Error> {
        self.handle
            .flush()
            .map_err(|e| io_err(&format!("opfs flush: {e:?}")))
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        self.js_write(offset, data)
    }
}

// ---------------------------------------------------------------------------
// Helper to open an OPFS file and return a SyncAccessHandle.
// Must be called from a Worker context.
// ---------------------------------------------------------------------------

/// Open (or create) an OPFS file and return an `OpfsBackend` for redb.
///
/// This function is **async** because `getFileHandle` and `createSyncAccessHandle`
/// are both async in the OPFS API. Call it once at worker startup.
#[wasm_bindgen]
pub async fn opfs_open_backend(db_name: &str) -> Result<JsValue, JsValue> {
    // Access the OPFS root from the worker global scope
    let global = js_sys::global();
    let navigator = js_sys::Reflect::get(&global, &"navigator".into())?;
    let storage = js_sys::Reflect::get(&navigator, &"storage".into())?;
    let get_dir: js_sys::Function =
        js_sys::Reflect::get(&storage, &"getDirectory".into())?.dyn_into()?;

    let dir: web_sys::FileSystemDirectoryHandle = wasm_bindgen_futures::JsFuture::from(
        get_dir.call0(&storage)?.dyn_into::<js_sys::Promise>()?,
    )
    .await?
    .dyn_into()?;

    // getFileHandle with create:true
    let file_opts = web_sys::FileSystemGetFileOptions::new();
    file_opts.set_create(true);

    let file_name = format!("taladb_{}.redb", db_name.replace(['/', '\\', ':'], "_"));

    let file_handle: web_sys::FileSystemFileHandle = wasm_bindgen_futures::JsFuture::from(
        dir.get_file_handle_with_options(&file_name, &file_opts),
    )
    .await?
    .dyn_into()?;

    // createSyncAccessHandle — only works in workers
    let create_sync: js_sys::Function =
        js_sys::Reflect::get(&file_handle, &"createSyncAccessHandle".into())?.dyn_into()?;

    let sync_handle: FileSystemSyncAccessHandle = wasm_bindgen_futures::JsFuture::from(
        create_sync
            .call0(&file_handle)?
            .dyn_into::<js_sys::Promise>()?,
    )
    .await?
    .dyn_into()?;

    // Return the raw JsValue so JS can pass it back into OpfsBackend::from_js_handle
    Ok(sync_handle.into())
}

// ---------------------------------------------------------------------------
// Internal helper
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn io_err(msg: &str) -> io::Error {
    io::Error::other(msg)
}
