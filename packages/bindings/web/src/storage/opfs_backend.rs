//! OPFS redb StorageBackend — uses `FileSystemSyncAccessHandle` for byte-level I/O.
//!
//! `FileSystemSyncAccessHandle` provides **synchronous** read/write/truncate/flush
//! operations and is only available in **dedicated** or **shared** worker threads.
//! It is the correct primitive for running redb on top of OPFS: no async, no
//! round-trips to the main thread, byte-addressable random I/O.
//!
//! Safety
//! ------
//! WASM is single-threaded. `FileSystemSyncAccessHandle` (a JS object) is not
//! `Send` by Rust's type system, but since there is exactly one thread in the
//! WASM runtime, the `unsafe impl Send + Sync` below is sound.

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

// WASM is single-threaded — this is safe.
unsafe impl Send for OpfsBackend {}
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
        OpfsBackend {
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

    fn js_read(&self, offset: u64, len: usize) -> Result<Vec<u8>, io::Error> {
        // Grow the scratch buffer only when a read outgrows it; steady state
        // reuses the same JS allocation for every page.
        let mut scratch = self.scratch.borrow_mut();
        if (scratch.length() as usize) < len {
            *scratch = Uint8Array::new_with_length(len as u32);
        }
        // `read` fills from index 0, so a longer scratch buffer would report a
        // read longer than requested — subarray keeps the length exact.
        let view = scratch.subarray(0, len as u32);

        let opts = self.opts_at(offset)?;
        let result = self
            .read_fn
            .call2(self.handle.as_ref(), &view, opts)
            .map_err(|e| io_err(&format!("opfs read: {:?}", e)))?;

        let n = result
            .as_f64()
            .ok_or_else(|| io_err("opfs read: non-numeric return"))? as usize;
        let mut out = vec![0u8; n];
        view.subarray(0, n as u32).copy_to(&mut out[..n]);
        Ok(out)
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
            .map_err(|e| io_err(&format!("opfs write: {:?}", e)))?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// redb StorageBackend impl
// ---------------------------------------------------------------------------

impl redb::StorageBackend for OpfsBackend {
    fn len(&self) -> Result<u64, io::Error> {
        self.handle
            .get_size()
            .map(|s| s as u64)
            .map_err(|e| io_err(&format!("opfs get_size: {:?}", e)))
    }

    fn read(&self, offset: u64, len: usize) -> Result<Vec<u8>, io::Error> {
        self.js_read(offset, len)
    }

    fn set_len(&self, new_len: u64) -> Result<(), io::Error> {
        self.handle
            .truncate_with_f64(new_len as f64)
            .map_err(|e| io_err(&format!("opfs truncate: {:?}", e)))
    }

    fn sync_data(&self, _eventual: bool) -> Result<(), io::Error> {
        self.handle
            .flush()
            .map_err(|e| io_err(&format!("opfs flush: {:?}", e)))
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
