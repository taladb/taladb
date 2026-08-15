// ============================================================
// TalaDB — Shared TypeScript Types
// ============================================================

// --------------- Vector types ---------------

/** Similarity metric used for vector search. */
export type VectorMetric = 'cosine' | 'dot' | 'euclidean';

export interface VectorIndexOptions {
  /** Number of dimensions in each stored vector. Enforced on insert and search. */
  dimensions: number;
  /** Similarity metric. Defaults to `"cosine"`. */
  metric?: VectorMetric;
  /**
   * Index algorithm. Defaults to `"flat"` (exact brute-force).
   * Use `"hnsw"` for approximate nearest-neighbour search — much faster on
   * large collections at the cost of occasional missed results.
   * Requires the `vector-hnsw` feature to be compiled in.
   */
  indexType?: 'flat' | 'hnsw';
  /** HNSW connectivity parameter M (default 16). Higher = better recall, more memory. */
  hnswM?: number;
  /** HNSW build-time quality parameter ef_construction (default 200). */
  hnswEfConstruction?: number;
}

/** Describes the indexes that exist on a collection. */
export interface CollectionIndexInfo {
  /** B-tree indexes (created with `createIndex`). */
  btree: string[];
  /** Full-text search indexes (created with `createFtsIndex`). */
  fts: string[];
  /** Vector indexes (created with `createVectorIndex`). */
  vector: string[];
}

/** A single result returned by `Collection.findNearest`. */
export interface VectorSearchResult<T extends Document = Document> {
  /** The matched document. */
  document: T;
  /**
   * Similarity score — higher means more similar.
   * Range depends on metric: cosine ∈ [-1,1], dot ∈ ℝ, euclidean ∈ (0,1].
   */
  score: number;
}

export interface TextSearchResult<T extends Document = Document> {
  /** The matched document. */
  document: T;
  /**
   * BM25 relevance score — higher means more relevant. Unbounded above, and
   * only meaningful for ordering within a single query's result set.
   */
  score: number;
}

/** Tuning for `searchText`'s BM25 ranking. */
export interface TextSearchOptions {
  /**
   * Term-frequency saturation (BM25 `k1`, default `1.2`). Higher values let a
   * repeated term keep adding relevance for longer.
   */
  k1?: number;
  /**
   * Length normalisation (BM25 `b`, default `0.75`). `0` ignores document
   * length; `1` normalises fully by length relative to the corpus average.
   */
  b?: number;
}

export interface HybridSearchResult<T extends Document = Document> {
  /** The matched document. */
  document: T;
  /**
   * Fused reciprocal-rank-fusion score. Small by construction and meaningful
   * only as an ordering within one result set — never a similarity or a
   * confidence.
   */
  score: number;
  /**
   * Zero-based position in the text ranking, or `null` if the text retriever
   * did not return this document.
   */
  textRank: number | null;
  /**
   * Zero-based position in the vector ranking, or `null` if the vector
   * retriever did not return this document.
   */
  vectorRank: number | null;
}

/** Tuning for `hybridSearch`'s fusion and per-retriever scoring. */
export interface HybridSearchOptions extends TextSearchOptions {
  /**
   * Reciprocal rank fusion smoothing constant (default `60`). Larger values
   * flatten the advantage of the very top ranks.
   */
  rrfK?: number;
  /** Relative weight of the text ranking (default `1`). Set `0` to disable it. */
  textWeight?: number;
  /** Relative weight of the vector ranking (default `1`). Set `0` to disable it. */
  vectorWeight?: number;
  /**
   * How many candidates to pull from each retriever before fusing
   * (default `max(topK * 4, 20)`). Raise it for better recall at more cost;
   * fusing only `topK` from each side drops documents that rank just outside
   * one retriever but high in the other.
   */
  candidates?: number;
}

export type Value =
  | null
  | boolean
  | number
  | string
  | Uint8Array
  | Value[]
  | { [key: string]: Value };

export type Document = { _id?: string; [key: string]: Value | undefined };


// --------------- Filter DSL ---------------

/**
 * The operators available on a single field.
 *
 * Deliberately **not** a conditional type. `T extends null | undefined ? … : …`
 * is *distributive*, so for a union-typed field (`type: 'Cabin' | 'Villa' | …`)
 * it spreads over every member and infers `$in?: 'Cabin'[] | 'Villa'[] | …`
 * instead of `$in?: ('Cabin' | 'Villa' | …)[]` — making `$in` unusable on any
 * union field without a cast.
 *
 * Tuple-wrapping the check (`[T] extends [null | undefined]`) stops the
 * distribution but then strips `$exists` from optional fields, which is the one
 * thing the conditional existed for. So there is no conditional at all: `$exists`
 * is always available (it is meaningful on every field), and the value operators
 * use `NonNullable<T>` so an optional field still compares against its real type.
 */
