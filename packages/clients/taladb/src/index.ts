import type {
  Collection,
  CollectionIndexInfo,
  CollectionOptions,
  Document,
  TalaDB,
  SyncAdapter,
  SyncOptions,
  SyncSchema,
  QuarantinedDocument,
  AggregatePipeline,
  Update,
  Filter,
  WriteOrigin,
} from './types';
import { loadConfig, validateConfig } from './config';
import type { TalaDbConfig, SyncConfig, DurabilityConfig } from './config';
import { runSync, unsupportedSync, type SyncHandle } from './sync';

// Re-export all public types for consumers (export…from satisfies S7763)
export type {
  Collection,
  CollectionIndexInfo,
  CollectionOptions,
  Document,
  Filter,
  Schema,
  Update,
  Value,
  TalaDB,
  VectorMetric,
  VectorIndexOptions,
  VectorSearchResult,
  AggregateStage,
  AggregatePipeline,
  SyncAdapter,
  SyncOptions,
  SyncResult,
  SyncDirection,
  SerializedChangeset,
  WriteOrigin,
  CursorSyncAdapter,
  PullResult,
} from './types';

export { HttpSyncAdapter } from './http-adapter';
export { deriveDocId } from './derive-id';

// --------------- Replication (coverage-first) ---------------
export {
  ReplicationCoordinator,
  REPLICA_SCOPE_FIELD,
  REPLICA_REVISION_FIELD,
  type CoordinatorOptions,
  type BridgeResult,
} from './replication/coordinator';
export {
  CoverageStore,
  COVERAGE_COLLECTION,
  coverageKey,
  isAuthoritative,
  progress,
  rowsApplied,
  type CoverageKey,
  type CoverageState,
} from './replication/coverage';
export { createRestSource, type RestSourceOptions } from './replication/rest';
export type {
  BootstrapPage,
  BootstrapRequest,
  BridgeQuery,
  DeltaPage,
  RemoteKey,
  ReplicationSource,
} from './replication/source';

// ============================================================
// Validation
// ============================================================

/**
 * Thrown when a document fails schema validation on `insert` or `insertMany`.
 * The `cause` property holds the original error thrown by the schema library.
 */
export class TalaDbValidationError extends Error {
  constructor(
    public readonly cause: unknown,
    context?: string,
  ) {
    const label = context ? ` (${context})` : '';
    const msg = cause instanceof Error ? cause.message : String(cause);
    super(`TalaDB schema validation failed${label}: ${msg}`);
    this.name = 'TalaDbValidationError';
  }
}

/** Structural deep-equality. Key order and property order are not significant. */
function deepEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((x, i) => deepEqual(x, b[i]));
  }
  const ao = a as Record<string, unknown>;
  const bo = b as Record<string, unknown>;
  const keys = Object.keys(ao);
  if (keys.length !== Object.keys(bo).length) return false;
  return keys.every(
    (k) => Object.prototype.hasOwnProperty.call(bo, k) && deepEqual(ao[k], bo[k]),
  );
}

