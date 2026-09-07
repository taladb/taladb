/** TalaDB React Native public API. CRUD is synchronous through the JSI host. */
import {
  createWebhookDispatcher,
  type HybridSearchOptions,
  type TextSearchOptions,
  type VectorMetric,
  type WebhookConfig,
  type WebhookDispatcher,
  type WebhookEvent,
  type WebhookStats,
} from 'taladb';
import NativeTalaDB from './NativeTalaDB';

export type {
  HybridSearchOptions,
  TextSearchOptions,
  VectorMetric,
} from 'taladb';

export const TalaDBModule = {
  /** Open the native database. The config controls durability/encryption only. */
  initialize: (dbName: string, configJson?: string) =>
    NativeTalaDB.initialize(dbName, configJson),
  close: () => NativeTalaDB.close(),
};

export interface Document {
  _id?: string;
  [key: string]: unknown;
}

export type InsertDocument<T extends Document> = Omit<T, '_id'> & { _id?: string };
export type Filter = Record<string, unknown>;
export type Update = Record<string, unknown>;

/** A query vector. `Float32Array` takes a zero-copy path across JSI. */
export type QueryVector = Float32Array | number[];

/** An ordered MongoDB-style aggregation pipeline. */
export type AggregatePipeline = Record<string, unknown>[];

/** A single result from `findNearest`. */
export interface VectorSearchResult<T extends Document = Document> {
  document: T;
  /**
   * Similarity score — higher means more similar. Range depends on the
   * metric: cosine ∈ [-1,1], dot ∈ ℝ, euclidean ∈ (0,1].
   */
  score: number;
}

/** A single result from `searchText`. */
export interface TextSearchResult<T extends Document = Document> {
  document: T;
  /**
   * BM25 relevance — higher is more relevant. Unbounded above and only
   * meaningful for ordering within one query's result set.
   */
  score: number;
}

/** A single result from `hybridSearch`. */
export interface HybridSearchResult<T extends Document = Document> {
  document: T;
  /**
   * Fused reciprocal-rank-fusion score. Small by construction and meaningful
   * only as an ordering within one result set — never a similarity or a
   * confidence.
   */
  score: number;
  /** Zero-based rank in the text ranking, or `null` if text did not return it. */
  textRank: number | null;
  /** Zero-based rank in the vector ranking, or `null` if vector did not return it. */
  vectorRank: number | null;
}

/**
 * HNSW build parameters.
 *
 * Both fields are required and snake_case: the native side deserialises this
 * object straight into the Rust `HnswOptions`, which declares no serde
 * defaults. A partial or camelCase object fails to parse and is discarded
 * silently, leaving a flat (exact, linear) index behind with no error — so the
 * types here deliberately do not let you write one.
 */
export interface HnswBuildOptions {
  /** Bi-directional links per node. The graph implementation supports only 32. */
  m: number;
  /** Build-time quality (ef during construction). Must be ≥ `m`. Typically 200. */
  ef_construction: number;
}

export interface VectorIndexOptions {
  /** Similarity metric. Defaults to `"cosine"`. */
  metric?: VectorMetric;
  /**
   * Supply this to build an HNSW (approximate) index instead of a flat one.
   *
   * The graph is held in memory only and is **not** persisted, so it is empty
   * after every app launch. Until it is warmed, `findNearest` silently falls
   * back to an exact linear scan. Call `upgradeVectorIndex(field)` once at
   * startup to rebuild it — see that method's docs.
   */
  hnsw?: HnswBuildOptions;
}

export interface Collection<T extends Document = Document> {
  insert(doc: InsertDocument<T>): string;
  insertMany(docs: InsertDocument<T>[]): string[];
  find(filter?: Filter): T[];
  findOne(filter: Filter): T | null;
  updateOne(filter: Filter, update: Update): boolean;
  updateMany(filter: Filter, update: Update): number;
  deleteOne(filter: Filter): boolean;
  deleteMany(filter: Filter): number;
  count(filter?: Filter): number;
  createIndex(field: string): void;
  dropIndex(field: string): void;
  createFtsIndex(field: string): void;
  dropFtsIndex(field: string): void;

  /**
   * Find documents on a background thread. Prefer this over `find` for scans
   * large enough to be felt as a dropped frame — every synchronous method here
   * runs on the JS thread.
   */
  findAsync(filter?: Filter): Promise<T[]>;