type FieldOps<T> = {
  $eq?: NonNullable<T>;
  $ne?: NonNullable<T>;
  $gt?: NonNullable<T>;
  $gte?: NonNullable<T>;
  $lt?: NonNullable<T>;
  $lte?: NonNullable<T>;
  $in?: NonNullable<T>[];
  $nin?: NonNullable<T>[];
  $exists?: boolean;
  /** Full-text search: matches documents where this string field contains the given token. */
  $contains?: string;
  $regex?: string;
};

export type Filter<T extends Document = Document> = {
  [K in keyof T]?: T[K] | FieldOps<T[K]>;
} & {
  $and?: Filter<T>[];
  $or?: Filter<T>[];
  $not?: Filter<T>;
};

// --------------- Update DSL ---------------

export type Update<T extends Document = Document> = {
  $set?: Partial<T>;
  $unset?: { [K in keyof T]?: true };
  $inc?: { [K in keyof T]?: number };
  $push?: { [K in keyof T]?: Value };
  $pull?: { [K in keyof T]?: Value };
};

// --------------- Schema validation ---------------

/**
 * A schema validator compatible with Zod, Valibot, and any library that
 * exposes a `parse(data: unknown): T` method.
 *
 * @example with Zod
 * const schema = z.object({ name: z.string(), age: z.number() });
 * const users = db.collection<z.infer<typeof schema>>('users', { schema });
 *
 * @example with Valibot
 * const schema = v.object({ name: v.string(), age: v.number() });
 * const users = db.collection<v.InferOutput<typeof schema>>('users', { schema });
 */
export interface Schema<T> {
  parse(data: unknown): T;
}

/** Primitive field type for a {@link SyncSchema}. `'any'` requires presence
 * without constraining the value's type. */
export type SyncFieldType = 'bool' | 'int' | 'float' | 'str' | 'bytes' | 'array' | 'object' | 'any';

/**
 * Declares this collection's document *shape version*.
 *
 * ::: warning Only `version` is live in 0.11.0
 * This schema was built for the tolerant import path used by `db.sync()`.
 * **Sync was removed in 0.11.0**, and with it the engine-side validator, so
 * `required`, `types`, `renames` and `defaults` are currently inert — nothing
 * reads them. `version` still does real work: it stamps `_v` on locally
 * inserted documents and is the target `migrateDocument` migrates *to*.
 * :::
 *
 * Distinct from {@link Schema} (Zod/Valibot), which is strict and runs on the
 * local `insert` path.
 *
 * The import behaviour these fields described — upgrade below `version`, accept
 * above it, quarantine a bad shape rather than drop it — went with the sync
 * path. `TalaDB.quarantined` no longer exists.
 *
 * @example
 * const users = db.collection<User>('users', {
 *   schema: User,                     // strict, on insert
 *   syncSchema: {                     // tolerant, on import
 *     version: 1,
 *     required: ['name'],
 *     types: { name: 'str', age: 'int' },
 *     defaults: { age: 0 },
 *   },
 * });
 */
export interface SyncSchema {
  /**
   * Current document shape version. Omit or `0` to disable the migration step
   * entirely — in which case {@link renames} and {@link defaults} never run, so
   * declaring either without a `version` is rejected at `db.collection()` rather
   * than silently quarantining the documents they were meant to upgrade.
   */
  version?: number;
  /** Inert since 0.11.0 — see {@link SyncSchema}. */
  required?: string[];
  /** Expected primitive type per field. Fields absent here accept any type. */
  types?: Record<string, SyncFieldType>;
  /** Values applied to missing fields when upgrading a below-`version` document. */
  defaults?: Record<string, Value>;
  /**
   * Field renames applied when upgrading a below-`version` document, as
   * `{ oldName: newName }`. If the old field is present and the new one absent,
   * the value moves. Applied before {@link defaults}. Structural (runs in the
   * engine at import) — for renames that need computation, use
   * {@link CollectionOptions.migrateDocument}.
   */
  renames?: Record<string, string>;
}

