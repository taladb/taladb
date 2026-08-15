/* tslint:disable */
/* eslint-disable */

export class CollectionWasm {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Run a MongoDB-style aggregation pipeline (`$match`, `$group`, `$sort`,
     * `$skip`, `$limit`, `$project`). Returns the resulting documents.
     */
    aggregate(pipeline: any): any;
    /**
     * Count documents matching the filter.
     */
    count(filter: any): number;
    /**
     * Create a compound index. `fields_json` is a JSON array of field names.
     */
    createCompoundIndex(fields_json: string): void;
    /**
     * Create a full-text search index on a string field.
     */
    createFtsIndex(field: string): void;
    /**
     * Create a secondary index on a field.
     */
    createIndex(field: string): void;
    /**
     * Create a vector index on `field`.
     *
     * `dimensions`           - expected vector length.
     * `metric`               - optional: `"cosine"` (default), `"dot"`, or `"euclidean"`.
     * `index_type`           - optional: `"flat"` (default) or `"hnsw"`.
     * `hnsw_m`               - HNSW connectivity (default 16).
     * `hnsw_ef_construction` - build quality (default 200).
     */
    createVectorIndex(field: string, dimensions: number, metric?: string | null, index_type?: string | null, hnsw_m?: number | null, hnsw_ef_construction?: number | null): void;
    /**
     * Delete all matching documents. Returns the count deleted.
     */
    deleteMany(filter: any): number;
    /**
     * Delete the first matching document. Returns true if deleted.
     */
    deleteOne(filter: any): boolean;
    /**
     * Drop a compound index by its ordered field list (`fields_json`).
     */
    dropCompoundIndex(fields_json: string): void;
    /**
     * Drop a full-text search index and everything it stores.
     */
    dropFtsIndex(field: string): void;
    /**
     * Drop a secondary index.
     */
    dropIndex(field: string): void;
    /**
     * Drop a vector index (and its HNSW graph if present).
     */
    dropVectorIndex(field: string): void;
    /**
     * Find documents matching the filter. Returns a JS array of plain objects.
     */
    find(filter: any): any;
    /**
     * Find the `top_k` nearest documents to `query` on a vector index.
     *
     * `filter` - optional pre-filter (same format as `find`). Pass `null` to
     *            search across all documents that have the vector field.
     *
     * Returns a JSON array of `{ document: {...}, score: number }` objects.
     */
    findNearest(field: string, query: Float32Array, top_k: number, filter: any): any;
    /**
     * Find a single document. Returns the document or null.
     */
    findOne(filter: any): any;
    /**
     * Hybrid retrieval — BM25 and vector similarity fused with reciprocal
     * rank fusion.
     *
     * `options` accepts `{ rrfK, textWeight, vectorWeight, candidates, k1, b }`.
     * Returns a JSON array of `{ document, score, textRank, vectorRank }`.
     */
    hybridSearch(text_field: string, text: string, vector_field: string, vector: Float32Array, top_k: number, filter: any, options: any): any;
    /**
     * Insert a document. Accepts a plain JS object, returns the ULID string id.
     */
    insert(doc: any): string;
    /**
     * Insert multiple documents. Returns an array of ULID string ids.
     */
    insertMany(docs: any): any;
    /**
     * Rank documents against a free-text query using BM25 (OR semantics).
     *
     * Returns a JSON array of `{ document, score }`.
     */
    searchText(field: string, query: string, top_k: number, filter: any, options: any): any;
    /**
     * Update all matching documents. Returns the count updated.
     */
    updateMany(filter: any, update: any): number;
    /**
     * Update the first matching document. Returns true if a document was updated.
     */
    updateOne(filter: any, update: any): boolean;
    /**
     * Rebuild the HNSW graph from the current flat vector table.
     */
    upgradeVectorIndex(field: string): void;
}

