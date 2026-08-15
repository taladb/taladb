/* @ts-self-types="./taladb_web.d.ts" */

export class CollectionWasm {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(CollectionWasm.prototype);
        obj.__wbg_ptr = ptr;
        CollectionWasmFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        CollectionWasmFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_collectionwasm_free(ptr, 0);
    }
    /**
     * Run a MongoDB-style aggregation pipeline (`$match`, `$group`, `$sort`,
     * `$skip`, `$limit`, `$project`). Returns the resulting documents.
     * @param {any} pipeline
     * @returns {any}
     */
    aggregate(pipeline) {
        const ret = wasm.collectionwasm_aggregate(this.__wbg_ptr, pipeline);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Count documents matching the filter.
     * @param {any} filter
     * @returns {number}
     */
    count(filter) {
        const ret = wasm.collectionwasm_count(this.__wbg_ptr, filter);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * Create a compound index. `fields_json` is a JSON array of field names.
     * @param {string} fields_json
     */
    createCompoundIndex(fields_json) {
        const ptr0 = passStringToWasm0(fields_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_createCompoundIndex(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Create a full-text search index on a string field.
     * @param {string} field
     */
    createFtsIndex(field) {
        const ptr0 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_createFtsIndex(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Create a secondary index on a field.
     * @param {string} field
     */
    createIndex(field) {
        const ptr0 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_createIndex(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Create a vector index on `field`.
     *
     * `dimensions`           - expected vector length.
     * `metric`               - optional: `"cosine"` (default), `"dot"`, or `"euclidean"`.
     * `index_type`           - optional: `"flat"` (default) or `"hnsw"`.
     * `hnsw_m`               - HNSW connectivity (default 16).
     * `hnsw_ef_construction` - build quality (default 200).
     * @param {string} field
     * @param {number} dimensions
     * @param {string | null} [metric]
     * @param {string | null} [index_type]
     * @param {number | null} [hnsw_m]
     * @param {number | null} [hnsw_ef_construction]
     */
    createVectorIndex(field, dimensions, metric, index_type, hnsw_m, hnsw_ef_construction) {
        const ptr0 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(metric) ? 0 : passStringToWasm0(metric, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(index_type) ? 0 : passStringToWasm0(index_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        var len2 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_createVectorIndex(this.__wbg_ptr, ptr0, len0, dimensions, ptr1, len1, ptr2, len2, isLikeNone(hnsw_m) ? 0x100000001 : (hnsw_m) >>> 0, isLikeNone(hnsw_ef_construction) ? 0x100000001 : (hnsw_ef_construction) >>> 0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Delete all matching documents. Returns the count deleted.
     * @param {any} filter
     * @returns {number}
     */
    deleteMany(filter) {
        const ret = wasm.collectionwasm_deleteMany(this.__wbg_ptr, filter);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * Delete the first matching document. Returns true if deleted.
     * @param {any} filter
     * @returns {boolean}
     */
    deleteOne(filter) {
        const ret = wasm.collectionwasm_deleteOne(this.__wbg_ptr, filter);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] !== 0;
    }
    /**
     * Drop a compound index by its ordered field list (`fields_json`).
     * @param {string} fields_json
     */
    dropCompoundIndex(fields_json) {
        const ptr0 = passStringToWasm0(fields_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_dropCompoundIndex(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Drop a full-text search index and everything it stores.
     * @param {string} field
     */
    dropFtsIndex(field) {
        const ptr0 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_dropFtsIndex(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Drop a secondary index.
     * @param {string} field
     */
    dropIndex(field) {
        const ptr0 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_dropIndex(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Drop a vector index (and its HNSW graph if present).
     * @param {string} field
     */
    dropVectorIndex(field) {
        const ptr0 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_dropVectorIndex(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Find documents matching the filter. Returns a JS array of plain objects.
     * @param {any} filter
     * @returns {any}
     */
    find(filter) {
        const ret = wasm.collectionwasm_find(this.__wbg_ptr, filter);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Find the `top_k` nearest documents to `query` on a vector index.
     *
     * `filter` - optional pre-filter (same format as `find`). Pass `null` to
     *            search across all documents that have the vector field.
     *
     * Returns a JSON array of `{ document: {...}, score: number }` objects.
     * @param {string} field
     * @param {Float32Array} query
     * @param {number} top_k
     * @param {any} filter
     * @returns {any}
     */
    findNearest(field, query, top_k, filter) {
        const ptr0 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayF32ToWasm0(query, wasm.__wbindgen_malloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_findNearest(this.__wbg_ptr, ptr0, len0, ptr1, len1, top_k, filter);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Find a single document. Returns the document or null.
     * @param {any} filter
     * @returns {any}
     */
    findOne(filter) {
        const ret = wasm.collectionwasm_findOne(this.__wbg_ptr, filter);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Hybrid retrieval — BM25 and vector similarity fused with reciprocal
     * rank fusion.
     *
     * `options` accepts `{ rrfK, textWeight, vectorWeight, candidates, k1, b }`.
     * Returns a JSON array of `{ document, score, textRank, vectorRank }`.
     * @param {string} text_field
     * @param {string} text
     * @param {string} vector_field
     * @param {Float32Array} vector
     * @param {number} top_k
     * @param {any} filter
     * @param {any} options
     * @returns {any}
     */
    hybridSearch(text_field, text, vector_field, vector, top_k, filter, options) {
        const ptr0 = passStringToWasm0(text_field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(text, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(vector_field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passArrayF32ToWasm0(vector, wasm.__wbindgen_malloc);
        const len3 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_hybridSearch(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, top_k, filter, options);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Insert a document. Accepts a plain JS object, returns the ULID string id.
     * @param {any} doc
     * @returns {string}
     */
    insert(doc) {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.collectionwasm_insert(this.__wbg_ptr, doc);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * Insert multiple documents. Returns an array of ULID string ids.
     * @param {any} docs
     * @returns {any}
     */
    insertMany(docs) {
        const ret = wasm.collectionwasm_insertMany(this.__wbg_ptr, docs);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Rank documents against a free-text query using BM25 (OR semantics).
     *
     * Returns a JSON array of `{ document, score }`.
     * @param {string} field
     * @param {string} query
     * @param {number} top_k
     * @param {any} filter
     * @param {any} options
     * @returns {any}
     */
    searchText(field, query, top_k, filter, options) {
        const ptr0 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(query, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_searchText(this.__wbg_ptr, ptr0, len0, ptr1, len1, top_k, filter, options);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return takeFromExternrefTable0(ret[0]);
    }
    /**
     * Update all matching documents. Returns the count updated.
     * @param {any} filter
     * @param {any} update
     * @returns {number}
     */
    updateMany(filter, update) {
        const ret = wasm.collectionwasm_updateMany(this.__wbg_ptr, filter, update);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * Update the first matching document. Returns true if a document was updated.
     * @param {any} filter
     * @param {any} update
     * @returns {boolean}
     */
    updateOne(filter, update) {
        const ret = wasm.collectionwasm_updateOne(this.__wbg_ptr, filter, update);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] !== 0;
    }
    /**
     * Rebuild the HNSW graph from the current flat vector table.
     * @param {string} field
     */
    upgradeVectorIndex(field) {
        const ptr0 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.collectionwasm_upgradeVectorIndex(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
}
if (Symbol.dispose) CollectionWasm.prototype[Symbol.dispose] = CollectionWasm.prototype.free;

export class TalaDBWasm {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(TalaDBWasm.prototype);
        obj.__wbg_ptr = ptr;
        TalaDBWasmFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        TalaDBWasmFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_taladbwasm_free(ptr, 0);
    }
    /**
     * Get a collection handle by name.
     * @param {string} name
     * @returns {CollectionWasm}
     */
    collection(name) {
        const ptr0 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.taladbwasm_collection(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return CollectionWasm.__wrap(ret[0]);
    }
    /**
     * Serialize the entire in-memory database to bytes.
     *
     * Pass the returned `Uint8Array` to `opfs_flush_snapshot` to persist, or
     * store it yourself.  On the next page load, pass the same bytes to
     * `openWithSnapshot` to restore all data.
     * @returns {Uint8Array}
     */
    exportSnapshot() {
        const ret = wasm.taladbwasm_exportSnapshot(this.__wbg_ptr);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * User collection names (reserved `_`-prefixed collections excluded).
     * @returns {string[]}
     */
    listCollectionNames() {
        const ret = wasm.taladbwasm_listCollectionNames(this.__wbg_ptr);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v1 = getArrayJsValueFromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 4, 4);
        return v1;
    }
    /**
     * Open an in-memory database (suitable for tests and environments without OPFS).
     * @returns {TalaDBWasm}
     */
    static openInMemory() {
        const ret = wasm.taladbwasm_openInMemory();
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return TalaDBWasm.__wrap(ret[0]);
    }
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
     * @param {Uint8Array | null} [snapshot]
     * @returns {TalaDBWasm}
     */
    static openWithSnapshot(snapshot) {
        var ptr0 = isLikeNone(snapshot) ? 0 : passArray8ToWasm0(snapshot, wasm.__wbindgen_malloc);
        var len0 = WASM_VECTOR_LEN;
        const ret = wasm.taladbwasm_openWithSnapshot(ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return TalaDBWasm.__wrap(ret[0]);
    }
    /**
     * Persist the application migration version. Called after each migration's
     * body succeeds so a crash mid-run resumes from the last applied version.
     * @param {number} version
     */
    setUserVersion(version) {
        const ret = wasm.taladbwasm_setUserVersion(this.__wbg_ptr, version);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Read the current application migration version (0 if never set). Backs
     * the `openDB({ migrations })` runner, which advances it per migration.
     * @returns {number}
     */
    userVersion() {
        const ret = wasm.taladbwasm_userVersion(this.__wbg_ptr);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
}
if (Symbol.dispose) TalaDBWasm.prototype[Symbol.dispose] = TalaDBWasm.prototype.free;

export class WorkerDB {
    static __wrap(ptr) {
        ptr = ptr >>> 0;
        const obj = Object.create(WorkerDB.prototype);
        obj.__wbg_ptr = ptr;
        WorkerDBFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WorkerDBFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_workerdb_free(ptr, 0);
    }
    /**
     * Run an aggregation pipeline. Returns a JSON array of result documents.
     * @param {string} collection
     * @param {string} pipeline_json
     * @returns {string}
     */
    aggregate(collection, pipeline_json) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(pipeline_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            const ret = wasm.workerdb_aggregate(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
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
    compact() {
        const ret = wasm.workerdb_compact(this.__wbg_ptr);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Count matching documents.
     * @param {string} collection
     * @param {string} filter_json
     * @returns {number}
     */
    count(collection, filter_json) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(filter_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_count(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * Create a compound index. `fields_json` is a JSON array of field names.
     * @param {string} collection
     * @param {string} fields_json
     */
    createCompoundIndex(collection, fields_json) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(fields_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_createCompoundIndex(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * @param {string} collection
     * @param {string} field
     */
    createFtsIndex(collection, field) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_createFtsIndex(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * @param {string} collection
     * @param {string} field
     */
    createIndex(collection, field) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_createIndex(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Create a vector index.
     *
     * - `metric_str`: `"cosine"` (default) | `"dot"` | `"euclidean"`
     * - `index_type`: `"flat"` (default) | `"hnsw"`
     * - `hnsw_m`: HNSW connectivity (default 16, only used when `index_type = "hnsw"`)
     * - `hnsw_ef_construction`: build-time quality (default 200, only used when `index_type = "hnsw"`)
     * @param {string} collection
     * @param {string} field
     * @param {number} dimensions
     * @param {string | null} [metric_str]
     * @param {string | null} [index_type]
     * @param {number | null} [hnsw_m]
     * @param {number | null} [hnsw_ef_construction]
     */
    createVectorIndex(collection, field, dimensions, metric_str, index_type, hnsw_m, hnsw_ef_construction) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(metric_str) ? 0 : passStringToWasm0(metric_str, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        var len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(index_type) ? 0 : passStringToWasm0(index_type, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        var len3 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_createVectorIndex(this.__wbg_ptr, ptr0, len0, ptr1, len1, dimensions, ptr2, len2, ptr3, len3, isLikeNone(hnsw_m) ? 0x100000001 : (hnsw_m) >>> 0, isLikeNone(hnsw_ef_construction) ? 0x100000001 : (hnsw_ef_construction) >>> 0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Delete all matching documents. Returns the count deleted.
     * @param {string} collection
     * @param {string} filter_json
     * @returns {number}
     */
    deleteMany(collection, filter_json) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(filter_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_deleteMany(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * Delete documents by id. Returns how many were present and removed.
     * The delete half of cross-tab write propagation.
     * @param {string} collection
     * @param {string} ids_json
     * @returns {number}
     */
    deleteManyWithIds(collection, ids_json) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(ids_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_deleteManyWithIds(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * Delete the first matching document. Returns `true` / `false`.
     * @param {string} collection
     * @param {string} filter_json
     * @returns {boolean}
     */
    deleteOne(collection, filter_json) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(filter_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_deleteOne(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] !== 0;
    }
    /**
     * Drop a compound index by its ordered field list (`fields_json`).
     * @param {string} collection
     * @param {string} fields_json
     */
    dropCompoundIndex(collection, fields_json) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(fields_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_dropCompoundIndex(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * @param {string} collection
     * @param {string} field
     */
    dropFtsIndex(collection, field) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_dropFtsIndex(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * @param {string} collection
     * @param {string} field
     */
    dropIndex(collection, field) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_dropIndex(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Drop a vector index (and its HNSW graph if present).
     * @param {string} collection
     * @param {string} field
     */
    dropVectorIndex(collection, field) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_dropVectorIndex(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Serialize the entire in-memory database to bytes for persistence.
     *
     * Pass the returned bytes to `idbSaveSnapshot` to persist across page reloads.
     * On next open, pass the same bytes to `openWithSnapshot` to restore all data.
     * @returns {Uint8Array}
     */
    exportSnapshot() {
        const ret = wasm.workerdb_exportSnapshot(this.__wbg_ptr);
        if (ret[3]) {
            throw takeFromExternrefTable0(ret[2]);
        }
        var v1 = getArrayU8FromWasm0(ret[0], ret[1]).slice();
        wasm.__wbindgen_free(ret[0], ret[1] * 1, 1);
        return v1;
    }
    /**
     * Find documents. Returns a JSON array of document objects.
     * @param {string} collection
     * @param {string} filter_json
     * @returns {string}
     */
    find(collection, filter_json) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(filter_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            const ret = wasm.workerdb_find(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
    /**
     * Find nearest neighbours. Returns a JSON string of `[{ document, score }]`.
     * @param {string} collection
     * @param {string} field
     * @param {string} query_json
     * @param {number} top_k
     * @param {string} filter_json
     * @returns {string}
     */
    findNearest(collection, field, query_json, top_k, filter_json) {
        let deferred6_0;
        let deferred6_1;
        try {
            const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            const ptr2 = passStringToWasm0(query_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len2 = WASM_VECTOR_LEN;
            const ptr3 = passStringToWasm0(filter_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len3 = WASM_VECTOR_LEN;
            const ret = wasm.workerdb_findNearest(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, top_k, ptr3, len3);
            var ptr5 = ret[0];
            var len5 = ret[1];
            if (ret[3]) {
                ptr5 = 0; len5 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred6_0 = ptr5;
            deferred6_1 = len5;
            return getStringFromWasm0(ptr5, len5);
        } finally {
            wasm.__wbindgen_free(deferred6_0, deferred6_1, 1);
        }
    }
    /**
     * Find one document. Returns a JSON object or `"null"`.
     * @param {string} collection
     * @param {string} filter_json
     * @returns {string}
     */
    findOne(collection, filter_json) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(filter_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            const ret = wasm.workerdb_findOne(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
    /**
     * Force batched (eventual) OPFS writes to durable storage. No-op under the
     * default immediate durability. Backs `db.flush()`.
     */
    flush() {
        const ret = wasm.workerdb_flush(this.__wbg_ptr);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Hybrid retrieval — BM25 and vector similarity fused with reciprocal
     * rank fusion.
     *
     * `options_json` accepts `{ rrfK, textWeight, vectorWeight, candidates, k1, b }`.
     * Returns a JSON array of `{ document, score, textRank, vectorRank }`.
     * @param {string} collection
     * @param {string} text_field
     * @param {string} text
     * @param {string} vector_field
     * @param {string} vector_json
     * @param {number} top_k
     * @param {string} filter_json
     * @param {string} options_json
     * @returns {string}
     */
    hybridSearch(collection, text_field, text, vector_field, vector_json, top_k, filter_json, options_json) {
        let deferred9_0;
        let deferred9_1;
        try {
            const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(text_field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            const ptr2 = passStringToWasm0(text, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len2 = WASM_VECTOR_LEN;
            const ptr3 = passStringToWasm0(vector_field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len3 = WASM_VECTOR_LEN;
            const ptr4 = passStringToWasm0(vector_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len4 = WASM_VECTOR_LEN;
            const ptr5 = passStringToWasm0(filter_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len5 = WASM_VECTOR_LEN;
            const ptr6 = passStringToWasm0(options_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len6 = WASM_VECTOR_LEN;
            const ret = wasm.workerdb_hybridSearch(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, top_k, ptr5, len5, ptr6, len6);
            var ptr8 = ret[0];
            var len8 = ret[1];
            if (ret[3]) {
                ptr8 = 0; len8 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred9_0 = ptr8;
            deferred9_1 = len8;
            return getStringFromWasm0(ptr8, len8);
        } finally {
            wasm.__wbindgen_free(deferred9_0, deferred9_1, 1);
        }
    }
    /**
     * Insert a document. Returns the new ULID as a string.
     * @param {string} collection
     * @param {string} doc_json
     * @returns {string}
     */
    insert(collection, doc_json) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(doc_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            const ret = wasm.workerdb_insert(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
    /**
     * Insert many documents. Returns a JSON array of ULID strings.
     * @param {string} collection
     * @param {string} docs_json
     * @returns {string}
     */
    insertMany(collection, docs_json) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(docs_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            const ret = wasm.workerdb_insertMany(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
    /**
     * Returns a JSON array of all collection names in the database.
     * @returns {string}
     */
    listCollections() {
        let deferred2_0;
        let deferred2_1;
        try {
            const ret = wasm.workerdb_listCollections(this.__wbg_ptr);
            var ptr1 = ret[0];
            var len1 = ret[1];
            if (ret[3]) {
                ptr1 = 0; len1 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred2_0 = ptr1;
            deferred2_1 = len1;
            return getStringFromWasm0(ptr1, len1);
        } finally {
            wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
        }
    }
    /**
     * Returns a JSON string `{ btree: string[], fts: string[], vector: string[] }`
     * listing all indexes on the given collection.
     * @param {string} collection
     * @returns {string}
     */
    listIndexes(collection) {
        let deferred3_0;
        let deferred3_1;
        try {
            const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ret = wasm.workerdb_listIndexes(this.__wbg_ptr, ptr0, len0);
            var ptr2 = ret[0];
            var len2 = ret[1];
            if (ret[3]) {
                ptr2 = 0; len2 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred3_0 = ptr2;
            deferred3_1 = len2;
            return getStringFromWasm0(ptr2, len2);
        } finally {
            wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
        }
    }
    /**
     * Open an in-memory database (for tests and OPFS-unavailable fallback).
     * @returns {WorkerDB}
     */
    static openInMemory() {
        const ret = wasm.workerdb_openInMemory();
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return WorkerDB.__wrap(ret[0]);
    }
    /**
     * Open a database backed by OPFS with HTTP push sync config.
     *
     * Not available when compiled with the `cf-workers` feature.
     *
     * `config_json` - JSON-serialised `TalaDbConfig`, or `null` to open without sync.
     *
     * ```js
     * const handle = await file_handle.createSyncAccessHandle();
     * const db = WorkerDB.openWithConfigAndOpfs(handle, JSON.stringify(config));
     * ```
     * @param {FileSystemSyncAccessHandle} sync_handle
     * @param {string | null} [config_json]
     * @param {string | null} [passphrase]
     * @param {Uint8Array | null} [salt]
     * @returns {WorkerDB}
     */
    static openWithConfigAndOpfs(sync_handle, config_json, passphrase, salt) {
        var ptr0 = isLikeNone(config_json) ? 0 : passStringToWasm0(config_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(passphrase) ? 0 : passStringToWasm0(passphrase, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(salt) ? 0 : passArray8ToWasm0(salt, wasm.__wbindgen_malloc);
        var len2 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_openWithConfigAndOpfs(sync_handle, ptr0, len0, ptr1, len1, ptr2, len2);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return WorkerDB.__wrap(ret[0]);
    }
    /**
     * Open a database from an optional snapshot with HTTP push sync config.
     *
     * `config_json` - JSON-serialised `TalaDbConfig`, or `null` to open without sync.
     *
     * ```js
     * const db = WorkerDB.openWithConfigAndSnapshot(snapshot, JSON.stringify(config));
     * ```
     * @param {Uint8Array | null} [data]
     * @param {string | null} [config_json]
     * @returns {WorkerDB}
     */
    static openWithConfigAndSnapshot(data, config_json) {
        var ptr0 = isLikeNone(data) ? 0 : passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(config_json) ? 0 : passStringToWasm0(config_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        var len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_openWithConfigAndSnapshot(ptr0, len0, ptr1, len1);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return WorkerDB.__wrap(ret[0]);
    }
    /**
     * Open a database backed by an OPFS `FileSystemSyncAccessHandle`.
     *
     * Not available when compiled with the `cf-workers` feature.
     *
     * Call sequence in the SharedWorker:
     * ```js
     * const handle = await file_handle.createSyncAccessHandle();
     * const workerDb = WorkerDB.openWithOpfs(handle);
     * ```
     * @param {FileSystemSyncAccessHandle} sync_handle
     * @returns {WorkerDB}
     */
    static openWithOpfs(sync_handle) {
        const ret = wasm.workerdb_openWithOpfs(sync_handle);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return WorkerDB.__wrap(ret[0]);
    }
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
     * @param {Uint8Array | null} [data]
     * @returns {WorkerDB}
     */
    static openWithSnapshot(data) {
        var ptr0 = isLikeNone(data) ? 0 : passArray8ToWasm0(data, wasm.__wbindgen_malloc);
        var len0 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_openWithSnapshot(ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return WorkerDB.__wrap(ret[0]);
    }
    /**
     * Rank documents against a free-text query using BM25 (OR semantics).
     *
     * Returns a JSON array of `{ document, score }`.
     * @param {string} collection
     * @param {string} field
     * @param {string} query
     * @param {number} top_k
     * @param {string} filter_json
     * @param {string} options_json
     * @returns {string}
     */
    searchText(collection, field, query, top_k, filter_json, options_json) {
        let deferred7_0;
        let deferred7_1;
        try {
            const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            const ptr2 = passStringToWasm0(query, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len2 = WASM_VECTOR_LEN;
            const ptr3 = passStringToWasm0(filter_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len3 = WASM_VECTOR_LEN;
            const ptr4 = passStringToWasm0(options_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len4 = WASM_VECTOR_LEN;
            const ret = wasm.workerdb_searchText(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, top_k, ptr3, len3, ptr4, len4);
            var ptr6 = ret[0];
            var len6 = ret[1];
            if (ret[3]) {
                ptr6 = 0; len6 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred7_0 = ptr6;
            deferred7_1 = len6;
            return getStringFromWasm0(ptr6, len6);
        } finally {
            wasm.__wbindgen_free(deferred7_0, deferred7_1, 1);
        }
    }
    /**
     * Set write durability: `eventual = true` batches OPFS fsyncs for
     * throughput (call `flush()` to force), `false` (default) fsyncs each
     * commit. Derived from `durability.flush_every_write` by the worker.
     * @param {boolean} eventual
     */
    setDurability(eventual) {
        wasm.workerdb_setDurability(this.__wbg_ptr, eventual);
    }
    /**
     * Persist the application migration version. Called after each migration's
     * body succeeds so a crash mid-run resumes from the last applied version.
     * @param {number} version
     */
    setUserVersion(version) {
        const ret = wasm.workerdb_setUserVersion(this.__wbg_ptr, version);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Update all matching documents. Returns the count updated.
     * @param {string} collection
     * @param {string} filter_json
     * @param {string} update_json
     * @returns {number}
     */
    updateMany(collection, filter_json, update_json) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(filter_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(update_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_updateMany(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * Update the first matching document. Returns `true` / `false`.
     * @param {string} collection
     * @param {string} filter_json
     * @param {string} update_json
     * @returns {boolean}
     */
    updateOne(collection, filter_json, update_json) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(filter_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(update_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_updateOne(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] !== 0;
    }
    /**
     * Rebuild the HNSW graph for a vector index from the current flat vector
     * table.  Use after bulk inserts or when ANN recall has degraded.
     *
     * No-op when the `vector-hnsw` feature is disabled or the index is flat-only.
     * @param {string} collection
     * @param {string} field
     */
    upgradeVectorIndex(collection, field) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(field, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_upgradeVectorIndex(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
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
     * @param {string} collection
     * @param {string} docs_json
     * @returns {string}
     */
    upsertManyWithIds(collection, docs_json) {
        let deferred4_0;
        let deferred4_1;
        try {
            const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(docs_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            const ret = wasm.workerdb_upsertManyWithIds(this.__wbg_ptr, ptr0, len0, ptr1, len1);
            var ptr3 = ret[0];
            var len3 = ret[1];
            if (ret[3]) {
                ptr3 = 0; len3 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred4_0 = ptr3;
            deferred4_1 = len3;
            return getStringFromWasm0(ptr3, len3);
        } finally {
            wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
        }
    }
    /**
     * Read the current application migration version (0 if never set). Backs
     * the `openDB({ migrations })` runner, which advances it per migration.
     * @returns {number}
     */
    userVersion() {
        const ret = wasm.workerdb_userVersion(this.__wbg_ptr);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0] >>> 0;
    }
    /**
     * This collection's write generation — a counter bumped once per committed
     * mutation.
     *
     * Backs `subscribe()`: a live query can compare one integer per tick
     * instead of re-running the query and `JSON.stringify`-ing the whole
     * result set to see whether anything moved.
     * @param {string} collection
     * @returns {number}
     */
    writeGeneration(collection) {
        const ptr0 = passStringToWasm0(collection, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.workerdb_writeGeneration(this.__wbg_ptr, ptr0, len0);
        if (ret[2]) {
            throw takeFromExternrefTable0(ret[1]);
        }
        return ret[0];
    }
}
if (Symbol.dispose) WorkerDB.prototype[Symbol.dispose] = WorkerDB.prototype.free;

/**
 * Load a previous database snapshot from IndexedDB.
 * Returns `None` if no snapshot exists yet (first open) or IDB is unavailable.
 * @param {string} db_name
 * @returns {Promise<Uint8Array | undefined>}
 */
export function idb_load_snapshot(db_name) {
    const ptr0 = passStringToWasm0(db_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.idb_load_snapshot(ptr0, len0);
    return ret;
}

/**
 * Persist a database snapshot to IndexedDB.
 * Returns `true` on success, `false` on any failure.
 * @param {string} db_name
 * @param {Uint8Array} data
 * @returns {Promise<boolean>}
 */
export function idb_save_snapshot(db_name, data) {
    const ptr0 = passStringToWasm0(db_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.idb_save_snapshot(ptr0, len0, ptr1, len1);
    return ret;
}

/**
 * Initialize panic hook for better error messages in the browser console.
 */
export function init() {
    wasm.init();
}

/**
 * Returns true if OPFS is available in the current browser context.
 * Always returns false in Workers without storage access.
 * @returns {Promise<boolean>}
 */
export function is_opfs_available() {
    const ret = wasm.is_opfs_available();
    return ret;
}

/**
 * Delete the OPFS snapshot file for `db_name`.
 * No-op if the file does not exist.
 * @param {string} db_name
 * @returns {Promise<boolean>}
 */
export function opfs_delete_snapshot(db_name) {
    const ptr0 = passStringToWasm0(db_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.opfs_delete_snapshot(ptr0, len0);
    return ret;
}

/**
 * Persist a database snapshot to OPFS.
 * Creates the file on first call. Subsequent calls overwrite atomically.
 * @param {string} db_name
 * @param {Uint8Array} data
 * @returns {Promise<boolean>}
 */
export function opfs_flush_snapshot(db_name, data) {
    const ptr0 = passStringToWasm0(db_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passArray8ToWasm0(data, wasm.__wbindgen_malloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.opfs_flush_snapshot(ptr0, len0, ptr1, len1);
    return ret;
}

/**
 * Load the last persisted database snapshot from OPFS.
 * Returns `None` if the file does not exist yet (first open).
 * @param {string} db_name
 * @returns {Promise<Uint8Array | undefined>}
 */
export function opfs_load_snapshot(db_name) {
    const ptr0 = passStringToWasm0(db_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.opfs_load_snapshot(ptr0, len0);
    return ret;
}

/**
 * Open (or create) an OPFS file and return an `OpfsBackend` for redb.
 *
 * This function is **async** because `getFileHandle` and `createSyncAccessHandle`
 * are both async in the OPFS API. Call it once at worker startup.
 * @param {string} db_name
 * @returns {Promise<any>}
 */
export function opfs_open_backend(db_name) {
    const ptr0 = passStringToWasm0(db_name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.opfs_open_backend(ptr0, len0);
    return ret;
}

function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg_Error_83742b46f01ce22d: function(arg0, arg1) {
            const ret = Error(getStringFromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_String_8564e559799eccda: function(arg0, arg1) {
            const ret = String(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_bigint_get_as_i64_447a76b5c6ef7bda: function(arg0, arg1) {
            const v = arg1;
            const ret = typeof(v) === 'bigint' ? v : undefined;
            getDataViewMemory0().setBigInt64(arg0 + 8 * 1, isLikeNone(ret) ? BigInt(0) : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_boolean_get_c0f3f60bac5a78d1: function(arg0) {
            const v = arg0;
            const ret = typeof(v) === 'boolean' ? v : undefined;
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg___wbindgen_debug_string_5398f5bb970e0daa: function(arg0, arg1) {
            const ret = debugString(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_in_41dbb8413020e076: function(arg0, arg1) {
            const ret = arg0 in arg1;
            return ret;
        },
        __wbg___wbindgen_is_bigint_e2141d4f045b7eda: function(arg0) {
            const ret = typeof(arg0) === 'bigint';
            return ret;
        },
        __wbg___wbindgen_is_function_3c846841762788c1: function(arg0) {
            const ret = typeof(arg0) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_null_0b605fc6b167c56f: function(arg0) {
            const ret = arg0 === null;
            return ret;
        },
        __wbg___wbindgen_is_object_781bc9f159099513: function(arg0) {
            const val = arg0;
            const ret = typeof(val) === 'object' && val !== null;
            return ret;
        },
        __wbg___wbindgen_is_string_7ef6b97b02428fae: function(arg0) {
            const ret = typeof(arg0) === 'string';
            return ret;
        },
        __wbg___wbindgen_is_undefined_52709e72fb9f179c: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_jsval_eq_ee31bfad3e536463: function(arg0, arg1) {
            const ret = arg0 === arg1;
            return ret;
        },
        __wbg___wbindgen_jsval_loose_eq_5bcc3bed3c69e72b: function(arg0, arg1) {
            const ret = arg0 == arg1;
            return ret;
        },
        __wbg___wbindgen_number_get_34bb9d9dcfa21373: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_string_get_395e606bd0ee4427: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_6ddd609b62940d55: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_6b5b6b8576d35cb1: function(arg0) {
            arg0._wbg_cb_unref();
        },
        __wbg_apply_ac9afb97ca32f169: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.apply(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_arrayBuffer_7ff5e58aa85a64f7: function(arg0) {
            const ret = arg0.arrayBuffer();
            return ret;
        },
        __wbg_call_2d781c1f4d5c0ef8: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.call(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_call_dcc2662fa17a72cf: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = arg0.call(arg1, arg2, arg3);
            return ret;
        }, arguments); },
        __wbg_call_e133b57c9155d22c: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.call(arg1);
            return ret;
        }, arguments); },
        __wbg_close_8b24f0978d9a4917: function(arg0) {
            const ret = arg0.close();
            return ret;
        },
        __wbg_createWritable_d2f6937622592d78: function(arg0) {
            const ret = arg0.createWritable();
            return ret;
        },
        __wbg_crypto_38df2bab126b63dc: function(arg0) {
            const ret = arg0.crypto;
            return ret;
        },
        __wbg_done_08ce71ee07e3bd17: function(arg0) {
            const ret = arg0.done;
            return ret;
        },
        __wbg_entries_e8a20ff8c9757101: function(arg0) {
            const ret = Object.entries(arg0);
            return ret;
        },
        __wbg_error_a6fa202b58aa1cd3: function(arg0, arg1) {
            let deferred0_0;
            let deferred0_1;
            try {
                deferred0_0 = arg0;
                deferred0_1 = arg1;
                console.error(getStringFromWasm0(arg0, arg1));
            } finally {
                wasm.__wbindgen_free(deferred0_0, deferred0_1, 1);
            }
        },
        __wbg_flush_1eca046e0ff7c399: function() { return handleError(function (arg0) {
            arg0.flush();
        }, arguments); },
        __wbg_from_4bdf88943703fd48: function(arg0) {
            const ret = Array.from(arg0);
            return ret;
        },
        __wbg_getDirectory_2406d369de179ff0: function(arg0) {
            const ret = arg0.getDirectory();
            return ret;
        },
        __wbg_getFileHandle_b85805519dc63efa: function(arg0, arg1, arg2, arg3) {
            const ret = arg0.getFileHandle(getStringFromWasm0(arg1, arg2), arg3);
            return ret;
        },
        __wbg_getFile_30341f4730c33225: function(arg0) {
            const ret = arg0.getFile();
            return ret;
        },
        __wbg_getRandomValues_3f44b700395062e5: function() { return handleError(function (arg0, arg1) {
            globalThis.crypto.getRandomValues(getArrayU8FromWasm0(arg0, arg1));
        }, arguments); },
        __wbg_getRandomValues_c44a50d8cfdaebeb: function() { return handleError(function (arg0, arg1) {
            arg0.getRandomValues(arg1);
        }, arguments); },
        __wbg_getSize_0a16c5e2524d34aa: function() { return handleError(function (arg0) {
            const ret = arg0.getSize();
            return ret;
        }, arguments); },
        __wbg_get_326e41e095fb2575: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_get_3ef1eba1850ade27: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_get_a8ee5c45dabc1b3b: function(arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        },
        __wbg_get_unchecked_329cfe50afab7352: function(arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        },
        __wbg_instanceof_ArrayBuffer_101e2bf31071a9f6: function(arg0) {
            let result;
            try {
                result = arg0 instanceof ArrayBuffer;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_FileSystemDirectoryHandle_2944d0641b4ea10c: function(arg0) {
            let result;
            try {
                result = arg0 instanceof FileSystemDirectoryHandle;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_FileSystemFileHandle_37ac45c6adcff28f: function(arg0) {
            let result;
            try {
                result = arg0 instanceof FileSystemFileHandle;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_FileSystemSyncAccessHandle_dc45d7dabb2f5ad9: function(arg0) {
            let result;
            try {
                result = arg0 instanceof FileSystemSyncAccessHandle;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_FileSystemWritableFileStream_4b3d3484a5ead457: function(arg0) {
            let result;
            try {
                result = arg0 instanceof FileSystemWritableFileStream;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_File_a301c444111d30cb: function(arg0) {
            let result;
            try {
                result = arg0 instanceof File;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Map_f194b366846aca0c: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Map;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Promise_7c3bdd7805c2c6e6: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Promise;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Uint8Array_740438561a5b956d: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Uint8Array;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Window_23e677d2c6843922: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_isArray_33b91feb269ff46e: function(arg0) {
            const ret = Array.isArray(arg0);
            return ret;
        },
        __wbg_isSafeInteger_ecd6a7f9c3e053cd: function(arg0) {
            const ret = Number.isSafeInteger(arg0);
            return ret;
        },
        __wbg_iterator_d8f549ec8fb061b1: function() {
            const ret = Symbol.iterator;
            return ret;
        },
        __wbg_length_b3416cf66a5452c8: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_length_ea16607d7b61445b: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_msCrypto_bd5a034af96bcba6: function(arg0) {
            const ret = arg0.msCrypto;
            return ret;
        },
        __wbg_navigator_9cebf56f28aa719b: function(arg0) {
            const ret = arg0.navigator;
            return ret;
        },
        __wbg_new_227d7c05414eb861: function() {
            const ret = new Error();
            return ret;
        },
        __wbg_new_49d5571bd3f0c4d4: function() {
            const ret = new Map();
            return ret;
        },
        __wbg_new_5f486cdf45a04d78: function(arg0) {
            const ret = new Uint8Array(arg0);
            return ret;
        },
        __wbg_new_a70fbab9066b301f: function() {
            const ret = new Array();
            return ret;
        },
        __wbg_new_ab79df5bd7c26067: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_new_d098e265629cd10f: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return wasm_bindgen__convert__closures_____invoke__h0dbbf48826ad16e8(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return ret;
            } finally {
                state0.a = state0.b = 0;
            }
        },
        __wbg_new_from_slice_22da9388ac046e50: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_new_typed_aaaeaf29cf802876: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return wasm_bindgen__convert__closures_____invoke__h0dbbf48826ad16e8(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return ret;
            } finally {
                state0.a = state0.b = 0;
            }
        },
        __wbg_new_with_length_825018a1616e9e55: function(arg0) {
            const ret = new Uint8Array(arg0 >>> 0);
            return ret;
        },
        __wbg_next_11b99ee6237339e3: function() { return handleError(function (arg0) {
            const ret = arg0.next();
            return ret;
        }, arguments); },
        __wbg_next_e01a967809d1aa68: function(arg0) {
            const ret = arg0.next;
            return ret;
        },
        __wbg_node_84ea875411254db1: function(arg0) {
            const ret = arg0.node;
            return ret;
        },
        __wbg_now_16f0c993d5dd6c27: function() {
            const ret = Date.now();
            return ret;
        },
        __wbg_process_44c7a14e11e9f69e: function(arg0) {
            const ret = arg0.process;
            return ret;
        },
        __wbg_prototypesetcall_d62e5099504357e6: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), arg2);
        },
        __wbg_push_e87b0e732085a946: function(arg0, arg1) {
            const ret = arg0.push(arg1);
            return ret;
        },
        __wbg_queueMicrotask_0c399741342fb10f: function(arg0) {
            const ret = arg0.queueMicrotask;
            return ret;
        },
        __wbg_queueMicrotask_a082d78ce798393e: function(arg0) {
            queueMicrotask(arg0);
        },
        __wbg_randomFillSync_6c25eac9869eb53c: function() { return handleError(function (arg0, arg1) {
            arg0.randomFillSync(arg1);
        }, arguments); },
        __wbg_removeEntry_b2a4d6d0ee2040c5: function(arg0, arg1, arg2) {
            const ret = arg0.removeEntry(getStringFromWasm0(arg1, arg2));
            return ret;
        },
        __wbg_require_b4edbdcf3e2a1ef0: function() { return handleError(function () {
            const ret = module.require;
            return ret;
        }, arguments); },
        __wbg_resolve_ae8d83246e5bcc12: function(arg0) {
            const ret = Promise.resolve(arg0);
            return ret;
        },
        __wbg_set_282384002438957f: function(arg0, arg1, arg2) {
            arg0[arg1 >>> 0] = arg2;
        },
        __wbg_set_6be42768c690e380: function(arg0, arg1, arg2) {
            arg0[arg1] = arg2;
        },
        __wbg_set_7eaa4f96924fd6b3: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(arg0, arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_set_bf7251625df30a02: function(arg0, arg1, arg2) {
            const ret = arg0.set(arg1, arg2);
            return ret;
        },
        __wbg_set_create_ef897736206a6f05: function(arg0, arg1) {
            arg0.create = arg1 !== 0;
        },
        __wbg_stack_3b0d974bbf31e44f: function(arg0, arg1) {
            const ret = arg1.stack;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_static_accessor_GLOBAL_8adb955bd33fac2f: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_ad356e0db91c7913: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_SELF_f207c857566db248: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_bb9f1ba69d61b386: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_storage_63fa6fa9455d2153: function(arg0) {
            const ret = arg0.storage;
            return ret;
        },
        __wbg_subarray_a068d24e39478a8a: function(arg0, arg1, arg2) {
            const ret = arg0.subarray(arg1 >>> 0, arg2 >>> 0);
            return ret;
        },
        __wbg_then_098abe61755d12f6: function(arg0, arg1) {
            const ret = arg0.then(arg1);
            return ret;
        },
        __wbg_then_9e335f6dd892bc11: function(arg0, arg1, arg2) {
            const ret = arg0.then(arg1, arg2);
            return ret;
        },
        __wbg_truncate_af8ac1613ab66393: function() { return handleError(function (arg0, arg1) {
            arg0.truncate(arg1);
        }, arguments); },
        __wbg_value_21fc78aab0322612: function(arg0) {
            const ret = arg0.value;
            return ret;
        },
        __wbg_versions_276b2795b1c6a219: function(arg0) {
            const ret = arg0.versions;
            return ret;
        },
        __wbindgen_cast_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 5, function: Function { arguments: [Externref], shim_idx: 6, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen__closure__destroy__h4ecd198e6e5fb530, wasm_bindgen__convert__closures_____invoke__hbf94730c3811ffd3);
            return ret;
        },
        __wbindgen_cast_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { dtor_idx: 525, function: Function { arguments: [Externref], shim_idx: 526, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm.wasm_bindgen__closure__destroy__h014c297fadd2a065, wasm_bindgen__convert__closures_____invoke__h20bda61557acb630);
            return ret;
        },
        __wbindgen_cast_0000000000000003: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000004: function(arg0) {
            // Cast intrinsic for `I64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_cast_0000000000000005: function(arg0, arg1) {
            // Cast intrinsic for `Ref(Slice(U8)) -> NamedExternref("Uint8Array")`.
            const ret = getArrayU8FromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_cast_0000000000000006: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_cast_0000000000000007: function(arg0) {
            // Cast intrinsic for `U64 -> Externref`.
            const ret = BigInt.asUintN(64, arg0);
            return ret;
        },
        __wbindgen_cast_0000000000000008: function(arg0, arg1) {
            var v0 = getArrayU8FromWasm0(arg0, arg1).slice();
            wasm.__wbindgen_free(arg0, arg1 * 1, 1);
            // Cast intrinsic for `Vector(U8) -> Externref`.
            const ret = v0;
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./taladb_web_bg.js": import0,
    };
}

function wasm_bindgen__convert__closures_____invoke__hbf94730c3811ffd3(arg0, arg1, arg2) {
    wasm.wasm_bindgen__convert__closures_____invoke__hbf94730c3811ffd3(arg0, arg1, arg2);
}

function wasm_bindgen__convert__closures_____invoke__h20bda61557acb630(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen__convert__closures_____invoke__h20bda61557acb630(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen__convert__closures_____invoke__h0dbbf48826ad16e8(arg0, arg1, arg2, arg3) {
    wasm.wasm_bindgen__convert__closures_____invoke__h0dbbf48826ad16e8(arg0, arg1, arg2, arg3);
}

const CollectionWasmFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_collectionwasm_free(ptr >>> 0, 1));
const TalaDBWasmFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_taladbwasm_free(ptr >>> 0, 1));
const WorkerDBFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_workerdb_free(ptr >>> 0, 1));

function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => state.dtor(state.a, state.b));

function debugString(val) {
    // primitive types
    const type = typeof val;
    if (type == 'number' || type == 'boolean' || val == null) {
        return  `${val}`;
    }
    if (type == 'string') {
        return `"${val}"`;
    }
    if (type == 'symbol') {
        const description = val.description;
        if (description == null) {
            return 'Symbol';
        } else {
            return `Symbol(${description})`;
        }
    }
    if (type == 'function') {
        const name = val.name;
        if (typeof name == 'string' && name.length > 0) {
            return `Function(${name})`;
        } else {
            return 'Function';
        }
    }
    // objects
    if (Array.isArray(val)) {
        const length = val.length;
        let debug = '[';
        if (length > 0) {
            debug += debugString(val[0]);
        }
        for(let i = 1; i < length; i++) {
            debug += ', ' + debugString(val[i]);
        }
        debug += ']';
        return debug;
    }
    // Test for built-in
    const builtInMatches = /\[object ([^\]]+)\]/.exec(toString.call(val));
    let className;
    if (builtInMatches && builtInMatches.length > 1) {
        className = builtInMatches[1];
    } else {
        // Failed to match the standard '[object ClassName]'
        return toString.call(val);
    }
    if (className == 'Object') {
        // we're a user defined class or Object
        // JSON.stringify avoids problems with cycles, and is generally much
        // easier than looping through ownProperties of `val`.
        try {
            return 'Object(' + JSON.stringify(val) + ')';
        } catch (_) {
            return 'Object';
        }
    }
    // errors
    if (val instanceof Error) {
        return `${val.name}: ${val.message}\n${val.stack}`;
    }
    // TODO we could test for more things here, like `Set`s and `Map`s.
    return className;
}

function getArrayJsValueFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    const mem = getDataViewMemory0();
    const result = [];
    for (let i = ptr; i < ptr + 4 * len; i += 4) {
        result.push(wasm.__wbindgen_externrefs.get(mem.getUint32(i, true)));
    }
    wasm.__externref_drop_slice(ptr, len);
    return result;
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

let cachedFloat32ArrayMemory0 = null;
function getFloat32ArrayMemory0() {
    if (cachedFloat32ArrayMemory0 === null || cachedFloat32ArrayMemory0.byteLength === 0) {
        cachedFloat32ArrayMemory0 = new Float32Array(wasm.memory.buffer);
    }
    return cachedFloat32ArrayMemory0;
}

function getStringFromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return decodeText(ptr, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeMutClosure(arg0, arg1, dtor, f) {
    const state = { a: arg0, b: arg1, cnt: 1, dtor };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            state.dtor(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passArray8ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 1, 1) >>> 0;
    getUint8ArrayMemory0().set(arg, ptr / 1);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passArrayF32ToWasm0(arg, malloc) {
    const ptr = malloc(arg.length * 4, 4) >>> 0;
    getFloat32ArrayMemory0().set(arg, ptr / 4);
    WASM_VECTOR_LEN = arg.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasm;
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedFloat32ArrayMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = module.ok && expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('taladb_web_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