/** Options passed to `db.collection()`. */
export interface CollectionOptions<T extends Document = Document> {
  /**
   * Schema validator. When provided, every document passed to `insert` and
   * `insertMany` is run through `schema.parse()` before being stored. If
   * validation fails, a `TalaDbValidationError` is thrown.
   *
   * Compatible with Zod (`z.object({...})`), Valibot (`v.object({...})`), or
   * any object with a `parse(data: unknown): T` method.
   */
  schema?: Schema<T>;
  /**
   * When `true`, documents returned by `find` and `findOne` are also passed
   * through `schema.parse()`. Useful for catching schema drift on old data.
   * Defaults to `false`.
   */
  validateOnRead?: boolean;
  /**
   * Document shape version for this collection. See {@link SyncSchema}.
   *
   * Its `required`/`types`/`renames`/`defaults` fields fed the sync import path
   * and are inert since sync was removed in 0.11.0; `version` remains the
   * migration target and the `_v` stamped on local inserts.
   *
   * Declaring a `version` also makes locally-inserted documents carry that `_v`,
   * so they are never mistaken for legacy documents on read.
   */
  syncSchema?: SyncSchema;
  /**
   * Lazy, read-time document migration — the arbitrary-JS complement to the
   * structural {@link SyncSchema}. When set, every document returned by `find`
   * / `findOne` whose `_v` is **below** `syncSchema.version` is passed through
   * `migrateDocument(doc, fromVersion)` and stamped to the current version
   * before you see it, so application code always reads the current shape even
   * for documents that predate the schema (renames, computed/derived fields,
   * splits/merges). Runs on **every runtime** (it's a pure read transform in
   * the client — no binding support needed).
   *
   * Requires `syncSchema.version` (the migration target). The transform is
   * applied to the returned value only; it is not persisted back to storage —
   * pair with `openDB({ migrations })` or a `syncSchema` rename to rewrite
   * stored documents eagerly. Must be pure and deterministic.
   *
   * @example
   * const users = db.collection<User>('users', {
   *   syncSchema: { version: 2 },
   *   migrateDocument: (doc, from) =>
   *     from < 2 ? { ...doc, fullName: `${doc.first} ${doc.last}` } : doc,
   * });
   */
  migrateDocument?: (doc: T, fromVersion: number) => T;
  /**
   * Lazy, read-time **downcast** — the mirror of {@link migrateDocument}, for a
   * document written by a *newer* peer. When set, every document returned by
   * `find` / `findOne` whose `_v` is **above** `syncSchema.version` is passed
   * through `downgradeDocument(doc, fromVersion)` and projected into the shape
   * this build understands, so application code on an old client sees a shape
   * it can actually read instead of an unexpected future one.
   *
   * Requires `syncSchema.version`. Must be pure and deterministic.
   *
   * **The projection is view-only and is never persisted**, regardless of
   * {@link persistMigrations}. The stored document keeps its original `_v` and
   * its newer fields intact, because this replica must continue to replicate
   * that document faithfully to other peers — an old client is a *reader* of a
   * newer shape, never its editor. For the same reason the returned document
   * keeps its original (higher) `_v`: it is a projection of a v-N document, not
   * a v-M one, and writing it back wholesale would tell the fleet otherwise.
   *
   * @example
   * // This build understands v1. A v2 peer split `name` into first/last.
   * const users = db.collection<User>('users', {
   *   syncSchema: { version: 1 },
   *   downgradeDocument: (doc) => ({ ...doc, name: `${doc.first} ${doc.last}` }),
   * });
   */
  downgradeDocument?: (doc: Readonly<T>, fromVersion: number) => T;
  /**
   * When `true`, a document upgraded by {@link migrateDocument} on read is
   * **written back** to storage (a best-effort `updateOne` computing a `$set`
   * diff) so the migration becomes permanent — after which filters and indexes
   * on the new shape match it. Default `false` (the migrated shape is returned
   * but not persisted).
   *
   * The write-back is **additive-only** except for fields explicitly listed in
   * {@link retiredFields}. A field present in storage but absent from the
   * migrated document is otherwise left alone rather than `$unset`.
   *
   * Trade-offs: reads that encounter un-migrated documents now issue writes
   * (which fire live-query and sync-hook notifications like any other write);
   * a failed write is swallowed and simply retried on the next read. For a
   * one-shot eager rewrite instead, prefer `openDB({ migrations })`.
   */
  persistMigrations?: boolean;
  /**
   * Fields an upcast is explicitly allowed to remove during persist-on-read.
   * Prefer this precise list to {@link allowFieldRemoval}: omissions of any
   * other field remain additive and are preserved.
   */
  retiredFields?: (keyof T & string)[];
  /**
   * How to read a field that {@link migrateDocument} left out of its output:
   * as an intentional removal (`true`), or as a field the migration simply
   * never heard of (`false`, the default).
   *
   * Default `false` — omitted fields are **preserved**: kept on the document
   * returned to your code, and left in storage by the {@link persistMigrations}
   * write-back rather than `$unset`.
   *
   * This default exists because on a synced collection the two cases are
   * indistinguishable from the migration's output, and guessing "removal" is
   * the destructive guess. A migration written today cannot mention a field a
   * *newer* peer will add tomorrow, so an innocent `(doc) => ({ id, name })`
   * becomes a deletion of a field its author never heard of — and under
   * whole-document LWW that deletion replicates to the whole fleet. An old
   * replica has to stay a faithful carrier of shapes it does not understand.
   *
   * Set `true` only on a collection that never syncs, or during a deliberate
   * add → backfill → dual-read → **retire** rollout, where you already know the
   * whole fleet has stopped writing the field.
   */
  /** @deprecated Prefer {@link retiredFields}; this treats every omission as removal. */
  allowFieldRemoval?: boolean;
}