/** A document after read-time normalization, plus whether it may be written back. */
interface Normalized<T> {
  value: T;
  /** `false` for a downcast projection of a newer-shaped stored document. */
  persistable: boolean;
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
export function applySchema<T extends Document>(
  col: Collection<T>,
  options: CollectionOptions<T>,
): Collection<T> {
  const {
    schema,
    validateOnRead = false,
    migrateDocument,
    downgradeDocument,
    syncSchema,
    persistMigrations = false,
    allowFieldRemoval = false,
    retiredFields = [],
  } = options;
  const targetVersion = syncSchema?.version ?? 0;

  if (migrateDocument && targetVersion < 1) {
    throw new Error('CollectionOptions.migrateDocument requires syncSchema.version (the migration target)');
  }
  if (downgradeDocument && targetVersion < 1) {
    throw new Error('CollectionOptions.downgradeDocument requires syncSchema.version (the shape this build reads)');
  }
  // `renames`/`defaults` only run on the import path when a document's `_v` is
  // below `version`. With no version the migration step is skipped entirely
  // while `required`/`types` still apply — so a rename schema would quarantine
  // every document it was meant to upgrade. Refuse the combination outright.
  if (syncSchema && targetVersion < 1 && (syncSchema.renames || syncSchema.defaults)) {
    throw new Error(
      'CollectionOptions.syncSchema.renames/defaults require syncSchema.version >= 1 — ' +
        'without a version the import migration step never runs and documents missing the ' +
        'renamed/defaulted fields are quarantined instead of upgraded',
    );
  }

  // Documents written locally carry `_v` so they are recognisable as already
  // current. Without the stamp a freshly-inserted document reads back with no
  // `_v`, is treated as version 0, and gets fed through `migrateDocument` as if
  // it were legacy data — corrupting brand-new documents.
  const stampVersion = targetVersion > 0;
  if (!schema && !migrateDocument && !downgradeDocument && !stampVersion) return col;

  /**
   * Re-attach keys that `original` carried and `next` dropped.
   *
   * The dropped keys are overwhelmingly fields belonging to a *newer* peer:
   * neither this build's schema nor its migration function has heard of them,
   * so both silently omit them from their output. Under whole-document LWW that
   * omission is not a no-op — it is a deletion that replicates. Preserving them
   * keeps an old replica a faithful carrier of shapes it does not understand,
   * which is what makes additive schema evolution safe across a version-drifted
   * fleet.
   */
  const retired = new Set<string>(retiredFields);
  const engineOwned = new Set([
    '_id', '_v', '_changed_at', '_remote', '_remote_rev', '_replica_scope',
  ]);
  const downcastViews = new WeakSet<object>();

  function preserveFields(
    original: T,
    next: T,
    preserveUnknown: boolean,
    preserveVersion = true,
  ): T {
    let out: T | null = null;
    for (const k of Object.keys(original)) {
      const ownedField = engineOwned.has(k) && (k !== '_v' || preserveVersion);
      const mustRestore = ownedField ||
        (preserveUnknown && !retired.has(k) && !(k in (next as Record<string, unknown>)));
      if (!mustRestore) continue;
      if (!ownedField && k in (next as Record<string, unknown>)) continue;
      out ??= { ...next };
      (out as Record<string, unknown>)[k] = (original as Record<string, unknown>)[k];
    }
    return out ?? next;
  }

  function parseWrite(doc: unknown, label: string): T {
    try {
      return schema!.parse(doc);
    } catch (err) {
      throw new TalaDbValidationError(err, label);
    }
  }

  function assertWritableDocument(doc: unknown, label: string): void {
    if (doc && typeof doc === 'object' && downcastViews.has(doc as object)) {
      throw new Error(`${label}: a downgradeDocument result is a read-only compatibility view`);
    }
    const version = (doc as Document | null | undefined)?._v;
    if (targetVersion > 0 && typeof version === 'number' && version > targetVersion) {
      throw new Error(
        `${label}: this client supports schema v${targetVersion}, but the document is v${version}`,
      );
    }
  }

  function writableFilter(filter: Filter<T>): Filter<T> {
    if (targetVersion < 1) return filter;
    return {
      $and: [
        filter,
        {
          $or: [
            { _v: { $exists: false } },
            { _v: { $lte: targetVersion } },
          ],
        },
      ],
    } as Filter<T>;
  }

  function assertSafeUpdate(update: Update<T>): void {
    const record = update as Record<string, Record<string, unknown> | undefined>;
    for (const op of ['$set', '$unset', '$inc', '$push', '$pull']) {
      const fields = record[op];
      if (!fields) continue;
      for (const field of Object.keys(fields)) {
        if (engineOwned.has(field)) {
          throw new Error(`update cannot modify engine-owned field '${field}'`);
        }
      }
    }
  }

  /** Stamp the current shape version, unless the caller set `_v` explicitly. */
  function stamp(doc: Omit<T, '_id'>): Omit<T, '_id'> {
    if (!stampVersion || (doc as Document)._v !== undefined) return doc;
    return { ...doc, _v: targetVersion };
  }

  /** As {@link stamp}, but for writes that carry their own `_id` (replication). */
  function stampDoc(doc: T): T {
    if (!stampVersion || (doc as Document)._v !== undefined) return doc;
    return { ...doc, _v: targetVersion };
  }

  /**
   * `$set` (and, only under `allowFieldRemoval`, `$unset`) that turns `original`
   * into `migrated`, ignoring `_id`.
   *
   * Additive-only by default: a field the migration omitted is left in storage
   * rather than deleted. See `CollectionOptions.allowFieldRemoval` for why — the
   * short version is that a v1 migration's omission of a v3 field is ignorance,
   * not intent, and `$unset` would replicate that ignorance as a deletion to
   * every peer.
   */
  function diffUpdate(original: T, migrated: T): Update<T> | null {
    const $set: Record<string, unknown> = {};
    const $unset: Record<string, true> = {};
    for (const k of Object.keys(migrated)) {
      if (k === '_id') continue;
      if (!deepEqual(migrated[k], original[k])) $set[k] = migrated[k];
    }
    if (allowFieldRemoval || retired.size > 0) {
      for (const k of Object.keys(original)) {
        if (
          k !== '_id' &&
          !(k in migrated) &&
          (allowFieldRemoval || retired.has(k))
        ) $unset[k] = true;
      }
    }
    const update: Record<string, unknown> = {};
    if (Object.keys($set).length) update.$set = $set;
    if (Object.keys($unset).length) update.$unset = $unset;
    return Object.keys(update).length ? (update as Update<T>) : null;
  }

  /**
   * Lazy read-time normalization against `targetVersion` — the shape *this*
   * build speaks.
   *
   * - `_v < target` → **upcast** via `migrateDocument`, stamp `_v = target`.
   *   Persistable: the document is legacy data this build owns and may rewrite.
   *   Fields the migration omitted are restored unless `allowFieldRemoval`.
   * - `_v > target` → **downcast** via `downgradeDocument`, `_v` left as-is.
   *   *Not* persistable: the stored document belongs to a newer shape that this
   *   replica still has to hand on to other peers intact. Writing a projection
   *   back would overwrite the truth with this build's lossy view of it and push
   *   that outward under LWW — the exact fleet-wide corruption the downcast hook
   *   exists to avoid.
   * - equal → untouched.
   */
  function normalizeRead(doc: T): { value: T; persistable: boolean } {
    const fromVersion = typeof doc._v === 'number' ? doc._v : 0;
    if (migrateDocument && fromVersion < targetVersion) {
      const up = { ...migrateDocument(doc, fromVersion), _v: targetVersion };
      return {
        // The migration owns the version transition, while every other
        // engine-owned field continues to come from the stored document.
        value: allowFieldRemoval
          ? preserveFields(doc, up, false, false)
          : preserveFields(doc, up, true, false),
        persistable: true,
      };
    }
    if (downgradeDocument && fromVersion > targetVersion) {
      const projected = {
        ...downgradeDocument(doc, fromVersion),
        ...(doc._id !== undefined ? { _id: doc._id } : {}),
        _v: fromVersion,
      } as T;
      downcastViews.add(projected as object);
      return { value: projected, persistable: false };
    }
    return { value: doc, persistable: true };
  }

  /**
   * Validate the read shape without narrowing it. Zod/Valibot object schemas
   * strip unknown keys, so returning `parse()`'s output verbatim would hand
   * application code a document with a newer peer's fields silently removed —
   * which a read-modify-write then writes back as a deletion. Parse for the
   * check and for any coercions it applies, then restore what it dropped.
   */
  function validateRead(doc: T): T {
    if (!validateOnRead || !schema) return doc;
    try {
      const parsed = schema.parse(doc);
      const fromVersion = typeof doc._v === 'number' ? doc._v : 0;
      return preserveFields(doc, parsed, targetVersion > 0 && fromVersion > targetVersion);
    } catch (err) {
      throw new TalaDbValidationError(err, 'read');
    }
  }

  /**
   * Opt-in write-back of upgraded documents, so the migration becomes permanent
   * and filters/indexes on the new shape match.
   *
   * Sequential, not `Promise.all`: each write-back is its own engine
   * transaction — fsync'd under the default `flush_every_write` durability —
   * and fires live-query and sync-push notifications. Fanning them out turned a
   * single `find()` over a large un-migrated collection into an N-way
   * concurrent write storm. `openDB({ migrations })` remains the right tool for
   * rewriting a whole collection at once.
   */
  async function persistAll(originals: T[], normalized: Normalized<T>[]): Promise<void> {
    if (!persistMigrations) return;
    for (let i = 0; i < originals.length; i++) {
      const original = originals[i];
      // A downcast projection is a lossy view of a newer stored document and
      // must never be written back over it.
      if (!normalized[i].persistable) continue;
      const migrated = normalized[i].value;
      // Reference-identical => normalizeRead left it alone => already current.
      if (migrated === original || typeof original._id !== 'string') continue;
      const update = diffUpdate(original, migrated);
      if (!update) continue;
      try {
        const guards: Filter<T>[] = [{ _id: original._id } as Filter<T>];
        if (original._v === undefined) guards.push({ _v: { $exists: false } } as Filter<T>);
        else guards.push({ _v: original._v } as Filter<T>);
        if (original._changed_at !== undefined) {
          guards.push({ _changed_at: original._changed_at } as Filter<T>);
        }
        await col.updateOne({ $and: guards } as Filter<T>, update);
      } catch {
        // Best-effort: the returned value is still migrated, and the write-back
        // is retried on the next read.
      }
    }
  }

  const wrapReads =
    Boolean(migrateDocument) || Boolean(downgradeDocument) || (validateOnRead && Boolean(schema));
  const wrapWrites = Boolean(schema) || stampVersion;

  function pipelinePreservesDocuments(pipeline: AggregatePipeline<T>): boolean {
    return pipeline.every((stage) => !('$group' in stage) && !('$project' in stage));
  }

  function normalizeViewRows<R extends Document>(docs: R[]): R[] {
    return docs.map((doc) => validateRead(normalizeRead(doc as unknown as T).value) as unknown as R);
  }

  return {
    ...col,
    insert: wrapWrites
      ? async (doc) => {
          assertWritableDocument(doc, 'insert');
          if (schema) parseWrite(doc, 'insert');
          return col.insert(stamp(doc));
        }
      : col.insert.bind(col),
    insertMany: wrapWrites
      ? async (docs) => {
          docs.forEach((doc, i) => assertWritableDocument(doc, `insertMany[${i}]`));
          if (schema) docs.forEach((doc, i) => parseWrite(doc, `insertMany[${i}]`));
          return col.insertMany(docs.map(stamp));
        }
      : col.insertMany.bind(col),
    // Rows arriving from a remote origin are validated like any other write. This
    // is the "parse, don't assert" boundary: the compile-time generic and the
    // runtime schema check have to be the same seam, or a malformed server
    // response walks straight into a typed collection.
    replaceManyWithIds: wrapWrites
      ? async (docs, origin) => {
          if (origin !== 'remote') {
            docs.forEach((doc, i) => assertWritableDocument(doc, `replaceManyWithIds[${i}]`));
            if (targetVersion > 0) {
              throw new Error(
                'local replaceManyWithIds is disabled on versioned collections; ' +
                  'use updateOne/updateMany so schema-version guards are atomic',
              );
            }
          }
          if (schema) docs.forEach((doc, i) => {
            const { _replica_scope, _remote_rev, ...schemaDoc } = doc as T & {
              _replica_scope?: unknown;
              _remote_rev?: unknown;
            };
            void _replica_scope;
            void _remote_rev;
            parseWrite(schemaDoc, `replaceManyWithIds[${i}]`);
          });
          return col.replaceManyWithIds(docs.map((d) => stampDoc(d)), origin);
        }
      : col.replaceManyWithIds.bind(col),
    deleteManyWithIds: stampVersion
      ? async (ids, origin) => {
          if (origin !== 'remote') {
            throw new Error(
              'local deleteManyWithIds is disabled on versioned collections; ' +
                'use deleteOne/deleteMany so schema-version guards are atomic',
            );
          }
          return col.deleteManyWithIds(ids, origin);
        }
      : col.deleteManyWithIds.bind(col),
    updateOne: wrapWrites
      ? async (filter, update) => {
          assertSafeUpdate(update);
          return col.updateOne(writableFilter(filter), update);
        }
      : col.updateOne.bind(col),
    updateMany: wrapWrites
      ? async (filter, update) => {
          assertSafeUpdate(update);
          return col.updateMany(writableFilter(filter), update);
        }
      : col.updateMany.bind(col),
    deleteOne: stampVersion
      ? (filter) => col.deleteOne(writableFilter(filter))
      : col.deleteOne.bind(col),
    deleteMany: stampVersion
      ? (filter) => col.deleteMany(writableFilter(filter))
      : col.deleteMany.bind(col),
    find: wrapReads
      ? async (filter?) => {
          const docs = await col.find(filter);
          const normalized = docs.map(normalizeRead);
          await persistAll(docs, normalized);
          return normalized.map((n) => validateRead(n.value));
        }
      : col.find.bind(col),
    findOne: wrapReads
      ? async (filter) => {
          const doc = await col.findOne(filter);
          if (doc === null) return null;
          const normalized = normalizeRead(doc);
          await persistAll([doc], [normalized]);
          return validateRead(normalized.value);
        }
      : col.findOne.bind(col),
    aggregate: wrapReads
      ? async <R extends Document = Document>(pipeline: AggregatePipeline<T>): Promise<R[]> => {
          const docs = await col.aggregate<R>(pipeline);
          return pipelinePreservesDocuments(pipeline) ? normalizeViewRows(docs) : docs;
        }
      : col.aggregate.bind(col),
    // Live queries feed every @taladb/react hook (useFind, useFindOne,
    // useQueries). Leaving them unwrapped meant React components received the
    // un-migrated shape while a direct find() returned the migrated one.
    subscribe: wrapReads
      ? (filter, callback, onError) =>
          col.subscribe(
            filter,
            (docs) => {
              const normalized = docs.map(normalizeRead);
              let out: T[];
              try {
                out = normalized.map((n) => validateRead(n.value));
              } catch (err) {
                onError?.(err);
                return;
              }
              callback(out);
              // Fire-and-forget: the callback is synchronous, and a successful
              // write-back re-fires this same subscription with the upgraded
              // documents (which then need no further write).
              void persistAll(docs, normalized);
            },
            onError,
          )
      : col.subscribe.bind(col),
    subscribeAggregate: wrapReads
      ? <R extends Document = Document>(
          pipeline: AggregatePipeline<T>,
          callback: (docs: R[]) => void,
          onError?: (error: unknown) => void,
        ) => col.subscribeAggregate<R>(
          pipeline,
          (docs) => {
            if (!pipelinePreservesDocuments(pipeline)) {
              callback(docs);
              return;
            }
            try {
              callback(normalizeViewRows(docs));
            } catch (error) {
              onError?.(error);
            }
          },
          onError,
        )
      : col.subscribeAggregate.bind(col),
  };
}

export type { TalaDbConfig, SyncConfig, DurabilityConfig } from './config';

// ============================================================
// Platform detection + dynamic import
// ============================================================

type Platform = 'browser' | 'react-native' | 'node';

function detectPlatform(): Platform {
  // navigator.product === 'ReactNative' is the canonical check across all RN
  // versions. nativeCallSyncHook is absent in the New Architecture (RN 0.71+),
  // and window/navigator are both defined on RN (window === global), so the
  // browser check must not run before this.
  if (typeof navigator !== 'undefined' && (navigator as unknown as { product?: string }).product === 'ReactNative') {
    return 'react-native';
  }
  if ((globalThis as Record<string, unknown>).nativeCallSyncHook !== undefined) {
    return 'react-native';
  }
  // Browser: window + navigator exist
  if (globalThis.window !== undefined && typeof navigator !== 'undefined') {
    return 'browser';
  }
  return 'node';
}

// ============================================================
// Browser adapter — SharedWorker + OPFS via FileSystemSyncAccessHandle
// ============================================================

/**
 * Thin proxy that forwards every DB operation to the SharedWorker
 * via a typed message protocol and awaits the response.
 *
 * The SharedWorker (taladb.worker.js) owns the OPFS file handle and the
 * WASM + redb instance. Multiple tabs share the same worker instance so
 * there is always exactly one writer.
 */
// WorkerProxy works with both MessagePort (SharedWorker) and Worker.
// Worker does not have a .start() method; MessagePort requires it.
type WorkerLike = Pick<Worker, 'postMessage'> & { onmessage: ((e: MessageEvent) => void) | null; start?: () => void };

class WorkerProxy {
  private readonly port: WorkerLike;
  private readonly pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private nextId = 1;
  private dead: Error | null = null;

