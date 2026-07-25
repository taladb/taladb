#pragma once
#ifndef TALADB_FFI_H
#define TALADB_FFI_H

/*
 * TalaDB C FFI header.
 *
 * This file is the stable C interface between the Rust taladb-ffi crate and
 * the C++ JSI HostObject.  It is kept in sync with rust/src/lib.rs manually
 * (or regenerated with cbindgen — see rust/cbindgen.toml).
 *
 * Ownership rules
 * ---------------
 *  - Strings IN  : caller-owned, UTF-8, null-terminated.
 *  - Strings OUT : heap-allocated by Rust; caller must free with
 *                  taladb_free_string().
 *  - Handles     : allocated by taladb_open(); freed by taladb_close().
 *  - Errors      : string functions return NULL; integer functions return -1.
 */

#include <stdint.h>
#include <stdlib.h>

#ifdef __cplusplus
extern "C" {
#endif

/* -------------------------------------------------------------------------
 * Opaque database handle
 * ---------------------------------------------------------------------- */
typedef struct TalaDbHandle TalaDbHandle;

/* -------------------------------------------------------------------------
 * Lifecycle
 * ---------------------------------------------------------------------- */

/** Open (or create) a database at the given file-system path. */
TalaDbHandle *taladb_open(const char *path);

/**
 * Open (or create) a database with HTTP push sync configuration.
 *
 * config_json — JSON-serialised TalaDbConfig, or NULL to open without sync.
 * Returns an opaque handle (same semantics as taladb_open), or NULL on failure.
 */
TalaDbHandle *taladb_open_with_config(const char *path, const char *config_json);

/** Flush and close the database, freeing the handle. */
void taladb_close(TalaDbHandle *handle);

/**
 * Compact the underlying storage file, reclaiming space freed by deletes and
 * updates. No-op on in-memory databases. Returns 1 on success, -1 on error.
 */
int32_t taladb_compact(TalaDbHandle *handle);

/** Free a C string returned by any taladb_* function. */
void taladb_free_string(char *s);

/* -------------------------------------------------------------------------
 * Insert
 * ---------------------------------------------------------------------- */

/**
 * Insert a document (JSON object).
 * Returns the new document's ULID as a C string, or NULL on error.
 * Caller must free with taladb_free_string().
 */
char *taladb_insert(TalaDbHandle *handle,
                    const char   *collection,
                    const char   *doc_json);

/**
 * Insert multiple documents (JSON array of objects).
 * Returns a JSON array of ULID strings, or NULL on error.
 * Caller must free with taladb_free_string().
 */
char *taladb_insert_many(TalaDbHandle *handle,
                         const char   *collection,
                         const char   *docs_json);

/* -------------------------------------------------------------------------
 * Replication writes — id-addressed upsert / delete
 * ---------------------------------------------------------------------- */

/**
 * Upsert many documents by caller-supplied `_id`, in one commit.
 *
 * Unlike taladb_insert_many(), which discards `_id` and mints a fresh ULID, this
 * honours the id on each document — which is what makes a replication upsert
 * idempotent across repeated fetches of the same remote row.
 *
 * `origin` is "remote" (authoritative rows replicated in from an origin) or
 * "local" (ordinary user writes). Remote rows are marked so they never replicate
 * back out.
 *
 * Returns a JSON array of ULID strings, or NULL on error.
 * Caller must free with taladb_free_string().
 */
char *taladb_replace_many_with_ids(TalaDbHandle *handle,
                                   const char   *collection,
                                   const char   *docs_json,
                                   const char   *origin);

/**
 * Delete many documents by id, in one commit.
 * Returns a JSON number (the count removed), or NULL on error.
 * Caller must free with taladb_free_string().
 */
char *taladb_delete_many_with_ids(TalaDbHandle *handle,
                                  const char   *collection,
                                  const char   *ids_json,
                                  const char   *origin);

/* -------------------------------------------------------------------------
 * Find
 * ---------------------------------------------------------------------- */

/**
 * Find all documents matching filter_json.
 * Pass "{}" or "null" to match all.
 * Returns a JSON array string, or NULL on error.
 * Caller must free with taladb_free_string().
 */
char *taladb_find(TalaDbHandle *handle,
                  const char   *collection,
                  const char   *filter_json);

/**
 * Find the first document matching filter_json.
 * Returns a JSON object string, or the string "null" if not found.
 * Caller must free with taladb_free_string().
 */
char *taladb_find_one(TalaDbHandle *handle,
                      const char   *collection,
                      const char   *filter_json);

/* -------------------------------------------------------------------------
 * Update
 * ---------------------------------------------------------------------- */

/** Update the first matching document. Returns 1 updated, 0 not found, -1 error. */
int32_t taladb_update_one(TalaDbHandle *handle,
                          const char   *collection,
                          const char   *filter_json,
                          const char   *update_json);

/** Update all matching documents. Returns count updated, or -1 on error. */
int32_t taladb_update_many(TalaDbHandle *handle,
                           const char   *collection,
                           const char   *filter_json,
                           const char   *update_json);

/* -------------------------------------------------------------------------
 * Delete
 * ---------------------------------------------------------------------- */

/** Delete the first matching document. Returns 1 deleted, 0 not found, -1 error. */
int32_t taladb_delete_one(TalaDbHandle *handle,
                          const char   *collection,
                          const char   *filter_json);

/** Delete all matching documents. Returns count deleted, or -1 on error. */
int32_t taladb_delete_many(TalaDbHandle *handle,
                           const char   *collection,
                           const char   *filter_json);

/* -------------------------------------------------------------------------
 * Count
 * ---------------------------------------------------------------------- */

/** Count documents matching filter_json. Returns count, or -1 on error. */
int32_t taladb_count(TalaDbHandle *handle,
                     const char   *collection,
                     const char   *filter_json);

/* -------------------------------------------------------------------------
 * Aggregate
 * ---------------------------------------------------------------------- */

/**
 * Run an aggregation pipeline. pipeline_json is a JSON array of stages.
 * Returns a JSON array of result documents, or NULL on error.
 * Caller must free with taladb_free_string().
 */
char *taladb_aggregate(TalaDbHandle *handle,
                       const char   *collection,
                       const char   *pipeline_json);

/* -------------------------------------------------------------------------
 * Bidirectional sync — changeset export / import (backs JS db.sync())
 * ---------------------------------------------------------------------- */

/**
 * Export a changeset for the collections in collections_json (a JSON array of
 * names) with changed_at > since_ms. Returns a JSON string, or NULL on error.
 * Caller must free with taladb_free_string().
 */
char *taladb_export_changes(TalaDbHandle *handle,
                            const char   *collections_json,
                            double        since_ms);

/**
 * Merge a JSON changeset (from a remote peer) via Last-Write-Wins.
 * Returns the number of documents changed, or -1 on error.
 */
int32_t taladb_import_changes(TalaDbHandle *handle,
                              const char   *changeset_json);

/**
 * User collection names (reserved names excluded) as a JSON array string.
 * Returns NULL on error. Caller must free with taladb_free_string().
 */
char *taladb_list_collection_names(TalaDbHandle *handle);

/**
 * Merge a JSON changeset through a tolerant structural validator built from
 * schemas_json ({ "<collection>": { version, required, types, defaults,
 * renames } }). Returns a JSON "{ applied, skipped, quarantined }" string, or
 * NULL on error. Caller must free with taladb_free_string().
 */
char *taladb_import_changes_validated(TalaDbHandle *handle,
                                      const char   *changeset_json,
                                      const char   *schemas_json);

/**
 * Documents set aside in a collection's quarantine table by a validated import,
 * as a JSON array of { document, reason, changedAt }. NULL on error. Caller
 * must free with taladb_free_string().
 */
char *taladb_quarantined(TalaDbHandle *handle, const char *collection);

/**
 * Read / write the application migration version (backs openDB({ migrations })).
 * taladb_user_version returns the version (0 if unset) or -1 on error;
 * taladb_set_user_version returns 0 on success, -1 on error.
 */
int64_t taladb_user_version(TalaDbHandle *handle);
int32_t taladb_set_user_version(TalaDbHandle *handle, uint32_t version);

/**
 * Force any batched (eventual-durability) writes to disk. Returns 0 on success,
 * -1 on error. No-op under the default immediate durability. Backs db.flush().
 */
int32_t taladb_flush(TalaDbHandle *handle);

/* -------------------------------------------------------------------------
 * Index management
 * ---------------------------------------------------------------------- */

void taladb_create_index    (TalaDbHandle *handle, const char *collection, const char *field);
void taladb_drop_index      (TalaDbHandle *handle, const char *collection, const char *field);
void taladb_create_fts_index(TalaDbHandle *handle, const char *collection, const char *field);
void taladb_drop_fts_index  (TalaDbHandle *handle, const char *collection, const char *field);

/* Compound (multi-field) indexes. fields_json is a JSON array of field names. */
void taladb_create_compound_index(TalaDbHandle *handle, const char *collection, const char *fields_json);
void taladb_drop_compound_index  (TalaDbHandle *handle, const char *collection, const char *fields_json);

/* -------------------------------------------------------------------------
 * Vector index management
 *
 *   metric    — "cosine" (default), "dot", or "euclidean". NULL ⇒ cosine.
 *   hnsw_json — JSON-encoded HnswOptions, or NULL for flat index.
 * ---------------------------------------------------------------------- */

int32_t taladb_create_vector_index(TalaDbHandle *handle,
                                   const char   *collection,
                                   const char   *field,
                                   size_t        dimensions,
                                   const char   *metric,
                                   const char   *hnsw_json);

int32_t taladb_drop_vector_index   (TalaDbHandle *handle, const char *collection, const char *field);
int32_t taladb_upgrade_vector_index(TalaDbHandle *handle, const char *collection, const char *field);

/* -------------------------------------------------------------------------
 * findNearest — Float32 zero-copy fast path
 *
 * query_ptr / query_len address `query_len` consecutive f32 values. Caller
 * retains ownership; the buffer may be freed immediately after the call.
 * filter_json may be NULL / "{}" / "null" to search without a pre-filter.
 *
 * Returns a JSON array string `[{document, score}, ...]`, or NULL on error.
 * Caller must free with taladb_free_string.
 * ---------------------------------------------------------------------- */

char *taladb_find_nearest(TalaDbHandle *handle,
                          const char   *collection,
                          const char   *field,
                          const float  *query_ptr,
                          size_t        query_len,
                          size_t        top_k,
                          const char   *filter_json);

/* BM25 full-text ranking. Returns a JSON array `[{document, score}, ...]`,
 * or NULL on error. filter_json / options_json may be NULL. options_json
 * accepts `{ k1, b }`. Caller must free with taladb_free_string. */
char *taladb_search_text(TalaDbHandle *handle,
                         const char   *collection,
                         const char   *field,
                         const char   *query,
                         size_t        top_k,
                         const char   *filter_json,
                         const char   *options_json);

/* Hybrid retrieval (BM25 + vector, fused with RRF). Returns a JSON array
 * `[{document, score, textRank, vectorRank}, ...]`, or NULL on error.
 * filter_json / options_json may be NULL. options_json accepts
 * `{ rrfK, textWeight, vectorWeight, candidates, k1, b }`.
 * Caller must free with taladb_free_string. */
char *taladb_hybrid_search(TalaDbHandle *handle,
                           const char   *collection,
                           const char   *text_field,
                           const char   *text,
                           const char   *vector_field,
                           const float  *vector_ptr,
                           size_t        vector_len,
                           size_t        top_k,
                           const char   *filter_json,
                           const char   *options_json);

char *taladb_sync_status(TalaDbHandle *handle);
int32_t taladb_sync_flush(TalaDbHandle *handle, uint64_t timeout_ms);

/* -------------------------------------------------------------------------
 * Async job API — run heavy queries on a background thread.
 *
 * Flow
 * ----
 *   TalaDbJob *j = taladb_find_nearest_start(...);  // spawns worker thread
 *   while (taladb_job_poll(j) == 0) { } // yield to JS event loop
 *   char *json = taladb_job_take_result(j);         // frees the job
 *
 * Workers clone the Arc-backed database state and do not borrow the handle.
 * ---------------------------------------------------------------------- */

typedef struct TalaDbJob TalaDbJob;

/** Kick a background `find_nearest`. Returns NULL on immediate arg error. */
TalaDbJob *taladb_find_nearest_start(TalaDbHandle *handle,
                                     const char   *collection,
                                     const char   *field,
                                     const float  *query_ptr,
                                     size_t        query_len,
                                     size_t        top_k,
                                     const char   *filter_json);

/** Kick a background `find`. Returns NULL on immediate arg error. */
TalaDbJob *taladb_find_start(TalaDbHandle *handle,
                             const char   *collection,
                             const char   *filter_json);

/** Non-blocking. Returns 1 if complete, 0 if still running, -1 on NULL job. */
int32_t taladb_job_poll(TalaDbJob *job);

/**
 * Join the worker, take its result, and free the job.
 * Returns a JSON string on success, or NULL on error (see taladb_last_error).
 * Caller must free with taladb_free_string. Always frees the job.
 */
char *taladb_job_take_result(TalaDbJob *job);

/** Detach the job and free the handle without waiting. */
void taladb_job_cancel(TalaDbJob *job);

/**
 * Last error message for the current thread, or NULL.
 * The returned pointer is valid until the next taladb_* call on this thread.
 * Do NOT free.
 */
const char *taladb_last_error(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* TALADB_FFI_H */