export class TalaDBWasm {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Get a collection handle by name.
     */
    collection(name: string): CollectionWasm;
    /**
     * Serialize the entire in-memory database to bytes.
     *
     * Pass the returned `Uint8Array` to `opfs_flush_snapshot` to persist, or
     * store it yourself.  On the next page load, pass the same bytes to
     * `openWithSnapshot` to restore all data.
     */
    exportSnapshot(): Uint8Array;
    /**
     * User collection names (reserved `_`-prefixed collections excluded).
     */
    listCollectionNames(): string[];
    /**
     * Open an in-memory database (suitable for tests and environments without OPFS).
     */
    static openInMemory(): TalaDBWasm;
    /**
     * Open a database, restoring from a previously exported snapshot if provided.
     *
     * Pass the bytes returned by `opfs_load_snapshot` (or `null`/`undefined` for
     * a fresh empty database).  After each write, call `exportSnapshot()` and
     * pass the bytes to `opfs_flush_snapshot` to persist across page reloads.
     *
     * ```js
     * const bytes = await opfs_load_snapshot('myapp.db');   // null on first open
     * const db = TalaDBWasm.openWithSnapshot(bytes);
     * // ... mutations ...
     * await opfs_flush_snapshot('myapp.db', db.exportSnapshot());
     * ```
     */
    static openWithSnapshot(snapshot?: Uint8Array | null): TalaDBWasm;
    /**
     * Persist the application migration version. Called after each migration's
     * body succeeds so a crash mid-run resumes from the last applied version.
     */
    setUserVersion(version: number): void;
    /**
     * Read the current application migration version (0 if never set). Backs
     * the `openDB({ migrations })` runner, which advances it per migration.
     */
    userVersion(): number;
}

