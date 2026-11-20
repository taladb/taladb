/*
 * TalaDB C FFI header.
 *
 * GENERATED FILE — do not edit. Regenerate with:
 *   packages/bindings/ffi/scripts/gen-header.sh
 * CI fails if this file and packages/bindings/ffi/src/lib.rs disagree.
 *
 * This is the stable C interface of the Rust taladb-ffi crate. Its consumers
 * are the React Native JSI HostObject and TurboModule in this repository and
 * the native Swift (taladb/taladb-swift) and Kotlin (taladb/taladb-kotlin)
 * packages, which ship as separate repositories.
 *
 * Compatibility
 * -------------
 *  TALADB_FFI_ABI_VERSION changes whenever a signature in this file changes
 *  incompatibly. A wrapper compiled against this header must check that
 *  taladb_ffi_abi_version() returns the same value before calling anything
 *  else; a mismatch means the library and header came from different releases.
 *
 * Ownership rules
 * ---------------
 *  - Strings IN  : caller-owned, UTF-8, null-terminated.
 *  - Strings OUT : heap-allocated by Rust; caller must free with
 *                  taladb_free_string().
 *  - Handles     : allocated by taladb_open(); freed by taladb_close().
 *  - Errors      : string functions return NULL; integer functions return -1,
 *                  with detail available from taladb_last_error().
 */


#ifndef TALADB_FFI_H
#define TALADB_FFI_H

#pragma once

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>


/**
 * Version of the C interface in `taladb.h`. Bumped whenever an exported
 * signature changes incompatibly, independently of the crate version.
 *
 * - 1 — through 0.11.8.
 * - 2 — the six index create/drop functions return `int32_t` instead of
 *   `void`; `taladb_call`, `taladb_ffi_abi_version`, `taladb_last_error_code`
 *   and the live-query functions (`taladb_watch`, `taladb_watch_next`,
 *   `taladb_watch_close`, `taladb_watch_with_options`) added.
 */
#define TALADB_FFI_ABI_VERSION 2

typedef struct TalaDbHandle TalaDbHandle;

/**
 * A background job handle. Opaque to the caller.
 */
typedef struct TalaDbJob TalaDbJob;

/**
 * A live query: a subscription to the documents in one collection that match
 * one filter. Opaque to the caller.
 */
typedef struct TalaDbWatch TalaDbWatch;