  constructor(port: WorkerLike) {
    this.port = port;
    this.port.onmessage = (e) => {
      const { id, result, error } = e.data;
      const p = this.pending.get(id);
      if (p) {
        this.pending.delete(id);
        if (error === undefined) p.resolve(result);
        else p.reject(new Error(error));
      }
    };
    // MessagePort requires .start(); Worker does not have it.
    this.port.start?.();
  }

  send<T = unknown>(op: string, args: Record<string, unknown> = {}): Promise<T> {
    if (this.dead) return Promise.reject(this.dead);
    return new Promise((resolve, reject) => {
      const id = this.nextId++;
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      this.port.postMessage({ id, op, ...args });
    });
  }

  /**
   * Reject every in-flight request and refuse new ones. Called when the
   * worker errors or is terminated — without this, pending promises would
   * hang forever (awaiting callers deadlock).
   */
  abort(reason: Error): void {
    this.dead = reason;
    for (const [, p] of this.pending) p.reject(reason);
    this.pending.clear();
  }
}

// ---------------------------------------------------------------------------
// Shared subscribe helper
// ---------------------------------------------------------------------------

/**
 * Polls `findFn` every 300 ms and fires `callback` whenever the result changes
 * (detected via JSON equality). Returns an unsubscribe function.
 *
 * Used by the in-memory, Node, and React Native adapters. The browser-worker
 * adapter has its own variant that integrates with BroadcastChannel nudging.
 */
function makePoller<T extends Document>(
  findFn: () => Promise<T[]>,
  callback: (docs: T[]) => void,
  onError?: (error: unknown) => void,
): () => void {
  let active = true;
  let lastJson = '';
  let running = false;
  let rerun = false;
  const poll = async () => {
    if (!active) return;
    if (running) { rerun = true; return; }
    running = true;
    try {
      const docs = await findFn();
      if (!active) return;
      const json = JSON.stringify(docs);
      if (json !== lastJson) {
        lastJson = json;
        callback(docs);
      }
    } catch (error) {
      if (active) onError?.(error);
    } finally {
      running = false;
      if (active) {
        if (rerun) { rerun = false; void poll(); }
        else setTimeout(poll, 300);
      }
    }
  };
  poll();
  return () => { active = false; };
}

/**
 * In-memory fallback for browsers that don't support SharedWorker (e.g. Safari iOS).
 * Data is not persisted across page reloads; all writes live in WASM memory only.
 */
async function createInMemoryBrowserDB(_dbName: string): Promise<TalaDB> {
  const wasmUrl = new URL('@taladb/web/pkg/taladb_web.js', import.meta.url);
  const wasm = await import(/* @vite-ignore */ wasmUrl.href);
  await wasm.default();
  const db = wasm.TalaDBWasm.openInMemory();

  function wrapCollection<T extends Document>(name: string, opts?: CollectionOptions<T>): Collection<T> {
    const col = db.collection(name);
    const wrapped: Collection<T> = {
      insert: async (doc) => col.insert(doc),
      insertMany: async (docs) => col.insertMany(docs),
      replaceManyWithIds: async (docs, origin = 'local') => col.replaceManyWithIds(docs, origin),
      deleteManyWithIds: async (ids, origin = 'local') => col.deleteManyWithIds(ids, origin),
      find: async (filter?) => col.find(filter ?? null),
      findOne: async (filter) => col.findOne(filter) ?? null,
      updateOne: async (filter, update) => col.updateOne(filter, update),
      updateMany: async (filter, update) => col.updateMany(filter, update),
      deleteOne: async (filter) => col.deleteOne(filter),
      deleteMany: async (filter) => col.deleteMany(filter),
      count: async (filter?) => col.count(filter ?? null),
      aggregate: async <R extends Document = Document>(pipeline: AggregatePipeline<T>): Promise<R[]> =>
        col.aggregate(pipeline as never) as R[],
      createIndex: async (field) => col.createIndex(field),
      dropIndex: async (field) => col.dropIndex(field),
      createCompoundIndex: async (fields) => col.createCompoundIndex(JSON.stringify(fields)),
      dropCompoundIndex: async (fields) => col.dropCompoundIndex(JSON.stringify(fields)),
      createFtsIndex: async (field) => col.createFtsIndex(field),
      dropFtsIndex: async (field) => col.dropFtsIndex(field),
      createVectorIndex: async (field, options) => {
        if (options.indexType === 'hnsw') throw new Error('HNSW vector indexes are not available in the browser (requires native threads). Use Node.js or React Native.');
        return col.createVectorIndex(field, options.dimensions, options.metric ?? null, null, null, null);
      },
      dropVectorIndex: async (field) => col.dropVectorIndex(field),
      upgradeVectorIndex: async (_field) => {
        throw new Error('HNSW vector indexes are not available in the browser (requires native threads). Use Node.js or React Native.');
      },
      findNearest: async (field, vector, topK, filter?) => {
        const raw = await col.findNearest(field, vector, topK, filter ?? null) as { document: T; score: number }[];
        return raw;
      },
      listIndexes: async () => {
        const json = col.listIndexes() as string;
        return JSON.parse(json);
      },
      subscribe: (filter, callback, onError) =>
        makePoller(async () => col.find(filter ?? null) as T[], callback, onError),
      subscribeAggregate: <R extends Document = Document>(
        pipeline: AggregatePipeline<T>,
        callback: (docs: R[]) => void,
        onError?: (error: unknown) => void,
      ) => makePoller(async () => wrapped.aggregate<R>(pipeline), callback, onError),
    };
    return opts ? applySchema(wrapped, opts) : wrapped;
  }

  // Not annotated `: TalaDB` so the extra `listCollectionNames` (needed by the
  // internal SyncHandle, not part of the public interface) doesn't trip the
  // excess-property check. Structurally still a TalaDB.
  const handle = {
    collection: <T extends Document>(name: string, opts?: CollectionOptions<T>) => wrapCollection<T>(name, opts),
    compact: async () => {},
    close: async () => {},
    exportChanges: async (collections: string[], sinceMs: number) => db.exportChanges(sinceMs, collections) as string,
    importChanges: async (changeset: string) => db.importChanges(changeset) as number,
    listCollectionNames: async () => db.listCollectionNames() as string[],
    sync: (adapter: SyncAdapter, options: SyncOptions) =>
      runSync(handle as unknown as SyncHandle, adapter, options),
  };
  return handle satisfies TalaDB;
}

async function createBrowserDB(
  dbName: string,
  config?: TalaDbConfig,
  passphrase?: string,
  migrations?: Migration[],
): Promise<TalaDB> {
  // createSyncAccessHandle (required for OPFS persistence) is only available
  // in Dedicated Workers per the WHATWG spec — not SharedWorkers. We use a
  // DedicatedWorker so each tab gets its own isolated worker + file handle.
  const workerUrl = new URL('@taladb/web/worker/taladb.worker.js', import.meta.url);
  const worker = new Worker(workerUrl, { type: 'module', name: 'taladb' });
  const proxy = new WorkerProxy(worker);
  // A crashed worker can never answer — fail in-flight requests instead of
  // letting their promises hang forever.
  worker.onerror = (e: ErrorEvent) => {
    proxy.abort(new Error(`taladb worker error: ${e.message ?? 'unknown'}`));
  };

  // Initialize the worker (opens OPFS file or falls back to IDB-backed in-memory).
  // Pass configJson so the worker can wire up HTTP push sync from the first write.
  const configJson = config !== undefined ? JSON.stringify(config) : undefined;
  try {
    await proxy.send('init', { dbName, configJson, passphrase });
  } catch (e) {
    // A failed init (wrong passphrase, OPFS unavailable, …) must not leave a
    // zombie worker behind: terminating it force-closes any OPFS access
    // handles it acquired, so the caller can retry (e.g. re-prompt for the
    // passphrase) without reloading the page.
    proxy.abort(e instanceof Error ? e : new Error(String(e)));
    worker.terminate();
    throw e;
  }

  // BroadcastChannel: when another tab's worker commits a write it posts
  // "taladb:changed".  We immediately nudge every active subscribe() poller
  // so it re-runs without waiting for the next 300 ms tick.
  const nudgeCallbacks = new Set<() => void>();
  let channel: BroadcastChannel | null = null;
  if (typeof BroadcastChannel !== 'undefined') {
    channel = new BroadcastChannel(`taladb:${dbName}`);
    channel.onmessage = (e: MessageEvent) => {
      if (e.data === 'taladb:changed') {
        for (const nudge of nudgeCallbacks) nudge();
      }
    };
  }

  // Per-collection sync schemas, registered as collections are opened, so
  // `db.sync()` validates pulled documents in the worker ("validate, never cast").
  const syncSchemas: Record<string, SyncSchema> = {};

  function wrapCollection<T extends Document>(name: string, opts?: CollectionOptions<T>): Collection<T> {
    if (opts?.syncSchema) syncSchemas[name] = opts.syncSchema;
    const s = JSON.stringify;
    const wrapped: Collection<T> = {
      insert: (doc) =>
        proxy.send<string>('insert', { collection: name, docJson: s(doc) }),

      insertMany: async (docs) => {
        const json = await proxy.send<string>('insertMany', {
          collection: name, docsJson: s(docs),
        });
        return JSON.parse(json) as string[];
      },

      replaceManyWithIds: async (docs, origin = 'local') => {
        const json = await proxy.send<string>('replaceManyWithIds', {
          collection: name, docsJson: s(docs), origin,
        });
        return JSON.parse(json) as string[];
      },

      deleteManyWithIds: (ids, origin = 'local') =>
        proxy.send<number>('deleteManyWithIds', {
          collection: name, idsJson: s(ids), origin,
        }),

      find: async (filter?) => {
        const json = await proxy.send<string>('find', {
          collection: name, filterJson: filter ? s(filter) : 'null',
        });
        return JSON.parse(json) as T[];
      },

      findOne: async (filter) => {
        const json = await proxy.send<string>('findOne', {
          collection: name, filterJson: filter ? s(filter) : 'null',
        });
        return JSON.parse(json) as T | null;
      },

      updateOne: (filter, update) =>
        proxy.send<boolean>('updateOne', {
          collection: name, filterJson: s(filter), updateJson: s(update),
        }),

      updateMany: (filter, update) =>
        proxy.send<number>('updateMany', {
          collection: name, filterJson: s(filter), updateJson: s(update),
        }),

      deleteOne: (filter) =>
        proxy.send<boolean>('deleteOne', { collection: name, filterJson: s(filter) }),

      deleteMany: (filter) =>
        proxy.send<number>('deleteMany', { collection: name, filterJson: s(filter) }),

      count: (filter?) =>
        proxy.send<number>('count', {
          collection: name, filterJson: filter ? s(filter) : 'null',
        }),

      aggregate: async <R extends Document = Document>(pipeline: AggregatePipeline<T>): Promise<R[]> => {
        const json = await proxy.send<string>('aggregate', {
          collection: name, pipelineJson: s(pipeline),
        });
        return JSON.parse(json) as R[];
      },

      createIndex: (field) =>
        proxy.send<void>('createIndex', { collection: name, field }),

      dropIndex: (field) =>
        proxy.send<void>('dropIndex', { collection: name, field }),

      createCompoundIndex: (fields) =>
        proxy.send<void>('createCompoundIndex', { collection: name, fieldsJson: JSON.stringify(fields) }),

      dropCompoundIndex: (fields) =>
        proxy.send<void>('dropCompoundIndex', { collection: name, fieldsJson: JSON.stringify(fields) }),

      createFtsIndex: (field) =>
        proxy.send<void>('createFtsIndex', { collection: name, field }),

      dropFtsIndex: (field) =>
        proxy.send<void>('dropFtsIndex', { collection: name, field }),

      createVectorIndex: (field, options) => {
        if (options.indexType === 'hnsw') return Promise.reject(new Error('HNSW vector indexes are not available in the browser (requires native threads). Use Node.js or React Native.'));
        return proxy.send<void>('createVectorIndex', {
          collection: name,
          field,
          dimensions: options.dimensions,
          metric: options.metric,
          indexType: null,
          hnswM: null,
          hnswEfConstruction: null,
        });
      },

      dropVectorIndex: (field) =>
        proxy.send<void>('dropVectorIndex', { collection: name, field }),

      upgradeVectorIndex: (_field) =>
        Promise.reject(new Error('HNSW vector indexes are not available in the browser (requires native threads). Use Node.js or React Native.')),

      listIndexes: async () => {
        const json = await proxy.send<string>('listIndexes', { collection: name });
        return JSON.parse(json);
      },

      findNearest: async (field, vector, topK, filter?) => {
        const json = await proxy.send<string>('findNearest', {
          collection: name,
          field,
          queryJson: JSON.stringify(vector),
          topK,
          filterJson: filter ? JSON.stringify(filter) : 'null',
        });
        return JSON.parse(json) as { document: T; score: number }[];
      },

      subscribe: (filter, callback, onError) =>
        nudgedPoller<T>(
          () => proxy.send<string>('find', {
            collection: name, filterJson: filter ? s(filter) : 'null',
          }),
          callback,
          onError,
        ),

      subscribeAggregate: <R extends Document = Document>(
        pipeline: AggregatePipeline<T>,
        callback: (docs: R[]) => void,
        onError?: (error: unknown) => void,
      ) =>
        nudgedPoller<R>(
          () => proxy.send<string>('aggregate', {
            collection: name, pipelineJson: s(pipeline),
          }),
          callback,
          onError,
        ),
    };
    return opts ? applySchema(wrapped, opts) : wrapped;
  }

  /**
   * The worker adapter's live-query poller: a 300 ms tick, plus a BroadcastChannel
   * "nudge" so a write in another tab re-runs the query immediately rather than
   * waiting out the tick.
   *
   * Shared by `subscribe` (a filter) and `subscribeAggregate` (a pipeline) — they
   * differ only in which worker op produces the JSON, so `fetchJson` is the seam.
   */
  function nudgedPoller<R extends Document>(
    fetchJson: () => Promise<string>,
    callback: (docs: R[]) => void,
    onError?: (error: unknown) => void,
  ): () => void {
    let active = true;
    // Must start empty (not '[]') so the first snapshot is always delivered —
    // otherwise an initially-empty collection never fires the callback and
    // useFind stays in loading state forever.
    let lastJson = '';
    let timer: ReturnType<typeof setTimeout> | null = null;
    let running = false;
    let rerun = false;

    const poll = async () => {
      if (!active) return;
      if (running) { rerun = true; return; }
      running = true;
      // Cancel any pending tick — we're running now (either nudged or ticked).
      if (timer !== null) { clearTimeout(timer); timer = null; }
      try {
        const json = await fetchJson();
        if (!active) return;
        if (json !== lastJson) {
          lastJson = json;
          callback(JSON.parse(json) as R[]);
        }
      } catch (error) {
        if (active) onError?.(error);
      } finally {
        running = false;
      }
      // Schedule the next polling tick (fallback when BroadcastChannel is
      // unavailable, and for same-tab writes).
      if (active) {
        if (rerun) { rerun = false; void poll(); }
        else timer = setTimeout(poll, 300);
      }
    };

    // Register with the BroadcastChannel nudge set so cross-tab writes
    // immediately re-trigger this poller.
    nudgeCallbacks.add(poll);
    poll();
    return () => {
      active = false;
      nudgeCallbacks.delete(poll);
      if (timer !== null) { clearTimeout(timer); timer = null; }
    };
  }

  // Not annotated `: TalaDB` so the extra `listCollectionNames` (needed by the
  // internal SyncHandle, not part of the public interface) doesn't trip the
  // excess-property check. Structurally still a TalaDB.
  const handle = {
    collection: <T extends Document>(name: string, opts?: CollectionOptions<T>) => wrapCollection<T>(name, opts),
    compact: () => proxy.send<void>('compact'),
    flush: async () => { await proxy.send<void>('flush'); },
    syncStatus: async () => JSON.parse(await proxy.send<string>('syncStatus')) as { pending: number; dropped: number; failed: number },
    flushSync: (timeoutMs = 5000) => proxy.send<boolean>('flushSync', { timeoutMs }),
    close: async () => {
      channel?.close();
      try {
        await proxy.send<void>('close');
      } finally {
        worker.terminate();
        proxy.abort(new Error('taladb worker closed'));
      }
    },
    // All engine work (export scan, LWW merge) runs inside the worker, off the
    // main thread — a sync pass never blocks rendering, whatever its size.
    exportChanges: (collections: string[], sinceMs: number) =>
      proxy.send<string>('exportChangeset', { collectionsJson: JSON.stringify(collections), sinceMs }),
    importChanges: (changeset: string) =>
      proxy.send<number>('importChangeset', { changesetJson: changeset }),
    importChangesValidated: async (changeset: string, schemasJson: string) =>
      JSON.parse(await proxy.send<string>('importChangesetValidated', { changesetJson: changeset, schemasJson })) as {
        applied: number;
        skipped: number;
        quarantined: number;
      },
    listCollectionNames: async () =>
      JSON.parse(await proxy.send<string>('listCollections')) as string[],
    quarantined: async <T extends Document = Document>(collection: string) =>
      JSON.parse(await proxy.send<string>('quarantined', { collection })) as QuarantinedDocument<T>[],
    sync: (adapter: SyncAdapter, options: SyncOptions) =>
      runSync(handle as unknown as SyncHandle, adapter, options, syncSchemas),
  };
  if (migrations?.length) {
    // Version accessors run inside the worker, alongside the engine, so the
    // migration bodies (which write through the same worker) stay consistent.
    await runMigrations(
      handle,
      async () => proxy.send<number>('userVersion'),
      async (v) => {
        await proxy.send<null>('setUserVersion', { version: v });
      },
      migrations,
    );
  }
  return handle satisfies TalaDB;
}

// ============================================================
// Node.js adapter (wraps @taladb/node native module)
// ============================================================

async function createNodeDB(
  dbName: string,
  config?: TalaDbConfig,
  passphrase?: string,
  migrations?: Migration[],
): Promise<TalaDB> {
  // @taladb/node ships platform-specific .node binaries
  // eslint-disable-next-line @typescript-eslint/ban-ts-comment
  // @ts-ignore — types generated by `napi build`; not available until package is built
  const native = await import('@taladb/node');
  // napi-rs normalizes the Rust struct `TalaDBNode` to the JS class `TalaDbNode`;
  // accept both so a future regeneration can't silently break openDB again.
  const TalaDBNode = (native as Record<string, any>).TalaDbNode ?? (native as Record<string, any>).TalaDBNode;
  if (!TalaDBNode) throw new Error('@taladb/node loaded but exports no TalaDbNode class — rebuild the native module');
  const configJson = config !== undefined ? JSON.stringify(config) : null;
  const db = TalaDBNode.open(dbName, configJson, passphrase ?? null);

  // Per-collection sync schemas, registered as collections are opened, so
  // `db.sync()` can validate pulled documents ("validate, never cast").
  const syncSchemas: Record<string, SyncSchema> = {};

  function wrapCollection<T extends Document>(name: string, opts?: CollectionOptions<T>): Collection<T> {
    if (opts?.syncSchema) syncSchemas[name] = opts.syncSchema;
    const col = db.collection(name);
    // Prefer the *Async native variants (added in 0.8.1): they run on the
    // libuv thread pool instead of blocking the JS event loop. Fall back to
    // the sync calls when running against an older prebuilt .node binary.
    const wrapped: Collection<T> = {
      insert: async (doc) =>
        col.insertAsync ? col.insertAsync(doc as Record<string, unknown>) : col.insert(doc as Record<string, unknown>),
      insertMany: async (docs) =>
        col.insertManyAsync ? col.insertManyAsync(docs as Record<string, unknown>[]) : col.insertMany(docs as Record<string, unknown>[]),
      replaceManyWithIds: async (docs, origin = 'local') =>
        col.replaceManyWithIdsAsync
          ? col.replaceManyWithIdsAsync(docs as Record<string, unknown>[], origin)
          : col.replaceManyWithIds(docs as Record<string, unknown>[], origin),
      deleteManyWithIds: async (ids, origin = 'local') =>
        col.deleteManyWithIdsAsync
          ? col.deleteManyWithIdsAsync(ids, origin)
          : col.deleteManyWithIds(ids, origin),
      find: async (filter?) =>
        col.findAsync ? col.findAsync(filter ?? null) : col.find(filter ?? null),
      findOne: async (filter) => col.findOne(filter) ?? null,
      updateOne: async (filter, update) =>
        col.updateOneAsync ? col.updateOneAsync(filter, update) : col.updateOne(filter, update),
      updateMany: async (filter, update) =>
        col.updateManyAsync ? col.updateManyAsync(filter, update) : col.updateMany(filter, update),
      deleteOne: async (filter) =>
        col.deleteOneAsync ? col.deleteOneAsync(filter) : col.deleteOne(filter),
      deleteMany: async (filter) =>
        col.deleteManyAsync ? col.deleteManyAsync(filter) : col.deleteMany(filter),
      count: async (filter?) => col.count(filter ?? null),
      aggregate: async <R extends Document = Document>(pipeline: AggregatePipeline<T>): Promise<R[]> =>
        col.aggregate(pipeline as never) as R[],
      createIndex: async (field) => col.createIndex(field),
      dropIndex: async (field) => col.dropIndex(field),
      createCompoundIndex: async (fields) => col.createCompoundIndex(fields as string[]),
      dropCompoundIndex: async (fields) => col.dropCompoundIndex(fields as string[]),
      createFtsIndex: async (field) => col.createFtsIndex(field),
      dropFtsIndex: async (field) => col.dropFtsIndex(field),
      createVectorIndex: async (field, options) =>
        col.createVectorIndex(field, options.dimensions, options.metric ?? null, options.indexType ?? null, options.hnswM ?? null, options.hnswEfConstruction ?? null),
      dropVectorIndex: async (field) => col.dropVectorIndex(field),
      upgradeVectorIndex: async (field) => col.upgradeVectorIndex(field),
      listIndexes: async () => {
        const json = col.listIndexes() as string;
        return JSON.parse(json);
      },
      findNearest: async (field, vector, topK, filter?) => {
        const raw = await col.findNearest(field, vector, topK, filter ?? null) as { document: T; score: number }[];
        return raw;
      },
      subscribe: (filter, callback, onError) =>
        makePoller(async () => col.find(filter ?? null) as T[], callback, onError),
      subscribeAggregate: <R extends Document = Document>(
        pipeline: AggregatePipeline<T>,
        callback: (docs: R[]) => void,
        onError?: (error: unknown) => void,
      ) => makePoller(async () => wrapped.aggregate<R>(pipeline), callback, onError),
    };
    return opts ? applySchema(wrapped, opts) : wrapped;
  }

  // Not annotated `: TalaDB` so the extra `listCollectionNames` (needed by the
  // internal SyncHandle, not part of the public interface) doesn't trip the
  // excess-property check. Structurally still a TalaDB.
  const handle = {
    collection: <T extends Document>(name: string, opts?: CollectionOptions<T>) => wrapCollection<T>(name, opts),
    compact: async () => db.compact(),
    // Releases the native file handle/lock (no-op on older .node binaries).
    close: async () => db.close?.(),
    flush: db.flush ? async () => { db.flush(); } : undefined,
    exportChanges: async (collections: string[], sinceMs: number) => db.exportChanges(sinceMs, collections),
    importChanges: async (changeset: string) => db.importChanges(changeset),
    // Feature-detected: only present when the loaded .node binary supports it,
    // so older prebuilt binaries fall back to plain importChanges.
    importChangesValidated: db.importChangesValidated
      ? async (changeset: string, schemasJson: string) =>
          db.importChangesValidated(changeset, schemasJson) as {
            applied: number;
            skipped: number;
            quarantined: number;
          }
      : undefined,
    listCollectionNames: async () => db.listCollectionNames(),
    quarantined: async <T extends Document = Document>(collection: string) =>
      (db.quarantined ? db.quarantined(collection) : []) as QuarantinedDocument<T>[],
    sync: (adapter: SyncAdapter, options: SyncOptions) =>
      runSync(handle as unknown as SyncHandle, adapter, options, syncSchemas),
  };
  if (migrations?.length) {
    if (typeof db.userVersion !== 'function' || typeof db.setUserVersion !== 'function') {
      throw new Error('openDB({ migrations }) requires @taladb/node ≥ 0.9.2 — rebuild the native module');
    }
    await runMigrations(
      handle,
      async () => db.userVersion(),
      async (v) => db.setUserVersion(v),
      migrations,
    );
  }
  return handle satisfies TalaDB;
}

// ============================================================
// React Native adapter (wraps JSI HostObject installed by @taladb/react-native)
// ============================================================

// The JSI HostObject is a flat API: every method takes the collection name as
// its first argument. There is no intermediate collection(name) sub-object.
interface NativeDB {
  insert(collection: string, doc: Record<string, unknown>): string;
  insertMany(collection: string, docs: Record<string, unknown>[]): string[];
  replaceManyWithIds(collection: string, docs: Record<string, unknown>[], origin: WriteOrigin): string[];
  deleteManyWithIds(collection: string, ids: string[], origin: WriteOrigin): number;
  find(collection: string, filter: Record<string, unknown>): Record<string, unknown>[];
  findOne(collection: string, filter: Record<string, unknown>): Record<string, unknown> | null;
  updateOne(collection: string, filter: Record<string, unknown>, update: Record<string, unknown>): boolean;
  updateMany(collection: string, filter: Record<string, unknown>, update: Record<string, unknown>): number;
  deleteOne(collection: string, filter: Record<string, unknown>): boolean;
  deleteMany(collection: string, filter: Record<string, unknown>): number;
  count(collection: string, filter: Record<string, unknown>): number;
  aggregate(collection: string, pipeline: unknown[]): Record<string, unknown>[];
  createIndex(collection: string, field: string): void;
  dropIndex(collection: string, field: string): void;
  createCompoundIndex(collection: string, fields: string[]): void;
  dropCompoundIndex(collection: string, fields: string[]): void;
  createFtsIndex(collection: string, field: string): void;
  dropFtsIndex(collection: string, field: string): void;
  createVectorIndex(collection: string, field: string, dimensions: number, opts?: Record<string, unknown>): void;
  dropVectorIndex(collection: string, field: string): void;
  upgradeVectorIndex(collection: string, field: string): void;
  findNearest(collection: string, field: string, query: number[], topK: number, filter?: Record<string, unknown> | null): { document: Record<string, unknown>; score: number }[];
  compact(): void;
  close(): void;
  // Bidirectional-sync primitives. Optional: only present on binaries built
  // with the sync HostObject methods (added in 0.9.x). Absent on older
  // prebuilt native modules, in which case db.sync() falls back to a clear
  // "not available" error rather than crashing.
  exportChanges?(collectionsJson: string[], sinceMs: number): string;
  importChanges?(changeset: string): number;
  listCollectionNames?(): string[];
  // Validate-on-import (JSI HostObject 0.9.2+). Feature-detected so `db.sync()`
  // falls back to unvalidated import on native modules that predate them.
  importChangesValidated?(changeset: string, schemasJson: string): {
    applied: number;
    skipped: number;
    quarantined: number;
  };
  quarantined?(collection: string): unknown[];
  // Migration version accessors (openDB({ migrations })). Present once the JSI
  // HostObject exposes them; feature-detected so older binaries throw a clear
  // "not available" error instead of silently skipping migrations.
  userVersion?(): number;
  setUserVersion?(version: number): void;
  // Force batched (eventual) writes durable. Feature-detected (JSI 0.9.2+).
  flush?(): void;
}

async function createNativeDB(_dbName: string, migrations?: Migration[]): Promise<TalaDB> {
  // The JSI HostObject is installed by @taladb/react-native's TurboModule
  // at app startup via TalaDBModule.initialize(dbName).
  // After that, it is available at globalThis.__TalaDB__.
  const maybeNative = (globalThis as Record<string, unknown>).__TalaDB__ as NativeDB | undefined;
  if (!maybeNative) {
    throw new Error(
      '@taladb/react-native JSI HostObject not found. ' +
      'Did you call TalaDBModule.initialize() in your app entry point?'
    );
  }
  const native: NativeDB = maybeNative;

  // Per-collection sync schemas, registered as collections are opened, so
  // `db.sync()` validates pulled documents ("validate, never cast").
  const syncSchemas: Record<string, SyncSchema> = {};

  function wrapCollection<T extends Document>(name: string, opts?: CollectionOptions<T>): Collection<T> {
    if (opts?.syncSchema) syncSchemas[name] = opts.syncSchema;
    const wrapped: Collection<T> = {
      insert: async (doc) => native.insert(name, doc as Record<string, unknown>),
      insertMany: async (docs) => native.insertMany(name, docs as Record<string, unknown>[]),
      replaceManyWithIds: async (docs, origin = 'local') =>
        native.replaceManyWithIds(name, docs as Record<string, unknown>[], origin),
      deleteManyWithIds: async (ids, origin = 'local') =>
        native.deleteManyWithIds(name, ids, origin),
      find: async (filter?) => native.find(name, filter ?? {}) as T[],
      findOne: async (filter) => native.findOne(name, filter ?? {}) as T | null,
      updateOne: async (filter, update) => native.updateOne(name, filter, update),
      updateMany: async (filter, update) => native.updateMany(name, filter, update),
      deleteOne: async (filter) => native.deleteOne(name, filter),
      deleteMany: async (filter) => native.deleteMany(name, filter),
      count: async (filter?) => native.count(name, filter ?? {}),
      aggregate: async <R extends Document = Document>(pipeline: AggregatePipeline<T>): Promise<R[]> =>
        native.aggregate(name, pipeline as unknown[]) as R[],
      createIndex: async (field) => native.createIndex(name, field),
      dropIndex: async (field) => native.dropIndex(name, field),
      createCompoundIndex: async (fields) => native.createCompoundIndex(name, fields as string[]),
      dropCompoundIndex: async (fields) => native.dropCompoundIndex(name, fields as string[]),
      createFtsIndex: async (field) => native.createFtsIndex(name, field),
      dropFtsIndex: async (field) => native.dropFtsIndex(name, field),
      createVectorIndex: async (field, options) => {
        const opts: Record<string, unknown> = {};
        if (options.metric) opts.metric = options.metric;
        if (options.hnswM || options.hnswEfConstruction) {
          opts.hnsw = { m: options.hnswM, efConstruction: options.hnswEfConstruction };
        }
        return native.createVectorIndex(name, field, options.dimensions, opts);
      },
      dropVectorIndex: async (field) => native.dropVectorIndex(name, field),
      upgradeVectorIndex: async (field) => native.upgradeVectorIndex(name, field),
      // The JSI HostObject does not expose index introspection yet; return a
      // correctly-shaped empty result rather than `{}` cast to the interface.
      listIndexes: async (): Promise<CollectionIndexInfo> => ({ btree: [], fts: [], vector: [] }),
      findNearest: async (field, vector, topK, filter?) => {
        const raw = native.findNearest(name, field, vector, topK, filter ?? null);
        return raw as { document: T; score: number }[];
      },
      subscribe: (filter, callback, onError) =>
        makePoller(async () => native.find(name, filter ?? {}) as T[], callback, onError),
      subscribeAggregate: <R extends Document = Document>(
        pipeline: AggregatePipeline<T>,
        callback: (docs: R[]) => void,
        onError?: (error: unknown) => void,
      ) => makePoller(async () => native.aggregate(name, pipeline as unknown[]) as R[], callback, onError),
    };
    return opts ? applySchema(wrapped, opts) : wrapped;
  }

  // Bidirectional sync is available only when the native module exposes the
  // changeset primitives (0.9.x+ JSI HostObject). Feature-detect so an older
  // prebuilt binary degrades to a clear error instead of a hard crash.
  const syncSurface =
    typeof native.exportChanges === 'function' &&
    typeof native.importChanges === 'function' &&
    typeof native.listCollectionNames === 'function'
      ? (() => {
          const handle = {
            collection: <T extends Document>(name: string, opts?: CollectionOptions<T>) => wrapCollection<T>(name, opts),
            exportChanges: async (collections: string[], sinceMs: number) => native.exportChanges!(collections, sinceMs),
            importChanges: async (changeset: string) => native.importChanges!(changeset),
            // Feature-detected: present on 0.9.2+ JSI HostObjects; when absent,
            // runSync falls back to unvalidated importChanges.
            importChangesValidated: native.importChangesValidated
              ? async (changeset: string, schemasJson: string) => native.importChangesValidated!(changeset, schemasJson)
              : undefined,
            listCollectionNames: async () => native.listCollectionNames!(),
            sync: (adapter: SyncAdapter, options: SyncOptions) =>
              runSync(handle as unknown as SyncHandle, adapter, options, syncSchemas),
          };
          return {
            exportChanges: handle.exportChanges,
            importChanges: handle.importChanges,
            sync: handle.sync,
          };
        })()
      : unsupportedSync('react-native');

  const handle: TalaDB = {
    collection: <T extends Document>(name: string, opts?: CollectionOptions<T>) => wrapCollection<T>(name, opts),
    compact: async () => native.compact(),
    close: async () => native.close(),
    flush: native.flush ? async () => { native.flush!(); } : undefined,
    quarantined: native.quarantined
      ? async <T extends Document = Document>(collection: string) =>
          native.quarantined!(collection) as QuarantinedDocument<T>[]
      : undefined,
    ...syncSurface,
  };
  if (migrations?.length) {
    // Feature-detected: the JSI HostObject exposes these once the native glue
    // ships. Until then, fail loudly rather than silently skip migrations.
    if (typeof native.userVersion !== 'function' || typeof native.setUserVersion !== 'function') {
      throw new Error(
        'openDB({ migrations }) is not available on this @taladb/react-native binary yet ' +
          '(the JSI HostObject does not expose userVersion/setUserVersion). Update the native module.',
      );
    }
    await runMigrations(
      handle,
      async () => native.userVersion!(),
      async (v) => native.setUserVersion!(v),
      migrations,
    );
  }
  return handle;
}

// ============================================================
// Public entry point
// ============================================================

/**
 * A single application schema migration, run once at `openDB` when its
 * `version` is greater than the database's stored migration version.
 */
export interface Migration {
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
export async function runMigrations(
  db: TalaDB,
  getVersion: () => Promise<number>,
  setVersion: (v: number) => Promise<void>,
  migrations: Migration[],
): Promise<void> {
  const sorted = [...migrations].sort((a, b) => a.version - b.version);
  for (let i = 0; i < sorted.length; i++) {
    const v = sorted[i].version;
    if (!Number.isInteger(v) || v < 1) {
      throw new Error(`TalaDB migration version must be a positive integer, got ${v}`);
    }
    if (i > 0 && v === sorted[i - 1].version) {
      throw new Error(`TalaDB duplicate migration version ${v}`);
    }
  }
  const current = await getVersion();
  for (const m of sorted) {
    if (m.version <= current) continue;
    await m.up(db);
    await setVersion(m.version);
  }
}

/** Options for `openDB`. */
export interface OpenDBOptions {
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
export async function openDB(dbName = 'taladb.db', options?: OpenDBOptions): Promise<TalaDB> {
  if (options?.passphrase !== undefined && options.passphrase.length === 0) {
    throw new Error('TalaDB encryption passphrase must not be empty');
  }
  let resolvedConfig: TalaDbConfig | undefined;
  if (options?.config !== undefined) {
    validateConfig(options.config);
    resolvedConfig = options.config;
  } else {
    resolvedConfig = await loadConfig(options?.configPath);
  }

  // A top-level `durability` option merges into config.durability (option wins).
  if (options?.durability) {
    resolvedConfig = {
      ...resolvedConfig,
      durability: { ...resolvedConfig?.durability, ...options.durability },
    };
  }

  const platform = detectPlatform();
  const migrations = options?.migrations;
  switch (platform) {
    case 'browser':
      // Browser encryption is applied inside the OPFS worker (AES-GCM-256, salt
      // in an OPFS sidecar). The worker fails closed if OPFS is unavailable, so
      // an encrypted DB is never silently downgraded to a plaintext fallback.
      return createBrowserDB(dbName, resolvedConfig, options?.passphrase, migrations);
    case 'react-native':
      if (options?.passphrase !== undefined) {
        throw new Error('On React Native, pass the passphrase in the config JSON to TalaDBModule.initialize(); refusing to assume the already-open native database is encrypted');
      }
      return createNativeDB(dbName, migrations);
    case 'node':         return createNodeDB(dbName, resolvedConfig, options?.passphrase, migrations);
  }
}