export class WorkerDB {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Run an aggregation pipeline. Returns a JSON array of result documents.
     */
    aggregate(collection: string, pipeline_json: string): string;
    /**
     * Compact the underlying OPFS / redb storage file, reclaiming space freed
     * by deletes and updates.
     *
     * Call this during idle periods (e.g. once on app startup after tombstone
     * compaction). No-op on in-memory (IDB-fallback) databases.
     *
     * ```js
     * db.compact();
     * ```
     */
    compact(): void;
    /**
     * Count matching documents.
     */
    count(collection: string, filter_json: string): number;
    /**
     * Create a compound index. `fields_json` is a JSON array of field names.
     */
    createCompoundIndex(collection: string, fields_json: string): void;
    createFtsIndex(collection: string, field: string): void;
    createIndex(collection: string, field: string): void;
    /**
     * Create a vector index.
     *
     * - `metric_str`: `"cosine"` (default) | `"dot"` | `"euclidean"`
     * - `index_type`: `"flat"` (default) | `"hnsw"`
     * - `hnsw_m`: HNSW connectivity (default 16, only used when `index_type = "hnsw"`)
     * - `hnsw_ef_construction`: build-time quality (default 200, only used when `index_type = "hnsw"`)
     */
    createVectorIndex(collection: string, field: string, dimensions: number, metric_str?: string | null, index_type?: string | null, hnsw_m?: number | null, hnsw_ef_construction?: number | null): void;
    /**
     * Delete all matching documents. Returns the count deleted.
     */
    deleteMany(collection: string, filter_json: string): number;
    /**
     * Delete documents by id. Returns how many were present and removed.
     * The delete half of cross-tab write propagation.
     */
    deleteManyWithIds(collection: string, ids_json: string): number;
    /**
     * Delete the first matching document. Returns `true` / `false`.
     */
    deleteOne(collection: string, filter_json: string): boolean;
    /**
     * Drop a compound index by its ordered field list (`fields_json`).
     */
    dropCompoundIndex(collection: string, fields_json: string): void;
    dropFtsIndex(collection: string, field: string): void;
    dropIndex(collection: string, field: string): void;
    /**
     * Drop a vector index (and its HNSW graph if present).
     */
    dropVectorIndex(collection: string, field: string): void;
    /**
     * Serialize the entire in-memory database to bytes for persistence.
     *
     * Pass the returned bytes to `idbSaveSnapshot` to persist across page reloads.
     * On next open, pass the same bytes to `openWithSnapshot` to restore all data.
     */
    exportSnapshot(): Uint8Array;
    /**
     * Find documents. Returns a JSON array of document objects.
     */
    find(collection: string, filter_json: string): string;
    /**
     * Find nearest neighbours. Returns a JSON string of `[{ document, score }]`.
     */
    findNearest(collection: string, field: string, query_json: string, top_k: number, filter_json: string): string;
    /**
     * Find one document. Returns a JSON object or `"null"`.
     */
    findOne(collection: string, filter_json: string): string;
    /**
     * Force batched (eventual) OPFS writes to durable storage. No-op under the
     * default immediate durability. Backs `db.flush()`.
     */
    flush(): void;
    /**
     * Hybrid retrieval — BM25 and vector similarity fused with reciprocal
     * rank fusion.
     *
     * `options_json` accepts `{ rrfK, textWeight, vectorWeight, candidates, k1, b }`.
     * Returns a JSON array of `{ document, score, textRank, vectorRank }`.
     */
    hybridSearch(collection: string, text_field: string, text: string, vector_field: string, vector_json: string, top_k: number, filter_json: string, options_json: string): string;
    /**
     * Insert a document. Returns the new ULID as a string.
     */
    insert(collection: string, doc_json: string): string;
    /**
     * Insert many documents. Returns a JSON array of ULID strings.
     */
    insertMany(collection: string, docs_json: string): string;
    /**
     * Returns a JSON array of all collection names in the database.
     */
    listCollections(): string;
    /**
     * Returns a JSON string `{ btree: string[], fts: string[], vector: string[] }`
     * listing all indexes on the given collection.
     */
    listIndexes(collection: string): string;
    /**
     * Open an in-memory database (for tests and OPFS-unavailable fallback).
     */
    static openInMemory(): WorkerDB;
    /**
     * Open a database backed by OPFS with HTTP push sync config.
     *
     * `config_json` - JSON-serialised `TalaDbConfig`, or `null` to open without sync.
     *
     * ```js
     * const handle = await file_handle.createSyncAccessHandle();
     * const db = WorkerDB.openWithConfigAndOpfs(handle, JSON.stringify(config));
     * ```
     */
    static openWithConfigAndOpfs(sync_handle: FileSystemSyncAccessHandle, config_json?: string | null, passphrase?: string | null, salt?: Uint8Array | null): WorkerDB;
    /**
     * Open a database from an optional snapshot with HTTP push sync config.
     *
     * `config_json` - JSON-serialised `TalaDbConfig`, or `null` to open without sync.
     *
     * ```js
     * const db = WorkerDB.openWithConfigAndSnapshot(snapshot, JSON.stringify(config));
     * ```
     */
    static openWithConfigAndSnapshot(data?: Uint8Array | null, config_json?: string | null): WorkerDB;
    /**
     * Open a database backed by an OPFS `FileSystemSyncAccessHandle`.
     *
     * Call sequence in the SharedWorker:
     * ```js
     * const handle = await file_handle.createSyncAccessHandle();
     * const workerDb = WorkerDB.openWithOpfs(handle);
     * ```
     */
    static openWithOpfs(sync_handle: FileSystemSyncAccessHandle): WorkerDB;
    /**
     * Open a database, restoring from a previously exported snapshot if provided.
     *
     * Pass the bytes returned by `WorkerDB.exportSnapshot()` (or `null`/`undefined`
     * for a fresh empty database). Used by the IndexedDB fallback path.
     *
     * ```js
     * const bytes = await idbLoadSnapshot(dbName);   // null on first open
     * const workerDb = WorkerDB.openWithSnapshot(bytes);
     * ```
     */
    static openWithSnapshot(data?: Uint8Array | null): WorkerDB;
    /**
     * Rank documents against a free-text query using BM25 (OR semantics).
     *
     * Returns a JSON array of `{ document, score }`.
     */
    searchText(collection: string, field: string, query: string, top_k: number, filter_json: string, options_json: string): string;
    /**
     * Set write durability: `eventual = true` batches OPFS fsyncs for
     * throughput (call `flush()` to force), `false` (default) fsyncs each
     * commit. Derived from `durability.flush_every_write` by the worker.
     */
    setDurability(eventual: boolean): void;
    /**
     * Persist the application migration version. Called after each migration's
     * body succeeds so a crash mid-run resumes from the last applied version.
     */
    setUserVersion(version: number): void;
    /**
     * Update all matching documents. Returns the count updated.
     */
    updateMany(collection: string, filter_json: string, update_json: string): number;
    /**
     * Update the first matching document. Returns `true` / `false`.
     */
    updateOne(collection: string, filter_json: string, update_json: string): boolean;
    /**
     * Rebuild the HNSW graph for a vector index from the current flat vector
     * table.  Use after bulk inserts or when ANN recall has degraded.
     *
     * No-op when the `vector-hnsw` feature is disabled or the index is flat-only.
     */
    upgradeVectorIndex(collection: string, field: string): void;
    /**
     * Upsert documents **by their own `_id`**, in one commit per document.
     *
     * Backs cross-tab write propagation: a tab that cannot hold the OPFS lock
     * writes to an in-memory database, then forwards the committed documents
     * here so the tab holding the lock applies them to the durable file. The
     * `_id`s travel with the documents, so an id an application already holds
     * stays valid after the hand-off — which a plain re-`insert` would break by
     * minting a new ULID.
     *
     * Unlike `insert_many`, a caller-supplied `_id` is required, not ignored.
     */
    upsertManyWithIds(collection: string, docs_json: string): string;
    /**
     * Read the current application migration version (0 if never set). Backs
     * the `openDB({ migrations })` runner, which advances it per migration.
     */
    userVersion(): number;
    /**
     * This collection's write generation — a counter bumped once per committed
     * mutation.
     *
     * Backs `subscribe()`: a live query can compare one integer per tick
     * instead of re-running the query and `JSON.stringify`-ing the whole
     * result set to see whether anything moved.
     */
    writeGeneration(collection: string): number;
}