  /**
   * Run a MongoDB-style aggregation pipeline: `$match`, `$group`, `$sort`,
   * `$skip`, `$limit`, `$project`, with `$sum` / `$avg` / `$min` / `$max` /
   * `$count` accumulators. `$match` uses an index where one exists.
   *
   * @example
   * const [{ total }] = repairs.aggregate<{ total: number }>([
   *   { $match: { thingId } },
   *   { $group: { _id: null, total: { $sum: '$cost' } } },
   * ]);
   */
  aggregate<R extends Document = Document>(pipeline: AggregatePipeline): R[];

  /**
   * Rank documents by BM25 keyword relevance. OR semantics — a document
   * matching more of the query scores higher. Requires an FTS index on `field`.
   *
   * @example
   * const hits = notes.searchText('body', 'chain replacement', 5);
   */
  searchText(
    field: string,
    query: string,
    topK: number,
    filter?: Filter | null,
    options?: TextSearchOptions,
  ): TextSearchResult<T>[];

  /**
   * Rank by keyword relevance (BM25) **and** vector similarity, then fuse the
   * two rankings with reciprocal rank fusion.
   *
   * The retrievers fail differently — keyword search misses paraphrases,
   * vector search misses exact identifiers and rare proper nouns — so fusing
   * them recovers both. Requires an FTS index on `textField` and a vector
   * index on `vectorField`. `filter` applies to both retrievers before ranking.
   */
  hybridSearch(
    text: { textField: string; text: string },
    vector: { vectorField: string; vector: QueryVector },
    topK: number,
    filter?: Filter | null,
    options?: HybridSearchOptions,
  ): HybridSearchResult<T>[];

  /**
   * Nearest neighbours by vector similarity.
   *
   * Passing a `filter` always takes the exact path: the graph cannot be
   * traversed under an arbitrary predicate, so matching IDs are scored
   * directly. That is usually what you want for a scoped search — it is exact,
   * and it reads only the vectors that survive the filter.
   */
  findNearest(
    field: string,
    query: QueryVector,
    topK: number,
    filter?: Filter | null,
  ): VectorSearchResult<T>[];

  /** `findNearest` on a background thread. Prefer it for unfiltered searches. */
  findNearestAsync(
    field: string,
    query: QueryVector,
    topK: number,
    filter?: Filter | null,
  ): Promise<VectorSearchResult<T>[]>;

  /**
   * Create a vector index on a numeric-array field. `dimensions` is enforced
   * on every insert and every query.
   */
  createVectorIndex(field: string, dimensions: number, options?: VectorIndexOptions): void;
  dropVectorIndex(field: string): void;

  /**
   * Rebuild this field's in-memory HNSW graph from the persisted vectors.
   *
   * The graph is never written to disk, so it is empty in every new process.
   * Until it is built, `findNearest` degrades silently to an exact linear scan
   * over the whole vector table — correct, but O(n). Call this once per
   * HNSW-indexed field after `initialize()` to get the approximate path back.
   * It scans every vector in the field, so run it off the first frame.
   *
   * A no-op on a flat index.
   */
  upgradeVectorIndex(field: string): void;
}

export interface OpenDBOptions {
  /** Runtime-agnostic outbound change webhook, delivered with global `fetch`. */
  webhook?: WebhookConfig;
}

export interface DB {
  collection<T extends Document = Document>(name: string): Collection<T>;
  webhookStats(): WebhookStats;
  flushWebhook(timeoutMs?: number): Promise<boolean>;
  close(): Promise<void>;

  /** Names of every collection that currently holds documents. */
  listCollectionNames(): string[];

  /**
   * Force a durable sync of everything written so far.
   *
   * Only meaningful when the database was opened with
   * `durability.flush_every_write: false`, which trades an fsync per write for
   * throughput. The default is `true` — every acknowledged write is already
   * durable — so most apps never need this. Batch importers should turn the
   * flag off, import, then call `flush()`.
   */
  flush(): void;

  /** Reclaim space from deleted documents. Expensive; run it off the hot path. */
  compact(): void;

  /**
   * Warm every HNSW vector index in the database.
   *
   * HNSW graphs are held in memory and never persisted — the underlying index
   * is built in one shot and has no incremental insert — so the cache is empty
   * in every new process, and a write to an indexed field drops it again. While
   * a graph is missing, `findNearest` silently takes the exact path and scans
   * the entire vector table: correct, but linear, with nothing in the result to
   * say so.
   *
   * Call this once after `TalaDBModule.initialize()` to get approximate search
   * back. It reads every indexed vector and rebuilds each graph, so keep it off
   * the first frame. Prefer `Collection.upgradeVectorIndex` when you know the
   * one field you need.
   *
   * A no-op when no HNSW index exists.
   */
  rebuildVectorIndexes(): void;

