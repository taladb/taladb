/*
 * TalaDB C FFI header.
 *
 * GENERATED FILE — do not edit. Regenerate with:
 *   pnpm --filter @taladb/react-native build:cbindgen
 * CI fails if this file and rust/src/lib.rs disagree.
 *
 * This is the stable C interface between the Rust taladb-ffi crate and the C++
 * JSI HostObject (cpp/TalaDBHostObject.cpp, cpp/TalaDBJni.cpp) and the iOS
 * TurboModule (ios/TalaDB.mm).
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


typedef struct TalaDbHandle TalaDbHandle;

/**
 * A background job handle. Opaque to the caller.
 */
typedef struct TalaDbJob TalaDbJob;

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
 * Create a secondary index on `field`. No-op if already exists.
 */
void taladb_create_index(struct TalaDbHandle *handle, const char *collection, const char *field);

/**
 * Drop a secondary index on `field`.
 */
void taladb_drop_index(struct TalaDbHandle *handle, const char *collection, const char *field);

/**
 * Create a compound index over `fields_json` (a JSON array of field names).
 */
void taladb_create_compound_index(struct TalaDbHandle *handle,
                                  const char *collection,
                                  const char *fields_json);

/**
 * Drop a compound index by its ordered field list (`fields_json`).
 */
void taladb_drop_compound_index(struct TalaDbHandle *handle,
                                const char *collection,
                                const char *fields_json);

/**
 * Create a full-text search index on `field`.
 */
void taladb_create_fts_index(struct TalaDbHandle *handle,
                             const char *collection,
                             const char *field);

/**
 * Drop a full-text search index on `field`.
 */
void taladb_drop_fts_index(struct TalaDbHandle *handle, const char *collection, const char *field);

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
 * Rebuild the HNSW graph for a vector index. No-op when HNSW is disabled or
 * the index is flat-only. Returns 1 on success, -1 on error.
 */
int32_t taladb_upgrade_vector_index(struct TalaDbHandle *handle,
                                    const char *collection,
                                    const char *field);

/**
 * Rebuild every HNSW graph in the database, warming the in-memory cache.
 *
 * The graphs are never persisted — `instant-distance` builds an index in one
 * shot and offers no incremental insert, so the cache is empty in every new
 * process and is dropped again whenever a write bumps a vector table's
 * revision. Until a graph is present, `taladb_find_nearest` silently takes the
 * exact path and scans the whole vector table: correct, but linear.
 *
 * This walks the stored HNSW options and rebuilds each one, so a caller does
 * not have to know which collections and fields were configured as HNSW —
 * unlike `taladb_upgrade_vector_index`, which rebuilds a single named field.
 *
 * It reads and re-inserts every indexed vector, so call it once after opening
 * the database and off any latency-sensitive path. Returns 1 on success, -1 on
 * error.
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