/**
 * Load a previous database snapshot from IndexedDB.
 * Returns `None` if no snapshot exists yet (first open) or IDB is unavailable.
 */
export function idb_load_snapshot(db_name: string): Promise<Uint8Array | undefined>;

/**
 * Persist a database snapshot to IndexedDB.
 * Returns `true` on success, `false` on any failure.
 */
export function idb_save_snapshot(db_name: string, data: Uint8Array): Promise<boolean>;

/**
 * Initialize panic hook for better error messages in the browser console.
 */
export function init(): void;

/**
 * Returns true if OPFS is available in the current browser context.
 * Always returns false in Workers without storage access.
 */
export function is_opfs_available(): Promise<boolean>;

/**
 * Delete the OPFS snapshot file for `db_name`.
 * No-op if the file does not exist.
 */
export function opfs_delete_snapshot(db_name: string): Promise<boolean>;

/**
 * Persist a database snapshot to OPFS.
 * Creates the file on first call. Subsequent calls overwrite atomically.
 */
export function opfs_flush_snapshot(db_name: string, data: Uint8Array): Promise<boolean>;

/**
 * Load the last persisted database snapshot from OPFS.
 * Returns `None` if the file does not exist yet (first open).
 */
export function opfs_load_snapshot(db_name: string): Promise<Uint8Array | undefined>;

/**
 * Open (or create) an OPFS file and return an `OpfsBackend` for redb.
 *
 * This function is **async** because `getFileHandle` and `createSyncAccessHandle`
 * are both async in the OPFS API. Call it once at worker startup.
 */