#ifdef __cplusplus
extern "C" {
#endif // __cplusplus

/**
 * Return the last error message as a null-terminated C string, or NULL if no error.
 *
 * The message always describes the most recent `taladb_*` call on this thread:
 * every entry point clears the slot before doing any work, so a NULL return
 * means that call succeeded rather than that it forgot to report.
 *
 * The returned pointer is valid until the next taladb_* call on this thread.
 * Do NOT free the returned string.
 */
const char *taladb_last_error(void);

/**
 * The engine's stable code for the error [`taladb_last_error`] describes —
 * `"Encryption"` for a wrong passphrase, `"InvalidFilter"`, `"DuplicateId"`,
 * and the rest of `TalaDbError::code` — or NULL when the last call succeeded
 * or failed outside the engine (a null pointer, malformed arguments).
 *
 * Codes are a public contract shared with the JavaScript bindings' `error.code`:
 * new ones may be added, an existing one never changes meaning. Read it in the
 * same native call as the message; like the message, it is thread-local.
 * The returned string is static. Do NOT free it.
 */
const char *taladb_last_error_code(void);

/**
 * The [`TALADB_FFI_ABI_VERSION`] this library was built with.
 *
 * The Swift and Kotlin packages live in their own repositories and load a
 * prebuilt library, so the header they compiled against and the library they
 * load can come from different releases. Each checks this once at load time
 * and refuses to continue on a mismatch, because a changed signature links
 * fine and then corrupts the stack at run time.
 */
uint32_t taladb_ffi_abi_version(void);

/**
 * Open (or create) a TalaDB database at `path`.
 *
 * Returns an opaque handle, or NULL on failure.
 * The handle must be freed with `taladb_close`.
 */
struct TalaDbHandle *taladb_open(const char *path);

/**
 * Open (or create) a TalaDB database at `path` with an optional config.
 *
 * `config_json` — JSON-serialised `TalaDbConfig` (durability, plus an optional
 * `passphrase` for encryption at rest), or NULL for defaults. Change webhooks
 * are delivered by the `taladb` TypeScript client, not by this binding.
 *
 * Unknown config keys are ignored so one config file can be shared with the TS
 * client — **except** a key that differs from `passphrase` only in spelling, and
 * a `passphrase` that is present but not a string. Both are errors rather than
 * a silent unencrypted open; see [`passphrase_from_config`].
 *
 * Returns an opaque handle, or NULL on failure.
 * The handle must be freed with `taladb_close`.
 */
struct TalaDbHandle *taladb_open_with_config(const char *path, const char *config_json);

/**
 * Compact the underlying storage file for this database handle.
 * No-op on in-memory databases. Returns 1 on success, -1 on error.
 */
int32_t taladb_compact(struct TalaDbHandle *handle);

/**
 * Close the database and free the handle.
 *
 * The file and its lock are released at once, even while watches or
 * background jobs started from this handle are still alive: those then fail
 * with a "database is closed" error (a watch reports `WatchClosed`) instead
 * of keeping the file open. A job whose transaction is already running
 * finishes it first. If the close itself fails, the handle is still freed and
 * the error is available from `taladb_last_error`.
 */
void taladb_close(struct TalaDbHandle *handle);

/**
 * Free a string returned by any taladb_* function.
 */
void taladb_free_string(char *s);

/**
 * Insert a document (JSON object string).
 * Returns the new document's ULID as a C string, or NULL on error.
 * Caller must free the returned string with `taladb_free_string`.
 */
char *taladb_insert(struct TalaDbHandle *handle, const char *collection, const char *doc_json);

/**
 * Insert multiple documents (JSON array of objects).
 * Returns a JSON array of ULID strings, or NULL on error.
 * Caller must free with `taladb_free_string`.
 *
 * **All or nothing.** If any element of the array is not an object, the whole
 * call fails and nothing is written. It previously skipped unparseable
 * elements and returned a shorter id array, so a caller zipping the returned
 * ids back onto its input silently mis-associated every document after the
 * first bad one — and had no way to learn which had been dropped.
 */
char *taladb_insert_many(struct TalaDbHandle *handle,
                         const char *collection,
                         const char *docs_json);

/**
 * Find all documents matching `filter_json`.
 * Pass `"{}"` or `"null"` to match all.
 * Returns a JSON array string, or NULL on error.
 * Caller must free with `taladb_free_string`.
 */
char *taladb_find(struct TalaDbHandle *handle, const char *collection, const char *filter_json);

/**
 * Find one document matching `filter_json`, or JSON `null` if none.
 * Caller must free with `taladb_free_string`.
 */
char *taladb_find_one(struct TalaDbHandle *handle, const char *collection, const char *filter_json);

/**
 * Update the first matching document.
 * Returns 1 if updated, 0 if not found, -1 on error.
 */
int32_t taladb_update_one(struct TalaDbHandle *handle,
                          const char *collection,
                          const char *filter_json,
                          const char *update_json);

/**
 * Update all matching documents.
 * Returns count updated, or -1 on error.
 */
int32_t taladb_update_many(struct TalaDbHandle *handle,
                           const char *collection,
                           const char *filter_json,
                           const char *update_json);

/**
 * Delete the first matching document.
 * Returns 1 if deleted, 0 if not found, -1 on error.
 */
int32_t taladb_delete_one(struct TalaDbHandle *handle,
                          const char *collection,
                          const char *filter_json);

/**
 * Delete all matching documents.
 * Returns count deleted, or -1 on error.
 */
int32_t taladb_delete_many(struct TalaDbHandle *handle,
                           const char *collection,
                           const char *filter_json);

/**
 * Count documents matching `filter_json`.
 * Returns count, or -1 on error.
 */
int32_t taladb_count(struct TalaDbHandle *handle, const char *collection, const char *filter_json);

/**
 * Run an aggregation pipeline (`pipeline_json` is a JSON array of stages).
 * Returns a JSON array of result documents, or NULL on error.
 * Caller must free with `taladb_free_string`.
 */
char *taladb_aggregate(struct TalaDbHandle *handle,
                       const char *collection,
                       const char *pipeline_json);

/**
 * User collection names (reserved `_`-prefixed excluded), as a JSON array
 * string. Backs the sync orchestration's "sync all collections" default.
 * NULL on error. Caller must free with `taladb_free_string`.
 */
char *taladb_list_collection_names(struct TalaDbHandle *handle);

/**
 * Read the current application migration version (0 if never set), or -1 on
 * error. Backs the `openDB({ migrations })` runner.
 */
int64_t taladb_user_version(struct TalaDbHandle *handle);

/**
 * Persist the application migration version. Returns 0 on success, -1 on error.
 */
int32_t taladb_set_user_version(struct TalaDbHandle *handle, uint32_t version);

/**
 * Force any batched (eventual-durability) writes to disk. Returns 0 on
 * success, -1 on error. No-op under the default immediate durability.
 */
int32_t taladb_flush(struct TalaDbHandle *handle);

/**
 * Create a secondary index on `field`. No-op if it already exists.
 * Returns 1 on success, -1 on error.
 */
int32_t taladb_create_index(struct TalaDbHandle *handle, const char *collection, const char *field);

/**
 * Drop a secondary index on `field`. Returns 1 on success, -1 on error
 * (including when no such index exists).
 */
int32_t taladb_drop_index(struct TalaDbHandle *handle, const char *collection, const char *field);

/**
 * Create a compound index over `fields_json` (a JSON array of field names).
 * Returns 1 on success, -1 on error.
 */
int32_t taladb_create_compound_index(struct TalaDbHandle *handle,
                                     const char *collection,
                                     const char *fields_json);

/**
 * Drop a compound index by its ordered field list (`fields_json`).
 * Returns 1 on success, -1 on error.
 */
int32_t taladb_drop_compound_index(struct TalaDbHandle *handle,
                                   const char *collection,
                                   const char *fields_json);

/**
 * Create a full-text search index on `field`. No-op if it already exists.
 * Returns 1 on success, -1 on error.
 */
int32_t taladb_create_fts_index(struct TalaDbHandle *handle,
                                const char *collection,
                                const char *field);

/**
 * Drop a full-text search index on `field`. Returns 1 on success, -1 on
 * error (including when no such index exists).
 */
int32_t taladb_drop_fts_index(struct TalaDbHandle *handle,
                              const char *collection,
                              const char *field);

/**
 * Rank documents against a free-text query using BM25 (OR semantics).
 *
 * `filter_json` / `options_json` may be NULL. `options_json` accepts
 * `{ k1, b }`.
 *
 * Returns a JSON array string `[{document, score}, ...]`, or NULL on error.
 * Caller must free with `taladb_free_string`.
 */
char *taladb_search_text(struct TalaDbHandle *handle,
                         const char *collection,
                         const char *field,
                         const char *query,
                         uintptr_t top_k,
                         const char *filter_json,
                         const char *options_json);

/**
 * Hybrid retrieval — BM25 and vector similarity fused with reciprocal rank
 * fusion.
 *
 * `vector_ptr` must point to `vector_len` consecutive `f32` values.
 * `options_json` accepts `{ rrfK, textWeight, vectorWeight, candidates, k1, b }`.
 *
 * Returns a JSON array string `[{document, score, textRank, vectorRank}, ...]`,
 * or NULL on error. Caller must free with `taladb_free_string`.
 *
 * # Safety
 * `vector_ptr` must be valid for `vector_len` `f32` reads.
 */
char *taladb_hybrid_search(struct TalaDbHandle *handle,
                           const char *collection,
                           const char *text_field,
                           const char *text,
                           const char *vector_field,
                           const float *vector_ptr,
                           uintptr_t vector_len,
                           uintptr_t top_k,
                           const char *filter_json,
                           const char *options_json);

/**
 * Create a vector index. `metric` and `hnsw_json` may be NULL.
 * Returns 1 on success, -1 on error.
 */
int32_t taladb_create_vector_index(struct TalaDbHandle *handle,
                                   const char *collection,
                                   const char *field,
                                   uintptr_t dimensions,
                                   const char *metric,
                                   const char *hnsw_json);

/**
 * Drop a vector index. Returns 1 on success, -1 on error.
 */
int32_t taladb_drop_vector_index(struct TalaDbHandle *handle,
                                 const char *collection,
                                 const char *field);

/**
 * Promote a flat/legacy vector index or compact its persistent HNSW graph.
 * Returns 1 on success, -1 on error.
 */
int32_t taladb_upgrade_vector_index(struct TalaDbHandle *handle,
                                    const char *collection,
                                    const char *field);

/**
 * Rebuild every configured persistent HNSW graph in the database.
 *
 * This walks the stored HNSW options and rebuilds each one, so a caller does
 * not have to know which collections and fields were configured as HNSW —
 * unlike `taladb_upgrade_vector_index`, which rebuilds a single named field.
 *
 * It reads and re-inserts every indexed vector, so call it for maintenance or
 * legacy migration and off any latency-sensitive path. Normal startup does
 * not need a rebuild. Returns 1 on success, -1 on error.
 */
int32_t taladb_rebuild_hnsw_indexes(struct TalaDbHandle *handle);

/**
 * Synchronous `find_nearest` with a zero-copy Float32 query vector.
 *
 * `query_ptr` — pointer to `query_len` consecutive f32 values (caller-owned).
 * `filter_json` — optional pre-filter JSON (may be NULL / "{}" / "null").
 *
 * Returns a JSON array string `[{document, score}, ...]`, or NULL on error.
 * Caller must free with `taladb_free_string`.
 */
char *taladb_find_nearest(struct TalaDbHandle *handle,
                          const char *collection,
                          const char *field,
                          const float *query_ptr,
                          uintptr_t query_len,
                          uintptr_t top_k,
                          const char *filter_json);

/**
 * Non-blocking poll. Returns 1 if the job has finished, 0 if still running.
 */
int32_t taladb_job_poll(struct TalaDbJob *job);

/**
 * Wait for the job to complete, take its result, and free the job.
 * Returns the result JSON string on success, or NULL on error (see
 * `taladb_last_error`). Always consumes and frees the job.
 * Caller must free the returned string with `taladb_free_string`.
 */
char *taladb_job_take_result(struct TalaDbJob *job);

/**
 * Cancel (detach) the job and free its handle. The worker owns a clone of all
 * database state it needs, so it can safely finish after the caller closes the
 * original database handle.
 */
void taladb_job_cancel(struct TalaDbJob *job);

/**
 * Start a bounded JSON operation on a background worker.
 *
 * # Safety
 * The handle must be live; both strings must be valid NUL-terminated UTF-8
 * for this call. All arguments are copied before returning.
 */
struct TalaDbJob *taladb_call_start(struct TalaDbHandle *handle,
                                    const char *op,
                                    const char *args_json);

/**
 * Run a JSON operation synchronously on the calling thread.
 *
 * Takes the same `op` names and `args_json` array as [`taladb_call_start`]
 * and returns the result JSON directly, or NULL on error (see
 * `taladb_last_error`). Caller must free the result with `taladb_free_string`.
 *
 * For callers that do their own threading — the Swift and Kotlin packages run
 * it on a background executor — this reaches every operation in the dispatch
 * table, including ones with no dedicated export (`listIndexes`,
 * `vectorCommand`), without a job handle to poll.
 */
char *taladb_call(struct TalaDbHandle *handle, const char *op, const char *args_json);

/**
 * Subscribe to the documents in `collection` matching `filter_json` (NULL,
 * `"{}"` or `"null"` for all).
 *
 * Writes made through any handle of the same database wake the watch. It
 * delivers no initial snapshot — read the current state with `taladb_find`
 * *after* this returns, so no write can fall between the two.
 *
 * The watch reads from the database it was created on. After `taladb_close`
 * it holds no storage: `taladb_watch_next` returns -1 with a `WatchClosed`
 * error, and `taladb_watch_close` must still be called to free it. Returns
 * NULL on error.
 */
struct TalaDbWatch *taladb_watch(struct TalaDbHandle *handle,
                                 const char *collection,
                                 const char *filter_json);

/**
 * [`taladb_watch`], with each snapshot projected like `find`'s options:
 * `{"fields": [..]}` keeps only those fields, `{"exclude": [..]}` drops them.
 * A NULL or `"null"` `options_json` is the same as `taladb_watch`.
 *
 * Every write sends a fresh snapshot across to the caller, so excluding a
 * large field the caller does not display (an embedding) keeps each update
 * small.
 *
 * # Safety
 * As for [`taladb_watch`]; `options_json` may be NULL.
 */
struct TalaDbWatch *taladb_watch_with_options(struct TalaDbHandle *handle,
                                              const char *collection,
                                              const char *filter_json,
                                              const char *options_json);

/**
 * Wait up to `timeout_ms` for a write to the watched collection.
 *
 * Returns 1 and sets `*out_json` to a JSON array of the matching documents
 * (free with `taladb_free_string`) if a write occurred — several writes since
 * the last call coalesce into one snapshot of the latest state. Returns 0 and
 * sets `*out_json` to NULL on timeout, and -1 on error.
 *
 * The timeout is what lets a caller stop a subscription: loop on this with a
 * short timeout and check for cancellation between calls. Do not call
 * `taladb_watch_close` while a call on the same watch is in progress.
 */
int32_t taladb_watch_next(struct TalaDbWatch *watch, uint32_t timeout_ms, char **out_json);

/**
 * Close a watch and release the storage it holds open. NULL is a no-op.
 */
void taladb_watch_close(struct TalaDbWatch *watch);

/**
 * Start a `find_nearest` in a background thread. Returns a job handle, or
 * NULL on immediate error (bad args). The Float32 query vector is copied
 * into the thread before the call returns, so `query_ptr` may be freed
 * immediately after this function returns.
 */
struct TalaDbJob *taladb_find_nearest_start(struct TalaDbHandle *handle,
                                            const char *collection,
                                            const char *field,
                                            const float *query_ptr,
                                            uintptr_t query_len,
                                            uintptr_t top_k,
                                            const char *filter_json);

/**
 * Start a `find` in a background thread. Returns a job handle, or NULL on
 * immediate error.
 */
struct TalaDbJob *taladb_find_start(struct TalaDbHandle *handle,
                                    const char *collection,
                                    const char *filter_json);

#ifdef __cplusplus
}  // extern "C"
#endif  // __cplusplus

#endif  /* TALADB_FFI_H */
