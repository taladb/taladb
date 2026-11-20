/**
 * TalaDB React Native — Android JSI bridge.
 *
 * The module loads `libtaladb_ffi.so` (the Rust C FFI crate compiled via
 * `cargo ndk`) and installs the C++ JSI HostObject into the Hermes/JSC
 * runtime via `installJSIBindings()`.
 *
 * Build setup
 * -----------
 *  1. Cross-compile: `cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 build --release`
 *     Output: `target/<triple>/release/libtaladb_ffi.so`
 *  2. Copy each .so into `android/src/main/jniLibs/<ABI>/libtaladb_ffi.so`
 *  3. `android/CMakeLists.txt` compiles `TalaDBHostObject.cpp` and links the .so.
 *
 * Runtime flow
 * ------------
 *  React Native calls `initialize(dbName)` once.
 *  The module resolves the DB path, then calls the native `nativeInstall()`
 *  function which opens the Rust database and installs `global.__TalaDB__`
 *  as a JSI HostObject. All subsequent CRUD calls go through JSI directly.
 */
package com.taladb

import com.facebook.react.bridge.*
import com.facebook.react.module.annotations.ReactModule

@ReactModule(name = TalaDBModule.NAME)
class TalaDBModule(private val reactContext: ReactApplicationContext) :
    NativeTalaDBSpec(reactContext) {

    companion object {
        const val NAME = "TalaDB"

        init {
            // libtaladb_ffi.so is the pre-built Rust crate (jniLibs/).
            // libtaladb_jsi.so wraps the C++ JSI HostObject and links taladb_ffi.
            // Load ffi first so the dynamic linker can resolve it when jsi loads.
            System.loadLibrary("taladb_ffi")
            System.loadLibrary("taladb_jsi")
        }
    }

    override fun getName() = NAME

    // -----------------------------------------------------------------------
    // JNI — implemented in TalaDBHostObject.cpp (via CMakeLists.txt)
    // -----------------------------------------------------------------------

    /**
     * Open the database at [dbPath] and install `global.__TalaDB__` into the
     * JSI runtime identified by [jsContextNativePtr].
     * Called once from [initialize].
     */
    private external fun nativeInstall(jsContextNativePtr: Long, dbPath: String, configJson: String?): String?
    private external fun nativeClose(jsContextNativePtr: Long)

    // -----------------------------------------------------------------------
    // TurboModule: initialize(dbName, configJson?) → Promise<void>
    // -----------------------------------------------------------------------

    override fun initialize(dbName: String, configJson: String?, promise: Promise) {
        try {
            // Private app storage excluded from Android Auto Backup. The OS
            // file-based-encryption layer protects it while the device is locked.
            val dbFile = java.io.File(reactContext.noBackupFilesDir, dbName)
            val legacyFile = java.io.File(reactContext.filesDir, dbName)
            if (!dbFile.exists() && legacyFile.exists() && !legacyFile.renameTo(dbFile)) {
                throw java.io.IOException("failed to migrate TalaDB into no-backup storage")
            }
            val legacySalt = java.io.File(legacyFile.absolutePath + ".taladb-salt")
            val saltFile = java.io.File(dbFile.absolutePath + ".taladb-salt")
            if (!saltFile.exists() && legacySalt.exists() && !legacySalt.renameTo(saltFile)) {
                throw java.io.IOException("failed to migrate TalaDB encryption salt")
            }
            val dbPath = dbFile.absolutePath

            // javaScriptContextHolder.get() returns the raw jsi::Runtime* pointer.
            val jsContextPtr = reactContext.javaScriptContextHolder!!.get()
            if (jsContextPtr == 0L) {
                promise.reject("TALADB_NO_RUNTIME", "JSI runtime is not available")
                return
            }

            // Install on the JS thread
            reactContext.runOnJSQueueThread {
                try {
                    val error = nativeInstall(jsContextPtr, dbPath, configJson)
                    if (error == null) promise.resolve(null)
                    else promise.reject("TALADB_INSTALL_ERROR", error)
                } catch (e: Exception) {
                    promise.reject("TALADB_INSTALL_ERROR", e.message, e)
                }
            }
        } catch (e: Exception) {
            promise.reject("TALADB_INIT_ERROR", e.message, e)
        }
    }

    // -----------------------------------------------------------------------
    // TurboModule: close() → Promise<void>
    // -----------------------------------------------------------------------

    override fun close(promise: Promise) {
        // nativeClose closes the database through the HostObject, then clears
        // `global.__TalaDB__`. Clearing the global alone would leave the file
        // open until the JS GC collected the HostObject.
        try {
            reactContext.runOnJSQueueThread {
                try {
                    val ptr = reactContext.javaScriptContextHolder?.get() ?: 0L
                    if (ptr != 0L) nativeClose(ptr)
                    promise.resolve(null)
                } catch (e: Exception) {
                    promise.reject("TALADB_CLOSE_ERROR", e.message, e)
                }
            }
        } catch (e: Exception) {
            promise.reject("TALADB_CLOSE_ERROR", e.message, e)
        }
    }

    // -----------------------------------------------------------------------
    // All synchronous CRUD methods are handled by the JSI HostObject.
    // The stubs below satisfy the TurboModule Codegen spec (NativeTalaDB.ts)
    // but are never invoked at runtime — JS calls global.__TalaDB__ directly.
    // -----------------------------------------------------------------------

    override fun insert(collection: String, doc: ReadableMap): String = ""
    override fun insertMany(collection: String, docs: ReadableArray): WritableArray =
        WritableNativeArray()
    override fun find(collection: String, filter: ReadableMap?): WritableArray =
        WritableNativeArray()
    override fun findOne(collection: String, filter: ReadableMap?): WritableMap? = null
    override fun updateOne(collection: String, filter: ReadableMap, update: ReadableMap): Boolean = false
    override fun updateMany(collection: String, filter: ReadableMap, update: ReadableMap): Double = 0.0
    override fun deleteOne(collection: String, filter: ReadableMap): Boolean = false
    override fun deleteMany(collection: String, filter: ReadableMap): Double = 0.0
    override fun count(collection: String, filter: ReadableMap?): Double = 0.0
    override fun createIndex(collection: String, field: String) {}
    override fun dropIndex(collection: String, field: String) {}
    override fun createFtsIndex(collection: String, field: String) {}
    override fun dropFtsIndex(collection: String, field: String) {}

    // Vector index + findNearest — JSI-only; stubs satisfy Codegen.

    override fun createVectorIndex(
        collection: String,
        field: String,
        dimensions: Double,
        opts: ReadableMap?,
    ) {}
    override fun dropVectorIndex(collection: String, field: String) {}
    override fun upgradeVectorIndex(collection: String, field: String) {}
    override fun findNearest(
        collection: String,
        field: String,
        query: ReadableArray,
        topK: Double,
        filter: ReadableMap?,
    ): WritableArray = WritableNativeArray()

    override fun findNearestAsync(
        collection: String,
        field: String,
        query: ReadableArray,
        topK: Double,
        filter: ReadableMap?,
        promise: Promise,
    ) {
        promise.resolve(WritableNativeArray())
    }

    override fun findAsync(collection: String, filter: ReadableMap?, promise: Promise) {
        promise.resolve(WritableNativeArray())
    }
}