  /** The application-defined schema version stored in the database file. */
  userVersion(): number;
  setUserVersion(version: number): void;
}

/**
 * The JSI host object installed as `global.__TalaDB__`.
 *
 * This mirrors the methods registered in `cpp/TalaDBHostObject.cpp`, which is
 * the real runtime surface. The TurboModule spec in `NativeTalaDB.ts` is
 * deliberately narrower: every method there generates an abstract member that
 * `TalaDBModule.kt` and `TalaDB.mm` must stub out, and those stubs are never
 * called. Add new methods here, not there.
 */
interface JsiTalaDB {
  insert(collection: string, doc: Object): string;
  insertMany(collection: string, docs: Object[]): string[];
  find(collection: string, filter: Object | null): Object[];
  findOne(collection: string, filter: Object | null): Object | null;
  updateOne(collection: string, filter: Object, update: Object): boolean;
  updateMany(collection: string, filter: Object, update: Object): number;
  deleteOne(collection: string, filter: Object): boolean;
  deleteMany(collection: string, filter: Object): number;
  count(collection: string, filter: Object | null): number;
  createIndex(collection: string, field: string): void;
  dropIndex(collection: string, field: string): void;
  createFtsIndex(collection: string, field: string): void;
  dropFtsIndex(collection: string, field: string): void;
  findAsync(collection: string, filter: Object | null): Promise<Object[]>;
  aggregate(collection: string, pipeline: Object[]): Object[];
  searchText(
    collection: string,
    field: string,
    query: string,
    topK: number,
    filter: Object | null,
    options?: Object,
  ): Object[];
  hybridSearch(
    collection: string,
    textField: string,
    text: string,
    vectorField: string,
    vector: QueryVector,
    topK: number,
    filter: Object | null,
    options?: Object,
  ): Object[];
  findNearest(
    collection: string,
    field: string,
    query: QueryVector,
    topK: number,
    filter: Object | null,
  ): Object[];
  findNearestAsync(
    collection: string,
    field: string,
    query: QueryVector,
    topK: number,
    filter: Object | null,
  ): Promise<Object[]>;
  createVectorIndex(
    collection: string,
    field: string,
    dimensions: number,
    opts?: Object,
  ): void;
  dropVectorIndex(collection: string, field: string): void;
  upgradeVectorIndex(collection: string, field: string): void;
  listCollectionNames(): string[];
  rebuildVectorIndexes(): void;
  flush(): void;
  compact(): void;
  userVersion(): number;
  setUserVersion(version: number): void;
}

function native(): JsiTalaDB {
  const host = (globalThis as { __TalaDB__?: JsiTalaDB }).__TalaDB__;
  if (!host) throw new Error('TalaDB is not initialized; await TalaDBModule.initialize() first');
  return host;
}

const EMPTY_STATS: WebhookStats = { pending: 0, delivered: 0, failed: 0, dropped: 0 };

function emitPostImages<T extends Document>(
  webhook: WebhookDispatcher | null,
  collection: string,
  ids: string[],
  op: 'insert' | 'update',
  committedAt: number,
): void {
  if (!webhook?.reports(collection) || ids.length === 0) return;
  const docs = native().find(collection, { _id: { $in: ids } }) as T[];
  const byId = new Map(docs.map((doc) => [doc._id, doc]));
  for (const id of ids) {
    const document = byId.get(id);
    if (document) webhook.emit({ op, collection, id, document, committedAt } as WebhookEvent);
  }
}

function emitDeletes<T extends Document>(
  webhook: WebhookDispatcher | null,
  collection: string,
  docs: T[],
  committedAt: number,
): void {
  if (!webhook?.reports(collection)) return;
  for (const document of docs) {
    if (typeof document._id !== 'string') continue;
    // DELETE carries the pre-image: after commit there is no post-image to read.
    webhook.emit({
      op: 'delete',
      collection,
      id: document._id,
      document,
      committedAt,
    } as WebhookEvent);
  }
}