export function opfs_open_backend(db_name: string): Promise<any>;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_collectionwasm_free: (a: number, b: number) => void;
    readonly __wbg_taladbwasm_free: (a: number, b: number) => void;
    readonly __wbg_workerdb_free: (a: number, b: number) => void;
    readonly collectionwasm_aggregate: (a: number, b: any) => [number, number, number];
    readonly collectionwasm_count: (a: number, b: any) => [number, number, number];
    readonly collectionwasm_createCompoundIndex: (a: number, b: number, c: number) => [number, number];
    readonly collectionwasm_createFtsIndex: (a: number, b: number, c: number) => [number, number];
    readonly collectionwasm_createIndex: (a: number, b: number, c: number) => [number, number];
    readonly collectionwasm_createVectorIndex: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => [number, number];
    readonly collectionwasm_deleteMany: (a: number, b: any) => [number, number, number];
    readonly collectionwasm_deleteOne: (a: number, b: any) => [number, number, number];
    readonly collectionwasm_dropCompoundIndex: (a: number, b: number, c: number) => [number, number];
    readonly collectionwasm_dropFtsIndex: (a: number, b: number, c: number) => [number, number];
    readonly collectionwasm_dropIndex: (a: number, b: number, c: number) => [number, number];
    readonly collectionwasm_dropVectorIndex: (a: number, b: number, c: number) => [number, number];
    readonly collectionwasm_find: (a: number, b: any) => [number, number, number];
    readonly collectionwasm_findNearest: (a: number, b: number, c: number, d: number, e: number, f: number, g: any) => [number, number, number];
    readonly collectionwasm_findOne: (a: number, b: any) => [number, number, number];
    readonly collectionwasm_hybridSearch: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: any, l: any) => [number, number, number];
    readonly collectionwasm_insert: (a: number, b: any) => [number, number, number, number];
    readonly collectionwasm_insertMany: (a: number, b: any) => [number, number, number];
    readonly collectionwasm_searchText: (a: number, b: number, c: number, d: number, e: number, f: number, g: any, h: any) => [number, number, number];
    readonly collectionwasm_updateMany: (a: number, b: any, c: any) => [number, number, number];
    readonly collectionwasm_updateOne: (a: number, b: any, c: any) => [number, number, number];
    readonly collectionwasm_upgradeVectorIndex: (a: number, b: number, c: number) => [number, number];
    readonly idb_load_snapshot: (a: number, b: number) => any;
    readonly idb_save_snapshot: (a: number, b: number, c: number, d: number) => any;
    readonly is_opfs_available: () => any;
    readonly opfs_delete_snapshot: (a: number, b: number) => any;
    readonly opfs_flush_snapshot: (a: number, b: number, c: number, d: number) => any;
    readonly opfs_load_snapshot: (a: number, b: number) => any;
    readonly opfs_open_backend: (a: number, b: number) => any;
    readonly taladbwasm_collection: (a: number, b: number, c: number) => [number, number, number];
    readonly taladbwasm_exportSnapshot: (a: number) => [number, number, number, number];
    readonly taladbwasm_listCollectionNames: (a: number) => [number, number, number, number];
    readonly taladbwasm_openInMemory: () => [number, number, number];
    readonly taladbwasm_openWithSnapshot: (a: number, b: number) => [number, number, number];
    readonly taladbwasm_setUserVersion: (a: number, b: number) => [number, number];
    readonly taladbwasm_userVersion: (a: number) => [number, number, number];
    readonly workerdb_aggregate: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly workerdb_compact: (a: number) => [number, number];
    readonly workerdb_count: (a: number, b: number, c: number, d: number, e: number) => [number, number, number];
    readonly workerdb_createCompoundIndex: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly workerdb_createFtsIndex: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly workerdb_createIndex: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly workerdb_createVectorIndex: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number) => [number, number];
    readonly workerdb_deleteMany: (a: number, b: number, c: number, d: number, e: number) => [number, number, number];
    readonly workerdb_deleteManyWithIds: (a: number, b: number, c: number, d: number, e: number) => [number, number, number];
    readonly workerdb_deleteOne: (a: number, b: number, c: number, d: number, e: number) => [number, number, number];
    readonly workerdb_dropCompoundIndex: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly workerdb_dropFtsIndex: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly workerdb_dropIndex: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly workerdb_dropVectorIndex: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly workerdb_exportSnapshot: (a: number) => [number, number, number, number];
    readonly workerdb_find: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly workerdb_findNearest: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => [number, number, number, number];
    readonly workerdb_findOne: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly workerdb_flush: (a: number) => [number, number];
    readonly workerdb_hybridSearch: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number, m: number, n: number, o: number, p: number) => [number, number, number, number];
    readonly workerdb_insert: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly workerdb_insertMany: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly workerdb_listCollections: (a: number) => [number, number, number, number];
    readonly workerdb_listIndexes: (a: number, b: number, c: number) => [number, number, number, number];
    readonly workerdb_openInMemory: () => [number, number, number];
    readonly workerdb_openWithConfigAndOpfs: (a: any, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number];
    readonly workerdb_openWithConfigAndSnapshot: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly workerdb_openWithOpfs: (a: any) => [number, number, number];
    readonly workerdb_openWithSnapshot: (a: number, b: number) => [number, number, number];
    readonly workerdb_searchText: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: number, l: number) => [number, number, number, number];
    readonly workerdb_setDurability: (a: number, b: number) => void;
    readonly workerdb_setUserVersion: (a: number, b: number) => [number, number];
    readonly workerdb_updateMany: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number];
    readonly workerdb_updateOne: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number];
    readonly workerdb_upgradeVectorIndex: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly workerdb_upsertManyWithIds: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly workerdb_userVersion: (a: number) => [number, number, number];
    readonly workerdb_writeGeneration: (a: number, b: number, c: number) => [number, number, number];
    readonly init: () => void;
    readonly wasm_bindgen__closure__destroy__h4ecd198e6e5fb530: (a: number, b: number) => void;
    readonly wasm_bindgen__closure__destroy__h014c297fadd2a065: (a: number, b: number) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h20bda61557acb630: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen__convert__closures_____invoke__h0dbbf48826ad16e8: (a: number, b: number, c: any, d: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__hbf94730c3811ffd3: (a: number, b: number, c: any) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __externref_drop_slice: (a: number, b: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