// --------------- Aggregation ---------------

/** A single MongoDB-style aggregation stage. */
export type AggregateStage<T extends Document = Document> =
  | { $match: Filter<T> }
  | {
      /** `_id` is a `"$field"` reference or `null` (single group); other keys are
       * accumulator outputs, e.g. `total: { $sum: '$amount' }`, `n: { $sum: 1 }`. */
      $group: { _id: string | null } & Record<string, unknown>;
    }
  | { $sort: Record<string, 1 | -1> }
  | { $skip: number }
  | { $limit: number }
  /**
   * Reshape each document. Either an **inclusion** (`{ name: 1, city: 1 }` —
   * keep only these) or an **exclusion** (`{ description: 0 }` — keep everything
   * else). The two cannot be mixed and doing so throws; `_id: 0` is the one
   * exclusion allowed alongside an inclusion.
   */
  | { $project: Record<string, 0 | 1> };

/** An ordered aggregation pipeline. */
export type AggregatePipeline<T extends Document = Document> = AggregateStage<T>[];

// --------------- Collection interface ---------------

/**
 * A document on its way in: `_id` is optional, not forbidden.
 *
 * The engine mints a ULID when none is given, and honours one when it is — that
 * is what makes `deriveDocId` hydration idempotent. Typing these parameters as
 * `Omit<T, '_id'>` rejected the very pattern the documentation recommends.
 */
export type InsertDoc<T extends Document> = Omit<T, '_id'> & { _id?: string };