function collection<T extends Document>(
  colName: string,
  webhook: WebhookDispatcher | null,
): Collection<T> {
  return {
    insert(doc) {
      const id = native().insert(colName, doc as Object);
      const committedAt = Date.now();
      emitPostImages<T>(webhook, colName, [id], 'insert', committedAt);
      return id;
    },
    insertMany(docs) {
      const ids = native().insertMany(colName, docs as Object[]);
      const committedAt = Date.now();
      emitPostImages<T>(webhook, colName, ids, 'insert', committedAt);
      return ids;
    },
    find: (filter) => native().find(colName, filter ?? null) as T[],
    findOne: (filter) => native().findOne(colName, filter) as T | null,
    updateOne(filter, update) {
      const before = native().findOne(colName, filter) as T | null;
      const changed = native().updateOne(colName, filter, update);
      const committedAt = Date.now();
      if (changed && typeof before?._id === 'string') {
        emitPostImages<T>(webhook, colName, [before._id], 'update', committedAt);
      }
      return changed;
    },
    updateMany(filter, update) {
      const before = native().find(colName, filter) as T[];
      const changed = native().updateMany(colName, filter, update);
      const committedAt = Date.now();
      if (changed > 0) {
        emitPostImages<T>(
          webhook,
          colName,
          before.map((doc) => doc._id).filter((id): id is string => typeof id === 'string'),
          'update',
          committedAt,
        );
      }
      return changed;
    },
    deleteOne(filter) {
      const before = native().findOne(colName, filter) as T | null;
      const deleted = native().deleteOne(colName, filter);
      const committedAt = Date.now();
      if (deleted && before) emitDeletes(webhook, colName, [before], committedAt);
      return deleted;
    },
    deleteMany(filter) {
      const before = native().find(colName, filter) as T[];
      const deleted = native().deleteMany(colName, filter);
      const committedAt = Date.now();
      if (deleted > 0) emitDeletes(webhook, colName, before, committedAt);
      return deleted;
    },
    count: (filter) => native().count(colName, filter ?? null),
    createIndex: (field) => native().createIndex(colName, field),
    dropIndex: (field) => native().dropIndex(colName, field),
    createFtsIndex: (field) => native().createFtsIndex(colName, field),
    dropFtsIndex: (field) => native().dropFtsIndex(colName, field),

    findAsync: async (filter) =>
      (await native().findAsync(colName, filter ?? null)) as T[],

    aggregate: <R extends Document = Document>(pipeline: AggregatePipeline) =>
      native().aggregate(colName, pipeline as Object[]) as R[],

    searchText: (field, query, topK, filter, options) =>
      native().searchText(
        colName,
        field,
        query,
        topK,
        filter ?? null,
        options as Object | undefined,
      ) as TextSearchResult<T>[],

    hybridSearch: (text, vector, topK, filter, options) =>
      native().hybridSearch(
        colName,
        text.textField,
        text.text,
        vector.vectorField,
        vector.vector,
        topK,
        filter ?? null,
        options as Object | undefined,
      ) as HybridSearchResult<T>[],

    findNearest: (field, query, topK, filter) =>
      native().findNearest(colName, field, query, topK, filter ?? null) as VectorSearchResult<T>[],

    findNearestAsync: async (field, query, topK, filter) =>
      (await native().findNearestAsync(
        colName,
        field,
        query,
        topK,
        filter ?? null,
      )) as VectorSearchResult<T>[],

    createVectorIndex: (field, dimensions, options) =>
      native().createVectorIndex(colName, field, dimensions, options as Object | undefined),
    dropVectorIndex: (field) => native().dropVectorIndex(colName, field),
    upgradeVectorIndex: (field) => native().upgradeVectorIndex(colName, field),
  };
}

/** Get a synchronous DB handle after `TalaDBModule.initialize(dbName)`. */
export function openDB(_dbName: string, options?: OpenDBOptions): DB {
  const webhook = createWebhookDispatcher(options?.webhook);
  return {
    collection: <T extends Document>(name: string) => collection<T>(name, webhook),
    webhookStats: () => webhook?.stats() ?? { ...EMPTY_STATS },
    flushWebhook: (timeoutMs) => webhook?.flush(timeoutMs) ?? Promise.resolve(true),
    listCollectionNames: () => native().listCollectionNames(),
    flush: () => native().flush(),
    compact: () => native().compact(),
    rebuildVectorIndexes: () => native().rebuildVectorIndexes(),
    userVersion: () => native().userVersion(),
    setUserVersion: (version) => native().setUserVersion(version),
    close: async () => {
      await webhook?.flush();
      await NativeTalaDB.close();
    },
  };
}
