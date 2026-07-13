/** Similarity metric used for vector search. */
type VectorMetric = 'cosine' | 'dot' | 'euclidean';
interface VectorIndexOptions {
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
interface CollectionIndexInfo {
    /** B-tree indexes (created with `createIndex`). */
    btree: string[];
    /** Full-text search indexes (created with `createFtsIndex`). */
    fts: string[];
    /** Vector indexes (created with `createVectorIndex`). */
    vector: string[];
}
/** A single result returned by `Collection.findNearest`. */
interface VectorSearchResult<T extends Document = Document> {
    /** The matched document. */
    document: T;
    /**
     * Similarity score — higher means more similar.
     * Range depends on metric: cosine ∈ [-1,1], dot ∈ ℝ, euclidean ∈ (0,1].
     */
    score: number;
}
type Value = null | boolean | number | string | Uint8Array | Value[] | {
    [key: string]: Value;
};
type Document = {
    _id?: string;
    [key: string]: Value | undefined;
};
/**
 * Who authored a write, and therefore whether it replicates outward.
 *
 * - `'local'` *(default)* — an ordinary user write. Replicates to peers as usual.
 * - `'remote'` — a row replicated **in** from an authoritative origin. The origin
 *   already has it, so it must never go back out: rows written this way fire no
 *   sync events and never appear in `exportChanges()`, and deletes made this way
 *   leave no tombstone. Enforced in the engine, not by convention.
 */
type WriteOrigin = 'local' | 'remote';
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
type Filter<T extends Document = Document> = {
    [K in keyof T]?: T[K] | FieldOps<T[K]>;
} & {
    $and?: Filter<T>[];
    $or?: Filter<T>[];
    $not?: Filter<T>;
};
type Update<T extends Document = Document> = {
    $set?: Partial<T>;
    $unset?: {
        [K in keyof T]?: true;
    };
    $inc?: {
        [K in keyof T]?: number;
    };
    $push?: {
        [K in keyof T]?: Value;
    };
    $pull?: {
        [K in keyof T]?: Value;
    };
};
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
interface Schema<T> {
    parse(data: unknown): T;
}
/** Primitive field type for a {@link SyncSchema}. `'any'` requires presence
 * without constraining the value's type. */
type SyncFieldType = 'bool' | 'int' | 'float' | 'str' | 'bytes' | 'array' | 'object' | 'any';
/**
 * A tolerant, structural schema applied to documents **arriving via sync**
 * (`db.sync()` pull). Distinct from {@link Schema} (Zod/Valibot), which is
 * strict and runs on the *local* `insert` path: sync import is the boundary you
 * don't control, so it validates structurally and never hard-rejects.
 *
 * On import, per document:
 * - `_v` **below** `version` → upgraded in place (missing `defaults` filled,
 *   `_v` stamped) — additive-only migration.
 * - `_v` **above** `version` → accepted untouched (the peer is ahead).
 * - a missing/`null` `required` field or a `types` mismatch → **quarantined**
 *   (set aside, recoverable via {@link TalaDB.quarantined}), never dropped and
 *   never aborting the batch.
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
interface SyncSchema {
    /**
     * Current document shape version. Omit or `0` to disable the migration step
     * entirely — in which case {@link renames} and {@link defaults} never run, so
     * declaring either without a `version` is rejected at `db.collection()` rather
     * than silently quarantining the documents they were meant to upgrade.
     */
    version?: number;
    /** Fields that must be present and non-null, or the document is quarantined. */
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
interface CollectionOptions<T extends Document = Document> {
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
     * Tolerant structural schema applied to documents arriving via `db.sync()`.
     * See {@link SyncSchema}. Enables validate-on-import ("validate, never cast")
     * in the core sync path, with `_v` migration and quarantine of bad shapes.
     * Wired on browser (OPFS worker), Node.js, and React Native; a binding whose
     * native module predates 0.9.2 falls back to unvalidated import.
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
/** A single MongoDB-style aggregation stage. */
type AggregateStage<T extends Document = Document> = {
    $match: Filter<T>;
} | {
    /** `_id` is a `"$field"` reference or `null` (single group); other keys are
     * accumulator outputs, e.g. `total: { $sum: '$amount' }`, `n: { $sum: 1 }`. */
    $group: {
        _id: string | null;
    } & Record<string, unknown>;
} | {
    $sort: Record<string, 1 | -1>;
} | {
    $skip: number;
} | {
    $limit: number;
}
/**
 * Reshape each document. Either an **inclusion** (`{ name: 1, city: 1 }` —
 * keep only these) or an **exclusion** (`{ description: 0 }` — keep everything
 * else). The two cannot be mixed and doing so throws; `_id: 0` is the one
 * exclusion allowed alongside an inclusion.
 */
 | {
    $project: Record<string, 0 | 1>;
};
/** An ordered aggregation pipeline. */
type AggregatePipeline<T extends Document = Document> = AggregateStage<T>[];
interface Collection<T extends Document = Document> {
    insert(doc: Omit<T, '_id'>): Promise<string>;
    insertMany(docs: Omit<T, '_id'>[]): Promise<string[]>;
    /**
     * Upsert many documents **by `_id`**, in a single commit. Existing rows are
     * replaced in place, absent rows are created, and rows not named in `docs` are
     * left alone — so writing page 2 never disturbs page 1.
     *
     * Unlike {@link insertMany}, which discards `_id` and mints a fresh ULID, this
     * *honours* the id you supply. That is the whole point: for a row replicated
     * from a remote origin, pass `_id: deriveDocId(collection, remoteKey)` and every
     * later fetch of that row converges on the same document instead of duplicating
     * it. Idempotent, and safe to run concurrently from a background hydration walk
     * and an on-demand fetch.
     *
     * `origin: 'remote'` marks the rows as replicated in from an authoritative
     * origin, which means they are **never replicated back out** — they will not
     * fire sync events and will not appear in `exportChanges()`. Use it for anything
     * the origin already knows about. Defaults to `'local'`.
     *
     * @example
     * await products.replaceManyWithIds(
     *   rows.map((r) => ({ ...r, _id: deriveDocId('products', r.id) })),
     *   'remote',
     * );
     */
    replaceManyWithIds(docs: T[], origin?: WriteOrigin): Promise<string[]>;
    /**
     * Delete many documents by `_id`, in a single commit. Returns how many were
     * present and removed; unknown ids are skipped.
     *
     * `origin: 'remote'` deletes **without a tombstone**, so the deletion is not
     * replicated outward — correct when the origin is the one that told you the row
     * was deleted. Defaults to `'local'`, which tombstones as usual.
     */
    deleteManyWithIds(ids: string[], origin?: WriteOrigin): Promise<number>;
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
    createVectorIndex(field: keyof Omit<T, '_id'> & string, options: VectorIndexOptions): Promise<void>;
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
    findNearest(field: keyof Omit<T, '_id'> & string, vector: number[], topK: number, filter?: Filter<T>): Promise<VectorSearchResult<T>[]>;
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
    subscribe(filter: Filter<T>, callback: (docs: T[]) => void, onError?: (error: unknown) => void): () => void;
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
    subscribeAggregate<R extends Document = Document>(pipeline: AggregatePipeline<T>, callback: (docs: R[]) => void, onError?: (error: unknown) => void): () => void;
}
/**
 * A JSON-encoded changeset — the opaque payload exchanged between peers. Produced
 * by {@link TalaDB.exportChanges}, transported by a {@link SyncAdapter}, and
 * consumed by {@link TalaDB.importChanges}. Treat it as an opaque string.
 */
type SerializedChangeset = string;
/** Direction of a sync pass. `'both'` (default) is fully bidirectional. */
type SyncDirection = 'push' | 'pull' | 'both';
/**
 * A transport for {@link TalaDB.sync}. Implement `push` to send local changes to
 * a remote, `pull` to fetch remote changes — or both for bidirectional sync.
 * The changeset is an opaque JSON string; move it over any wire you like.
 */
interface SyncAdapter {
    /** Send a local changeset to the remote. Required for `'push'` / `'both'`. */
    push?(changeset: SerializedChangeset): Promise<void>;
    /**
     * Fetch remote changes with `changed_at` after `sinceMs` (ms epoch), as a
     * serialized changeset. Return `'[]'` when there is nothing new. Required for
     * `'pull'` / `'both'`.
     *
     * @deprecated in spirit, not in support — wall-clock timestamps are not safe
     * cursors (see {@link CursorSyncAdapter}), which is why every pass built on this
     * method replays the whole collection from zero. Implement
     * {@link CursorSyncAdapter.pullWithCursor} instead when your origin can issue a
     * cursor. Adapters that only implement `pull` keep working unchanged.
     */
    pull?(sinceMs: number): Promise<SerializedChangeset>;
}
/** One page of remote changes, plus where to resume from. */
interface PullResult {
    /** The changes themselves. `'[]'` when there is nothing new. */
    changeset: SerializedChangeset;
    /**
     * Opaque resume token, issued by the origin. **Never parse this.** It may be a
     * timestamp, a sequence number, an LSN, a snapshot id — that is the origin's
     * business, and treating it as a number is how clients reintroduce the
     * clock-skew bug this type exists to kill.
     */
    cursor: string;
    /** `true` when more pages remain; call again with the returned `cursor`. */
    hasMore: boolean;
}
/**
 * A {@link SyncAdapter} whose origin can issue a resume cursor.
 *
 * ## Why this exists
 *
 * The original contract is `pull(sinceMs)`, and it cannot be made correct. Author
 * wall-clock timestamps are not safe cursors: a write can commit *after* an export
 * yet carry an *earlier* timestamp, so resuming from "the newest timestamp I saw"
 * silently drops rows. TalaDB's answer was to give up on cursors entirely and
 * replay from zero on every pass — correct, but it re-downloads the whole
 * collection forever, which makes a full local replica of a real catalog
 * unaffordable.
 *
 * The fix is to stop inventing the cursor on the client. The origin issues an
 * opaque token; we store it and hand it back. Whatever ordering guarantee the
 * origin has (a sequence, an LSN, a snapshot) travels with the token, and the
 * client never has to reason about clocks at all.
 *
 * `runSync` feature-detects `pullWithCursor` and prefers it. Adapters that only
 * implement `pull(sinceMs)` are untouched and keep their replay-from-zero
 * behavior.
 */
interface CursorSyncAdapter extends SyncAdapter {
    /**
     * Fetch changes after `cursor`, or from the beginning when it is `null`.
     * Returns the changes plus the token to resume from next time.
     */
    pullWithCursor(cursor: string | null): Promise<PullResult>;
}
interface SyncOptions {
    /**
     * Collections to sync. Omit to sync **all** user collections (reserved
     * `_`-prefixed collections are always skipped). Provide an array to sync only
     * those.
     */
    collections?: string[];
    /**
     * Collections to skip. Applied after `collections` (or after the
     * all-collections default), so `{ exclude: ['logs'] }` means "sync everything
     * except logs".
     */
    exclude?: string[];
    /** Direction of the pass. Default `'both'` (bidirectional). */
    direction?: SyncDirection;
    /**
     * Names this sync target. Reserved cursor state remains isolated per target
     * for forward compatibility with monotonic server cursors. Default
     * `'default'`.
     */
    target?: string;
}
interface SyncResult {
    /** Number of local changes pushed to the remote. */
    pushed: number;
    /** Number of documents changed locally by the pulled remote changeset. */
    pulled: number;
    /**
     * Documents in the pulled changeset skipped by an import validator (a
     * collection this client does not model). Always `0` when no `syncSchema`
     * applied to the pass.
     */
    skipped?: number;
    /**
     * Documents in the pulled changeset set aside by an import validator because
     * they failed structural validation. Recoverable via {@link TalaDB.quarantined}.
     * Always `0` when no `syncSchema` applied to the pass.
     */
    quarantined?: number;
    /** Active sync cursor. Currently `0` because timestamp adapters replay safely. */
    cursor: number;
}
/** A document set aside during a validated sync import, with its rejection reason. */
interface QuarantinedDocument<T extends Document = Document> {
    /** The rejected document, retained verbatim. */
    document: T;
    /** Human-readable reason the document was quarantined. */
    reason: string;
    /** The `changed_at` (ms epoch) the rejected change carried. */
    changedAt: number;
}
interface TalaDB {
    collection<T extends Document = Document>(name: string, options?: CollectionOptions<T>): Collection<T>;
    /**
     * Run one bidirectional sync pass against `adapter`: pull remote changes and
     * merge them (Last-Write-Wins), then push local changes since the last cursor.
     * The cursor is persisted per `target`, so successive calls sync incrementally.
     * Set `direction` to `'push'` or `'pull'` to make it one-way.
     *
     * @example
     * await db.sync(httpAdapter, { collections: ['notes'] });          // bidirectional
     * await db.sync(httpAdapter, { collections: ['logs'], direction: 'push' });
     */
    sync(adapter: SyncAdapter, options: SyncOptions): Promise<SyncResult>;
    /**
     * Low-level: export changes to `collections` with `changed_at` after `sinceMs`
     * (exclusive) as a serialized changeset. Most apps use {@link TalaDB.sync}.
     */
    exportChanges(collections: string[], sinceMs: number): Promise<SerializedChangeset>;
    /**
     * Low-level: merge a serialized changeset into the local database via
     * Last-Write-Wins. Returns the number of documents changed. Idempotent —
     * re-importing the same changeset is a no-op.
     */
    importChanges(changeset: SerializedChangeset): Promise<number>;
    /**
     * Compact the underlying storage file, reclaiming space freed by deletes
     * and updates.
     *
     * Call during idle periods — e.g. once on startup after `compactTombstones`.
     * No-op on in-memory (IndexedDB-fallback) databases.
     *
     * @example
     * await db.compact();
     */
    compact(): Promise<void>;
    /**
     * Return the documents set aside in `collection`'s quarantine table by a
     * validated sync import (see {@link SyncSchema}). Empty when nothing was
     * quarantined. Wired on browser and Node.js; resolves to `[]` on runtimes
     * without support.
     */
    quarantined?<T extends Document = Document>(collection: string): Promise<QuarantinedDocument<T>[]>;
    /**
     * Force any batched (eventual-durability) writes to durable storage, and on
     * the browser also write the IndexedDB fallback snapshot immediately. A
     * no-op under the default `flush_every_write: true` durability. Use for
     * "save now" moments (before checkout, on `visibilitychange`).
     */
    flush?(): Promise<void>;
    /** Browser HTTP-push queue health, when supported by the active binding. */
    syncStatus?(): Promise<{
        pending: number;
        dropped: number;
        failed: number;
    }>;
    /** Wait for accepted browser HTTP-push events, returning false on timeout. */
    flushSync?(timeoutMs?: number): Promise<boolean>;
    close(): Promise<void>;
}

/** HTTP push sync settings. */
interface SyncConfig {
    /**
     * Enable HTTP push sync. Defaults to `false`.
     * Everything is a no-op when disabled, so a config block without
     * `enabled: true` is safe to ship.
     */
    enabled?: boolean;
    /**
     * Default endpoint URL that receives all mutation events.
     * Required when `enabled: true`.
     */
    endpoint?: string;
    /** HTTP headers sent with every outgoing request (e.g. `Authorization`). */
    headers?: Record<string, string>;
    /** Override the endpoint for `insert` events only. */
    insert_endpoint?: string;
    /** Override the endpoint for `update` events only. */
    update_endpoint?: string;
    /** Override the endpoint for `delete` events only. */
    delete_endpoint?: string;
    /**
     * Document fields to omit from every outgoing sync payload.
     *
     * Useful for stripping large computed fields such as embedding vectors
     * that the remote endpoint doesn't need.
     *
     * @example
     * exclude_fields: ['embedding', 'clip_vector']
     */
    exclude_fields?: string[];
}
/** Storage durability settings. */
interface DurabilityConfig {
    /**
     * When `true` (default), every write commit is fsync'd immediately — a crash
     * never loses an acknowledged write. When `false`, commits are batched for
     * higher write throughput; call `db.flush()` to force a durable sync. Applies
     * to Node (file) and browser OPFS storage; in-memory ignores it.
     */
    flush_every_write?: boolean;
    /**
     * Browser IndexedDB-fallback snapshot debounce, in milliseconds (default
     * 500). Only affects the non-OPFS browser fallback path — the OPFS and Node
     * paths use `flush_every_write`.
     */
    flush_ms?: number;
}
/** Top-level TalaDB configuration. */
interface TalaDbConfig {
    /** HTTP push sync configuration. Disabled by default. */
    sync?: SyncConfig;
    /** Storage durability configuration. */
    durability?: DurabilityConfig;
}

interface HttpSyncAdapterOptions {
    /** Base URL, e.g. `https://api.example.com/sync`. `/push` and `/pull` are appended. */
    endpoint: string;
    /** Extra headers on every request — typically `Authorization`. */
    headers?: Record<string, string>;
    /**
     * `fetch` implementation. Defaults to the global `fetch` (Node 18+, browsers,
     * React Native). Inject a custom one for tests or non-standard environments.
     */
    fetch?: typeof fetch;
    /** Paths appended to `endpoint`. Override to match an existing API. */
    paths?: {
        push?: string;
        pull?: string;
    };
}
/**
 * A ready-to-use {@link SyncAdapter} that syncs over plain HTTP. Pair it with
 * {@link TalaDB.sync}:
 *
 * ```ts
 * const adapter = new HttpSyncAdapter({
 *   endpoint: 'https://api.example.com/sync',
 *   headers: { Authorization: `Bearer ${token}` },
 * });
 * await db.sync(adapter, { collections: ['notes'] });
 * ```
 */
declare class HttpSyncAdapter implements SyncAdapter {
    private readonly endpoint;
    private readonly headers;
    private readonly fetchFn;
    private readonly pushPath;
    private readonly pullPath;
    constructor(options: HttpSyncAdapterOptions);
    push(changeset: SerializedChangeset): Promise<void>;
    pull(sinceMs: number): Promise<SerializedChangeset>;
}

/**
 * Deterministic document ids for replicated rows.
 *
 * The engine assigns ULIDs and **ignores a caller-supplied `_id`** — it silently
 * becomes an ordinary field, so `find({ _id: 'sku-1' })` then matches nothing.
 * That leaves a document replicated from a remote origin with no stable local
 * identity to merge on: re-fetching the same row would insert a duplicate.
 *
 * Hashing the origin's primary key into the ULID gives that identity back. The
 * same `(collection, key)` always maps to the same document, which is what makes
 * replication upserts **idempotent** (re-applying a page is a no-op), **resumable**
 * (a bootstrap walk can restart mid-way), and **safe to run concurrently** (an
 * on-demand fetch and the background walk can touch the same row and converge on
 * one document rather than two).
 *
 * ## This must stay byte-identical to the Rust `derive_doc_id`
 *
 * The same rows are addressed from both sides. If the two implementations ever
 * disagree, two clients assign different `_id`s to the same remote row and the
 * replica silently forks into duplicates — with no error anywhere. The shared
 * test vectors in `derive-id.test.ts` and `packages/core/src/document.rs` exist to
 * make that impossible to do by accident; keep them in lockstep.
 *
 * FNV-1a is used over a stronger hash precisely *because* it is short enough to
 * port between the two languages without ambiguity. It is non-cryptographic, which
 * is fine here: the input is a primary key from an origin the client already
 * trusts, not adversarial input.
 */
/**
 * Derive a stable `_id` for a row replicated from a remote origin.
 *
 * `collection` is part of the preimage, so the same remote id in two different
 * collections cannot collide.
 *
 * @example
 * deriveDocId('products', 'sku-123')  // → '56GC678DQYWW1Z98HPYJ90WVKH', always
 *
 * ## Ordering caveat
 *
 * The result is a hash, so its ULID timestamp prefix is **not** chronological.
 * Documents written with a derived id do not come back in insertion order from an
 * unsorted `find()`; reads over replicated collections must carry an explicit
 * sort. Documents written via `insert`/`insertMany` are unaffected — they still
 * get monotonic ULIDs.
 */
declare function deriveDocId(collection: string, key: string): string;

/**
 * Coverage — "is this collection complete enough, locally, to answer a query
 * without the network?"
 *
 * This is the question the whole coverage-first design turns on, and it is *not*
 * "have I fetched this page?". A replica assembled from whichever pages a user
 * happened to visit is an arbitrary partial subset: it cannot answer a query
 * nobody has asked yet ("products under ₱500" may live on page 43), so every new
 * filter or sort still goes to the network and the local database buys you almost
 * nothing. Coverage is what licenses a purely local read.
 *
 * Two things make it trustworthy:
 *
 * 1. **It is scoped, not per-collection.** `complete` for a bare collection name
 *    would leak across users: log in as someone else and you inherit the previous
 *    user's "complete" flag *and* their rows. The key is a tuple.
 * 2. **It is a state machine, not a boolean.** Only `complete` authorizes a
 *    local-only read. `best-effort` exists precisely so an origin that *cannot*
 *    give us a consistent snapshot degrades honestly instead of claiming a
 *    completeness it never established.
 */

/** Reserved collection holding one coverage document per replicated scope. */
declare const COVERAGE_COLLECTION = "__taladb_replica";
/**
 * What identifies a replicated scope. Every component must be part of the key,
 * because each one changes what "complete" means:
 *
 * - `origin` — two origins are two different datasets.
 * - `collection` — the local collection being filled.
 * - `scope` — the *authorization* slice (a user, a tenant, a store). This is the
 *   one that bites: without it, user B logging in inherits user A's completeness.
 * - `projectionVersion` — a replica hydrated with a slimmer projection is not
 *   complete for a query that needs the dropped fields.
 * - `schemaVersion` — rows hydrated under an older shape may not satisfy today's.
 */
interface CoverageKey {
    origin: string;
    collection: string;
    scope: string;
    projectionVersion: number;
    schemaVersion: number;
}
type CoverageState = 
/** Nothing local. */
{
    status: 'empty';
}
/**
 * A bootstrap walk is in progress. `snapshot` pins every page to one logical
 * view of the origin; `nextPage` is the durable resume point.
 */
 | {
    status: 'hydrating';
    snapshot: string;
    nextPage: string | number;
    rowsApplied: number;
    deltaCursor?: string;
    total?: number;
}
/**
 * The scope is fully local as of `cursor`. **The only state that permits a
 * local-only read.**
 */
 | {
    status: 'complete';
    cursor: string;
    completedAt: number;
    rowsApplied: number;
    total?: number;
}
/**
 * Every row the origin offered was applied, but the origin could not pin a
 * snapshot, so we cannot *prove* we saw a consistent view — a row that shifted
 * between pages mid-walk may have been missed. Reads must not treat this as
 * authoritative.
 */
 | {
    status: 'best-effort';
    cursor: string;
    reason: string;
    rowsApplied: number;
    total?: number;
}
/** Complete once, but known to have fallen behind (e.g. a projection change). */
 | {
    status: 'stale';
    cursor: string;
    reason: string;
}
/** The walk failed. `resumeFrom` is where to pick it up. */
 | {
    status: 'error';
    resumeFrom: string | number;
    snapshot?: string;
    deltaCursor?: string;
    rowsApplied?: number;
    total?: number;
    error: string;
};
/**
 * Serialize a {@link CoverageKey} into a stable string.
 *
 * Field order is fixed rather than derived from `Object.keys`, so the key cannot
 * change meaning if someone reorders the interface — a silent coverage reset,
 * which would look like "the app re-downloads everything for no reason".
 */
declare function coverageKey(key: CoverageKey): string;
/**
 * Persistent coverage state, one document per scope.
 *
 * The state is stored as a JSON string rather than as structured fields: it is a
 * discriminated union whose shape varies per variant, and TalaDB documents are
 * flat. Writing it whole also makes each transition a single atomic write, which
 * is what lets `markComplete` be the durable commit point of a bootstrap.
 */
declare class CoverageStore {
    private readonly col;
    constructor(db: TalaDB);
    read(key: CoverageKey): Promise<CoverageState>;
    write(key: CoverageKey, state: CoverageState): Promise<void>;
    /** Drop a scope's coverage, forcing a fresh bootstrap on next use. */
    clear(key: CoverageKey): Promise<void>;
}
/**
 * Whether a local-only read is authorized for this state.
 *
 * Deliberately strict: **only `complete`**. `best-effort` is the interesting
 * exclusion — it means we applied everything the origin gave us, but the origin
 * could not pin a snapshot, so a row that moved between pages during the walk may
 * never have been seen. Serving that as authoritative would silently return
 * incomplete results, which is worse than going to the network.
 */
declare function isAuthoritative(state: CoverageState): boolean;
/** Rows applied so far, for progress reporting. */
declare function rowsApplied(state: CoverageState): number;
/** Fractional hydration progress, when the origin told us the total. */
declare function progress(state: CoverageState): number | undefined;

/**
 * The replication *source* — wire translation, and nothing else.
 *
 * A source knows how to talk to one origin: how to ask for a page, how to ask for
 * changes since a cursor, how to find a row's primary key, and how to shape a row
 * into a document. It owns **no orchestration**: no batching, no yielding, no
 * cursor persistence, no coverage transitions, no retry, no dedup. All of that
 * belongs to the coordinator, which is generic over sources.
 *
 * That split is deliberate. The obvious alternative — make the REST origin a
 * `SyncAdapter` and let `db.sync()` drive it — does not work: a bootstrap of 100k
 * rows would sit inside a single `pull()` call with no way to report progress,
 * pause, resume, or yield to the UI between pages. Orchestration has to live one
 * level up, or it cannot be orchestrated at all.
 */

/** The origin's primary key for a row. Stringified before hashing into an id. */
type RemoteKey = string;
/** A request for one page of the initial bootstrap walk. */
interface BootstrapRequest {
    /**
     * Where to resume. `null` on the first call — which is also when the origin is
     * expected to *issue* the snapshot and delta cursor.
     */
    page: string | number | null;
    /**
     * The snapshot token from the first page, echoed back on every subsequent one.
     * `null` on the first call, and on origins that don't support snapshots.
     */
    snapshot: string | null;
    /** Rows per page. */
    limit: number;
}
/** One page of the bootstrap walk. */
interface BootstrapPage<RemoteRow> {
    rows: RemoteRow[];
    /** Resume token for the next page; `null` when the walk is done. */
    nextPage: string | number | null;
    /**
     * An opaque token pinning every page of this walk to one logical view of the
     * origin.
     *
     * **Omit it and you get `best-effort` coverage, not `complete`.** Without a
     * snapshot, a page walk over live data is not a consistent read: fetch page 1,
     * a row is inserted, everything shifts, and the row that was going to be on
     * page 20 is now on page 19 — which you already passed. It is never seen. The
     * walk still "succeeds", and the replica silently has a hole in it. Since
     * nothing detects that, the honest response is to refuse to call the result
     * complete, and to keep serving reads from the network.
     */
    snapshot?: string;
    /**
     * The cursor to begin the *delta* stream from once the walk finishes. Issued on
     * the first page — i.e. as of the snapshot — so no change made during the walk
     * can slip between "bootstrap ended" and "delta began".
     */
    deltaCursor?: string;
    /** Total rows in scope, when the origin knows it. Drives progress reporting. */
    total?: number;
}
/** One batch of incremental changes since a cursor. */
interface DeltaPage<RemoteRow> {
    changed: RemoteRow[];
    /**
     * Primary keys the origin has deleted.
     *
     * This is the only way a REST replica learns about deletions. A plain paged GET
     * returns survivors, and a row's *absence* from a response is ambiguous — it may
     * have been deleted, or it may merely have shifted to another page. Guessing
     * would eventually delete live data, so we never infer; the origin must say so.
     */
    deleted: RemoteKey[];
    cursor: string;
    hasMore: boolean;
}
/**
 * Everything the coordinator needs to replicate one collection from one origin.
 *
 * @typeParam RemoteRow - the row shape the origin returns, before mapping.
 * @typeParam T - the local document shape.
 */
interface ReplicationSource<RemoteRow = unknown, T extends Document = Document> {
    /** Bump when a custom source's behavior changes without changing its metadata. */
    readonly configVersion?: string | number;
    /** Stable identity for this origin. Part of the coverage key. */
    readonly origin: string;
    /** The local collection this source fills. */
    readonly collection: string;
    /**
     * The authorization slice these rows belong to — a user, tenant, or store.
     * Part of the coverage key, so one user's completeness never licenses another's
     * reads. Use a constant for genuinely global data.
     */
    readonly scope: string;
    /** Bump when {@link mapRow} starts producing a different shape. */
    readonly projectionVersion: number;
    /** Bump when the local schema changes in a way hydrated rows must match. */
    readonly schemaVersion: number;
    /** Fetch one page of the initial walk. */
    bootstrap(request: BootstrapRequest): Promise<BootstrapPage<RemoteRow>>;
    /** Fetch changes since `cursor`. Absent when the origin has no delta feed. */
    delta?(cursor: string): Promise<DeltaPage<RemoteRow>>;
    /**
     * Fetch exactly the rows a specific query needs, for the cold-start bridge.
     *
     * Optional. When absent, a query against an un-hydrated scope simply waits for
     * coverage rather than short-circuiting to the network.
     */
    fetchQuery?(query: BridgeQuery): Promise<RemoteRow[]>;
    /** The origin's primary key for a row. Must be stable across fetches. */
    keyOf(row: RemoteRow): RemoteKey;
    /**
     * Monotonic authoritative revision for stale-response protection. Strongly
     * recommended whenever bridge/bootstrap/delta requests may overlap.
     */
    revisionOf(row: RemoteRow): number;
    /** Shape a remote row into a local document (minus `_id`, which is derived). */
    mapRow(row: RemoteRow): Omit<T, '_id'>;
}
/**
 * A local query, handed to the bridge so it can ask the origin for the same rows.
 *
 * Deliberately loose: every REST API spells pagination and filtering differently,
 * so translating this into a query string is the source's job, not ours.
 */
interface BridgeQuery {
    filter?: Record<string, unknown>;
    sort?: Record<string, 1 | -1>;
    page?: number;
    limit?: number;
}

/**
 * The replication coordinator — all orchestration, no wire format.
 *
 * Owns: the bootstrap walk, resume-after-crash, delta refresh, the cold-start
 * bridge, batching, yielding, coverage transitions, and in-flight dedup. The
 * {@link ReplicationSource} it drives owns only wire translation.
 *
 * ## The two mechanisms are one mechanism
 *
 * "Fetch the page the user is looking at" and "import the whole catalog in the
 * background" look like separate features. They are the same primitive with two
 * schedulers: *fetch rows → upsert them by derived id*. Because both write the
 * **same rows under the same ids**, they compose for free — a bridged fetch is not
 * a throwaway cache entry, it is a down payment on the replica, and when the walk
 * later reaches those rows it overwrites them in place instead of duplicating
 * them. Nothing has to reconcile the two.
 *
 * The one thing they do *not* share is coverage. A bridge fetch must never advance
 * the bootstrap cursor, because it did not come from the walk's snapshot and
 * proves nothing about completeness. Trading a little duplicate network for a
 * trustworthy completeness proof is the right side of that bargain.
 */

interface CoordinatorOptions<T extends Document = Document> {
    /** Rows per bootstrap page. Larger = fewer commits, longer stalls. */
    pageSize?: number;
    /**
     * Called between pages so the walk yields. Defaults to a macrotask.
     *
     * This matters more than it looks. Live queries re-run on a 300 ms poll, and on
     * React Native every write is *synchronous on the JS thread* — a tight bootstrap
     * loop starves both, and the UI freezes for the duration of the import.
     */
    yieldFn?: () => Promise<void>;
    /** Fired after each committed page, for progress UI. */
    onProgress?: (state: CoverageState) => void;
    /** Collection schema/migration options registered by the host application. */
    collectionOptions?: CollectionOptions<T>;
}
declare const REPLICA_SCOPE_FIELD = "_replica_scope";
declare const REPLICA_REVISION_FIELD = "_remote_rev";
interface BridgeResult {
    count: number;
    ids: string[];
}
declare class ReplicationCoordinator<RemoteRow, T extends Document> {
    private readonly db;
    private readonly source;
    private readonly coverage;
    private readonly key;
    private readonly pageSize;
    private readonly yieldFn;
    private readonly onProgress?;
    private readonly collectionOptions?;
    /**
     * In-flight passes, keyed by intent. Two components mounting the same query must
     * fire one request, and the background walk must not race the bridge for the
     * same rows — both join the existing promise instead.
     */
    private readonly inflight;
    constructor(db: TalaDB, source: ReplicationSource<RemoteRow, T>, options?: CoordinatorOptions<T>);
    get replicaScope(): string;
    private get identityNamespace();
    getCoverage(): Promise<CoverageState>;
    /** Whether a purely local read is authorized right now. */
    isReady(): Promise<boolean>;
    /** Dedup by intent: identical concurrent work joins rather than duplicating. */
    private dedup;
    /**
     * Write a batch of remote rows into the local collection.
     *
     * One commit for the whole batch, ids derived from the origin's primary key, and
     * `origin: 'remote'` so the rows can never replicate back out at the origin they
     * came from. This is the *only* write path in the coordinator — bootstrap, delta
     * and bridge all funnel through it, which is precisely why they converge instead
     * of conflicting.
     */
    private applyRows;
    /**
     * Hydrate the scope: walk the origin page by page until the whole collection is
     * local, then mark it complete.
     *
     * Resumable and idempotent. If the walk is interrupted — a reload, a crash, a
     * dead network — the next call picks up from the last committed page, and
     * re-applying a page it already wrote is a no-op because the ids are derived.
     */
    hydrate(): Promise<CoverageState>;
    private runHydrate;
    /**
     * Apply incremental changes since the stored cursor.
     *
     * Deletions are applied by mapping the origin's primary keys through the same
     * `deriveDocId`, and are written with `origin: 'remote'` so they leave no
     * tombstone — the origin already knows it deleted these, and a tombstone would
     * push its own deletion back at it.
     */
    refresh(): Promise<CoverageState>;
    private runRefresh;
    /**
     * Cold-start bridge: fetch exactly the rows one query needs, right now.
     *
     * Needed because a SPA or React Native app has no server render to paint behind
     * while the replica fills. The rows land in the same collection under the same
     * derived ids as the walk's, so this is not a cache — it is the replica, arriving
     * early.
     *
     * **Does not advance coverage.** These rows did not come from the bootstrap
     * snapshot and prove nothing about completeness; treating them as progress would
     * let a page-1 fetch masquerade as a hydrated catalog.
     */
    bridge(query: BridgeQuery): Promise<BridgeResult>;
    /** Drop coverage and force a fresh bootstrap. Local rows are left alone. */
    reset(): Promise<void>;
}

/**
 * A {@link ReplicationSource} for an ordinary paged JSON API.
 *
 * This is the adoption path: point it at `GET /api/products?page=1&limit=500` and
 * a team on Express + Postgres gets a local replica without rewriting their API to
 * speak TalaDB's sync contract. Everything here is wire translation — the
 * coordinator owns the walk, the coverage, and the retries.
 *
 * ## What the origin has to provide, and what happens when it doesn't
 *
 * | Feature | Endpoint | Without it |
 * |---|---|---|
 * | Paged list | `?page=&limit=` | Nothing works. Required. |
 * | Snapshot token | `snapshot` in the response | Coverage caps at `best-effort`; reads keep hitting the network |
 * | Delta feed | `?since=<cursor>` | No incremental refresh, and **deletions never propagate** |
 *
 * The snapshot and the delta feed are each about twenty minutes of Express work
 * (a monotonic `updated_at`/revision column, a soft-delete table, and a
 * `rev <= snapshotRev` predicate). They are worth it: without a snapshot the
 * replica can never be trusted for a local-only read, which is the entire point.
 */

interface RestSourceOptions<RemoteRow, T extends Document> {
    /** Base URL, e.g. `/api/products`. */
    endpoint: string;
    /** The local collection to fill. */
    collection: string;
    /** Stable identity for the origin. Defaults to `endpoint`. */
    origin?: string;
    /**
     * The authorization slice these rows belong to — a user id, tenant, or store.
     * Part of the coverage key, so one user's completeness never licenses another
     * user's reads. Defaults to `'global'`; **set it for anything user-scoped.**
     */
    scope?: string;
    /** Bump when {@link mapRow} starts producing a different shape. Default 1. */
    projectionVersion?: number;
    /** Bump when the local schema changes. Default 1. */
    schemaVersion?: number;
    /** Field on the remote row holding its primary key. Default `'id'`. */
    key?: string;
    /** Field/callback yielding a monotonic numeric row revision. Default `'rev'`. */
    revision?: string | ((row: RemoteRow) => number | undefined);
    /** Shape a remote row into a local document. Default: identity, minus `_id`. */
    mapRow?: (row: RemoteRow) => Omit<T, '_id'>;
    /** Per-request headers, resolved **at send time** so a refreshed token is used. */
    getAuth?: () => Promise<Record<string, string>> | Record<string, string>;
    /** `fetch` implementation. Defaults to the global. */
    fetch?: typeof fetch;
    /** Sub-paths appended to `endpoint`. */
    paths?: {
        bootstrap?: string;
        delta?: string;
    };
    /** Enable delta polling. Defaults to true only when `paths.delta` is set. */
    delta?: boolean;
    /** Meaning of the fallback `page` parameter when no next token is returned. */
    pagination?: 'page' | 'offset';
    /** Translate a local query into this API's query-string conventions. */
    toParams?: (query: BridgeQuery) => Record<string, string>;
    /** Pull the row array out of a response whose envelope we don't recognize. */
    parse?: (body: unknown) => unknown[];
}
declare function createRestSource<RemoteRow = Record<string, unknown>, T extends Document = Document>(options: RestSourceOptions<RemoteRow, T>): ReplicationSource<RemoteRow, T>;

/**
 * Thrown when a document fails schema validation on `insert` or `insertMany`.
 * The `cause` property holds the original error thrown by the schema library.
 */
declare class TalaDbValidationError extends Error {
    readonly cause: unknown;
    constructor(cause: unknown, context?: string);
}
/**
 * Wraps a `Collection<T>` to intercept writes through a schema validator
 * (`schema`), stamp `_v` on insert when a `syncSchema.version` is declared, and
 * normalize reads through a lazy `migrateDocument`. Returns the collection
 * unchanged when none of those apply.
 *
 * Read normalization covers `find`, `findOne`, and `subscribe` (live queries).
 * It cannot cover `aggregate`, `findNearest`, or FTS `search`: those run inside
 * the engine against the *stored* shape, so a below-version document is matched
 * and projected as it sits on disk. Use `persistMigrations` or
 * `openDB({ migrations })` to rewrite storage if you query old documents that way.
 *
 * @internal Exported for unit testing; not part of the public API surface.
 */
declare function applySchema<T extends Document>(col: Collection<T>, options: CollectionOptions<T>): Collection<T>;

/**
 * A single application schema migration, run once at `openDB` when its
 * `version` is greater than the database's stored migration version.
 */
interface Migration {
    /** Monotonic version. Must be a positive integer, unique across the array. */
    version: number;
    /** Optional human-readable label for logs. */
    description?: string;
    /**
     * The migration body. Receives the open database and may use the full
     * collection API. Runs to completion before the version is advanced.
     *
     * **Write migrations idempotently.** TalaDB checkpoints per version (the
     * stored version advances only after `up` fully resolves), but a single `up`
     * is not wrapped in one atomic transaction — if it throws partway, the writes
     * it already made persist and `up` re-runs from the start on the next open.
     */
    up: (db: TalaDB) => Promise<void> | void;
}
/**
 * Runtime-agnostic migration runner. Each binding supplies `getVersion` /
 * `setVersion` (its own persisted counter); the loop is identical everywhere.
 *
 * Runs pending migrations (`version` > stored) in ascending order, advancing
 * the stored version after each `up` resolves — checkpoint per version. If an
 * `up` throws, the loop stops and the error propagates; the stored version
 * reflects the last fully-applied migration, so the next open resumes there.
 *
 * @internal Exported for unit testing; not part of the public API surface.
 */
declare function runMigrations(db: TalaDB, getVersion: () => Promise<number>, setVersion: (v: number) => Promise<void>, migrations: Migration[]): Promise<void>;
/** Options for `openDB`. */
interface OpenDBOptions {
    /** Encrypt native database values at rest. Never hard-code this value. */
    passphrase?: string;
    /**
     * Ordered application schema migrations, run once each at open in ascending
     * `version` order (only those newer than the stored migration version). The
     * stored version advances after each migration succeeds — checkpoint per
     * version, resuming from the last applied one on the next open.
     *
     * Supported on Node.js, the browser (via the OPFS worker), and React Native.
     * On a binding whose native module predates 0.9.2 (no `userVersion` /
     * `setUserVersion`), `openDB` throws rather than silently skipping the
     * migrations — rebuild or update the native module.
     */
    migrations?: Migration[];
    /**
     * Explicit path to a `taladb.config.yml` / `taladb.config.json` file.
     * If omitted, TalaDB auto-discovers the file from `process.cwd()` on Node.js.
     * Ignored on browser and React Native — those platforms do not support
     * file-based config discovery. Pass `config` inline instead, or on React Native
     * pass `JSON.stringify(config)` as the second argument to `TalaDBModule.initialize`.
     */
    configPath?: string;
    /**
     * Inline config object. Takes precedence over any config file when provided.
     * Useful for passing config programmatically without a config file on disk.
     */
    config?: TalaDbConfig;
    /**
     * Storage durability, e.g. `{ flush_every_write: false }` to batch commits
     * for write throughput (call `db.flush()` to force a sync), or `{ flush_ms }`
     * to tune the browser IndexedDB-fallback snapshot debounce. Merged into
     * `config.durability`. Node + browser; on React Native pass it in the config
     * JSON to `TalaDBModule.initialize`.
     */
    durability?: DurabilityConfig;
}
/**
 * Open a TalaDB database.
 *
 * @param dbName   Name of the database file (used for OPFS and native file paths).
 * @param options  Optional config. Pass `{ config }` for inline sync settings or
 *                 `{ configPath }` to load from a specific file.
 *
 * @example
 * const db = await openDB('myapp.db');
 *
 * @example with inline sync config
 * const db = await openDB('myapp.db', {
 *   config: { sync: { enabled: true, endpoint: 'https://api.example.com/events' } },
 * });
 */
declare function openDB(dbName?: string, options?: OpenDBOptions): Promise<TalaDB>;

export { type AggregatePipeline, type AggregateStage, type BootstrapPage, type BootstrapRequest, type BridgeQuery, type BridgeResult, COVERAGE_COLLECTION, type Collection, type CollectionIndexInfo, type CollectionOptions, type CoordinatorOptions, type CoverageKey, type CoverageState, CoverageStore, type CursorSyncAdapter, type DeltaPage, type Document, type DurabilityConfig, type Filter, HttpSyncAdapter, type Migration, type OpenDBOptions, type PullResult, REPLICA_REVISION_FIELD, REPLICA_SCOPE_FIELD, type RemoteKey, ReplicationCoordinator, type ReplicationSource, type RestSourceOptions, type Schema, type SerializedChangeset, type SyncAdapter, type SyncConfig, type SyncDirection, type SyncOptions, type SyncResult, type TalaDB, type TalaDbConfig, TalaDbValidationError, type Update, type Value, type VectorIndexOptions, type VectorMetric, type VectorSearchResult, type WriteOrigin, applySchema, coverageKey, createRestSource, deriveDocId, isAuthoritative, openDB, progress, rowsApplied, runMigrations };