export interface Collection<T extends Document = Document> {
  insert(doc: InsertDoc<T>): Promise<string>;
  insertMany(docs: InsertDoc<T>[]): Promise<string[]>;
  find(filter?: Filter<T>): Promise<T[]>;
  findOne(filter: Filter<T>): Promise<T | null>;
  updateOne(filter: Filter<T>, update: Update<T>): Promise<boolean>;
  updateMany(filter: Filter<T>, update: Update<T>): Promise<number>;
  deleteOne(filter: Filter<T>): Promise<boolean>;
  deleteMany(filter: Filter<T>): Promise<number>;
  count(filter?: Filter<T>): Promise<number>;
  /**
   * Run a MongoDB-style aggregation pipeline (`$match`, `$group`, `$sort`,
   * `$skip`, `$limit`, `$project`) inside the engine. Returns the resulting
   * documents. Available on every runtime: Node, the OPFS worker, the in-memory
   * browser build, and React Native.
   *
   * This is also how you page a collection locally — `find()` has no sort/skip/
   * limit — but note it returns a **snapshot**. For a paged read that stays live
   * as rows land, use {@link subscribeAggregate}.
   *
   * @example
   * const byStatus = await orders.aggregate([
   *   { $group: { _id: '$status', total: { $sum: '$amount' }, n: { $sum: 1 } } },
   *   { $sort: { total: -1 } },
   * ]);
   */
  aggregate<R extends Document = Document>(pipeline: AggregatePipeline<T>): Promise<R[]>;
  createIndex(field: keyof Omit<T, '_id'> & string): Promise<void>;
  dropIndex(field: keyof Omit<T, '_id'> & string): Promise<void>;
  /**
   * Create a compound (multi-field) index over an ordered list of fields.
   *
   * The query planner uses it to accelerate an `$and` where **every** field of
   * the index is constrained by equality — e.g. an index on
   * `['userId', 'status']` serves `find({ userId, status })` with a single
   * index scan instead of a full-collection scan. Fields are ascending; a
   * partial-prefix or trailing-range match is not used yet (planned).
   *
   * @example
   * await orders.createCompoundIndex(['userId', 'status'])
   */
  createCompoundIndex(fields: (keyof Omit<T, '_id'> & string)[]): Promise<void>;
  /** Drop a compound index by its ordered field list. */
  dropCompoundIndex(fields: (keyof Omit<T, '_id'> & string)[]): Promise<void>;
  /**
   * Create a full-text search index on a string field.
   *
   * Enables fast `{ field: { $contains: 'token' } }` queries using an
   * inverted token index instead of a full collection scan.
   *
   * @example
   * await notes.createFtsIndex('body');
   */
  createFtsIndex(field: keyof Omit<T, '_id'> & string): Promise<void>;
  /** Drop a full-text search index. */
  dropFtsIndex(field: keyof Omit<T, '_id'> & string): Promise<void>;
  /**
   * Rank documents against a free-text `query` using BM25, most relevant first.
   *
   * Unlike the `$contains` filter, which requires **every** token to be
   * present, this uses OR semantics — a document that matches more of the
   * query simply scores higher. Requires an FTS index on `field`.
   *
   * @example
   * const hits = await articles.searchText('body', 'reset my password', 5);
   * // hits: Array<{ document: Article, score: number }>
   */
  searchText(
    field: keyof Omit<T, '_id'> & string,
    query: string,
    topK: number,
    filter?: Filter<T>,
    options?: TextSearchOptions,
  ): Promise<TextSearchResult<T>[]>;
  /**
   * Hybrid retrieval: rank by keyword relevance (BM25) **and** vector
   * similarity, then fuse the two rankings with reciprocal rank fusion.
   *
   * The two retrievers fail differently — keyword search misses paraphrases,
   * vector search misses exact identifiers and rare proper nouns — so fusing
   * them recovers both. A document both retrievers rank well outranks one that
   * only a single retriever found. Requires an FTS index on `textField` and a
   * vector index on `vectorField`.
   *
   * The optional `filter` is applied to both retrievers before ranking.
   *
   * @example
   * const hits = await articles.hybridSearch(
   *   { textField: 'body', text: 'reset my password' },
   *   { vectorField: 'embedding', vector: queryVec },
   *   5,
   * );
   */
  hybridSearch(
    text: { textField: keyof Omit<T, '_id'> & string; text: string },
    vector: { vectorField: keyof Omit<T, '_id'> & string; vector: number[] },
    topK: number,
    filter?: Filter<T>,
    options?: HybridSearchOptions,
  ): Promise<HybridSearchResult<T>[]>;
  /**
   * Return the indexes that currently exist on this collection.
   *
   * @example
   * const { btree, fts, vector } = await notes.listIndexes();
   */
  listIndexes(): Promise<CollectionIndexInfo>;
  /**
   * Create a vector index on a numeric-array field.
   *
   * After creation, `findNearest` can search this field and new inserts/updates
   * automatically maintain the index. Existing documents are backfilled.
   *
   * @example
   * await articles.createVectorIndex('embedding', { dimensions: 384 });
   */
  createVectorIndex(
    field: keyof Omit<T, '_id'> & string,
    options: VectorIndexOptions,
  ): Promise<void>;
  /** Drop a vector index. */
  dropVectorIndex(field: keyof Omit<T, '_id'> & string): Promise<void>;
  /**
   * Upgrade a flat vector index to HNSW in-place.
   *
   * After calling this, `findNearest` uses approximate nearest-neighbour
   * search which is significantly faster on large collections.
   * Requires the `vector-hnsw` feature to be compiled in; no-op otherwise.
   */
  upgradeVectorIndex(field: keyof Omit<T, '_id'> & string): Promise<void>;
  /**
   * Find the `topK` most similar documents to `vector` using a vector index.
   *
   * Optionally combine with a metadata `filter` to restrict the search space
   * before ranking — e.g. find the 5 most similar english-language articles.
   *
   * Results are ordered by descending similarity score (highest first).
   *
   * @example
   * const results = await articles.findNearest('embedding', queryVec, 5);
   * // results: Array<{ document: Article, score: number }>
   *
   * @example with pre-filter
   * const results = await articles.findNearest('embedding', queryVec, 5, {
   *   locale: 'en',
   * });
   */
  findNearest(
    field: keyof Omit<T, '_id'> & string,
    vector: number[],
    topK: number,
    filter?: Filter<T>,
  ): Promise<VectorSearchResult<T>[]>;
  /**
   * Subscribe to live query results. The callback receives a full snapshot of
   * matching documents immediately and again after every write that could
   * affect the result set.
   *
   * @returns An unsubscribe function. Call it to stop receiving updates.
   *
   * @example
   * const unsub = users.subscribe({ active: true }, (docs) => {
   *   console.log('active users:', docs);
   * });
   * // later…
   * unsub();
   */
  subscribe(
    filter: Filter<T>,
    callback: (docs: T[]) => void,
    onError?: (error: unknown) => void,
  ): () => void;
  /**
   * Subscribe to a live **aggregation** — the same as {@link subscribe}, but the
   * result set is produced by a pipeline rather than a filter.
   *
   * This exists because {@link aggregate} is the only way to sort/skip/limit, and
   * on its own it returns a dead snapshot: a paged read built on it would never
   * re-run when new rows land, so a page would sit frozen while a background
   * hydration filled the collection underneath it. Anything that pages locally
   * should subscribe here instead of calling `aggregate` in an effect.
   *
   * The callback receives a snapshot immediately and again after every write that
   * could affect the result.
   *
   * @returns An unsubscribe function.
   *
   * @example
   * const unsub = products.subscribeAggregate(
   *   [{ $match: { category: 'kitchen' } }, { $sort: { price: 1 } }, { $limit: 20 }],
   *   (page) => render(page),
   * );
   */
  subscribeAggregate<R extends Document = Document>(
    pipeline: AggregatePipeline<T>,
    callback: (docs: R[]) => void,
    onError?: (error: unknown) => void,
  ): () => void;
}

// --------------- Sync ---------------

import type { WebhookStats } from './webhook';

// --------------- TalaDB interface ---------------

export interface TalaDB {
  collection<T extends Document = Document>(name: string, options?: CollectionOptions<T>): Collection<T>;
  /**
   * Compact the underlying storage file, reclaiming space freed by deletes
   * and updates.
   *
   * Call during idle periods — e.g. once on startup. No-op on in-memory
   * (IndexedDB-fallback) databases.
   *
   * @example
   * await db.compact();
   */
  compact(): Promise<void>;
  /**
   * Force any batched (eventual-durability) writes to durable storage, and on
   * the browser also write the IndexedDB fallback snapshot immediately. A
   * no-op under the default `flush_every_write: true` durability. Use for
   * "save now" moments (before checkout, on `visibilitychange`).
   */
  flush?(): Promise<void>;
  /**
   * Whether this tab's writes land authoritatively, without being forwarded to
   * another tab.
   *
   * **Browser only.** The first tab to open a database becomes the primary and
   * owns the storage; later tabs run an in-memory copy and forward their writes
   * to it over BroadcastChannel. Both the OPFS owner and — where OPFS is
   * unavailable — the tab that publishes the IndexedDB snapshot report `true`.
   *
   * Use it for work that must not run in more than one tab at a time, or that
   * depends on reading its own writes back immediately: a background queue
   * drainer, a scheduled cleanup pass, an outbound sync loop. A secondary tab
   * sees other tabs' writes up to ~500 ms late, and its own writes only once
   * the primary has applied them.
   *
   * Primary status changes during a session — closing the owning tab promotes
   * another — so re-check it rather than caching the answer.
   *
   * Always `true` on Node.js and React Native, where a single process owns the
   * database. May be absent on older `@taladb/web` builds — treat absence as
   * `true`.
   *
   * @example
   * if (await db.isPrimary?.() ?? true) {
   *   await drainOutbox();
   * }
   */
  isPrimary?(): Promise<boolean>;
  /**
   * Change-webhook delivery counters, when the webhook is enabled. All zero
   * (and `pending: 0`) when it is not.
   */
  webhookStats?(): WebhookStats;
  /**
   * Wait for queued change-webhook events to drain. Resolves `true` when the
   * queue emptied, `false` on timeout. Resolves `true` immediately when the
   * webhook is disabled.
   */
  flushWebhook?(timeoutMs?: number): Promise<boolean>;
  close(): Promise<void>;
}
