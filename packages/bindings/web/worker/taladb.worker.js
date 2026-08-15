/**
 * TalaDB Worker
 *
 * Runs as a Dedicated Worker. Each tab spawns its own worker instance.
 * Multi-tab write safety is provided by the Web Locks API — only one worker
 * holds the exclusive lock on the OPFS file at a time. Other workers queue up
 * and acquire the lock automatically when the current holder closes.
 *
 * Why DedicatedWorker (not SharedWorker)?
 * ----------------------------------------
 * createSyncAccessHandle() — required for synchronous OPFS I/O — is only
 * available in DedicatedWorkerGlobalScope per the WHATWG File System spec.
 * SharedWorker cannot call it; Chrome throws "is not a function".
 *
 * Why Web Locks?
 * --------------
 * Without coordination, two tabs opening the same OPFS file would race.
 * navigator.locks.request() gives us an exclusive named lock. The first tab
 * acquires it immediately; subsequent tabs block until the holder's worker is
 * terminated (tab closed / navigated) or db.close() is called explicitly.
 * If Web Locks is unavailable the worker opens the file directly and logs a
 * warning (safe for single-tab use).
 *
 * Message protocol
 * ----------------
 * Request  → { id: number, op: string, ...args }
 * Response → { id: number, result: unknown }
 *          | { id: number, error: string }
 *
 * Supported ops
 * -------------
 * init          { dbName }
 * insert        { collection, docJson }
 * insertMany    { collection, docsJson }
 * find          { collection, filterJson }
 * findOne       { collection, filterJson }
 * updateOne     { collection, filterJson, updateJson }
 * updateMany    { collection, filterJson, updateJson }
 * deleteOne     { collection, filterJson }
 * deleteMany    { collection, filterJson }
 * count         { collection, filterJson }
 * aggregate     { collection, pipelineJson }        → JSON array of result docs
 * createIndex   { collection, field }
 * dropIndex     { collection, field }
 * createFtsIndex    { collection, field }
 * dropFtsIndex      { collection, field }
 * listIndexes       { collection }  → JSON { btree, fts, vector }
 * createVectorIndex { collection, field, dimensions, metric?, indexType?, hnswM?, hnswEfConstruction? }
 * dropVectorIndex   { collection, field }
 * upgradeVectorIndex { collection, field }
 * findNearest       { collection, field, queryJson, topK, filterJson? }
 * searchText        { collection, field, query, topK, filterJson?, optionsJson? }
 * hybridSearch      { collection, textField, text, vectorField, vectorJson, topK, filterJson?, optionsJson? }
 * listCollections   {}                             → JSON string[]
 * compact           {}                             → null
 * close             {}
 *
 * Multi-tab live queries (BroadcastChannel)
 * -----------------------------------------
 * When a write op (insert/insertMany/updateOne/updateMany/deleteOne/deleteMany) commits, the worker
 * posts a `"taladb:changed"` message on a BroadcastChannel named
 * `"taladb:<dbName>"`.  Other tabs listening on the same channel re-trigger
 * their active `subscribe()` pollers immediately, bypassing the 300 ms tick.
 *
 * Cross-tab write forwarding
 * --------------------------
 * A tab that cannot take the OPFS lock runs an in-memory database and does NOT
 * publish the IndexedDB snapshot, so it has no durable storage of its own.
 * After every write it forwards the change on the BroadcastChannel as
 * `{ type: 'taladb:tab-write', … }`; the tab holding the lock applies it and
 * runs the normal onWriteCommitted() path, so every tab is notified and the
 * OPFS file stays authoritative.
 *
 * Inserts forward whole documents with their `_id`s (upserted by id, so an id
 * the application already holds stays valid); filter-based updates and deletes
 * forward the filter and update, re-evaluated by the primary against
 * authoritative data. Ordering is arrival order at the primary — two tabs on
 * one device share a clock, so there is no skew to arbitrate.
 *
 * The primary acknowledges each forwarded write. Unacknowledged writes are kept
 * so that a tab promoted to owner (below) can replay exactly the ones that were
 * never persisted.
 *
 * Lock takeover
 * -------------
 * A tab that loses the OPFS lock queues for it in the background. When the
 * holder closes its tab or calls `db.close()`, the lock passes to a waiting tab,
 * which opens the OPFS file, adopts it as authoritative, and replays its own
 * unacknowledged writes. Without this, closing the tab that owned the file left
 * every other tab writing into memory that nothing would ever persist.
 *
 * IndexedDB fallback (no OPFS)
 * ----------------------------
 * When OPFS is unavailable (cross-origin iframes, Firefox without storage
 * access) the worker opens an in-memory database seeded from the last snapshot
 * stored in IndexedDB.  After every write it flushes a new snapshot back to
 * IndexedDB so data survives page reloads.
 */

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/** @type {import('../pkg/taladb_web').WorkerDB | null} */
let db = null;

/**
 * WorkerDB constructor — hoisted to module scope so snapshot reloads in
 * IDB-fallback mode can call WorkerDB.openWithSnapshot without re-importing.
 * @type {typeof import('../pkg/taladb_web').WorkerDB | null}
 */
let WorkerDB = null;

/**
 * Set to true by the BroadcastChannel listener (fallback mode) when the
 * primary tab commits a write. Cleared at the start of the next dispatch.
 * @type {boolean}
 */
let snapshotDirty = false;

/**
 * Resolve function set when a fallback tab is waiting for the primary tab to
 * export a snapshot via BroadcastChannel. Cleared once resolved or timed out.
 * @type {(() => void) | null}
 */
let pendingSnapshotResolve = null;

/**
 * Deduplicates concurrent init calls for the same dbName within one worker.
 * @type {Map<string, Promise<void>>}
 */
const initPromises = new Map();

/**
 * Identifies this worker on the BroadcastChannel, so a forwarded write's
 * acknowledgement can be routed back to the tab that sent it.
 */
const TAB_ID = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;

/**
 * Forwarded writes this tab has sent but the primary has not acknowledged.
 *
 * A transient tab's writes live only in its own RAM until the primary applies
 * them. If the primary goes away — the tab holding the OPFS lock is closed —
 * nothing acknowledges them, and they used to be lost outright. Keeping them
 * here lets `promoteToPrimary` replay exactly the writes that never landed,
 * without replaying any that did (a replayed `$inc` would double-count).
 * @type {Map<number, object>}
 */
const unackedWrites = new Map();
let writeSeq = 0;

/** Bound on `unackedWrites`, so an absent primary cannot grow it without limit. */
const MAX_UNACKED = 1000;

/**
 * Per-collection offsets keeping `writeGeneration` monotonic across the
 * database instance swaps a snapshot reload performs.
 *
 * The generation is an in-memory counter owned by the `Database` instance, so
 * reloading a snapshot resets it to zero. Live-query pollers treat "same
 * generation as last tick" as "nothing changed and the query can be skipped" —
 * and a reload only ever happens *because* another tab wrote. Returning the raw
 * counter therefore made a fallback tab's live queries go permanently silent:
 * every reload put the generation back to the value the poller had already seen.
 * @type {Map<string, number>}
 */
const generationBase = new Map();
/** Last raw engine generation observed per collection, for the offset above. */
const generationRaw = new Map();

/** Fold the outgoing instance's generations into the offsets, before a swap. */
function carryGenerationsForward() {
  for (const [collection, raw] of generationRaw) {
    // `+ 1` so the next reading differs even when the new instance starts at
    // the same raw value the old one ended on.
    generationBase.set(collection, (generationBase.get(collection) ?? 0) + raw + 1);
  }
  generationRaw.clear();
}

/**
 * WASM fetch + compile, started at module scope rather than inside `doInit`.
 *
 * The worker is spawned and its module evaluated well before the main thread's
 * `init` message arrives. Waiting for that message to *begin* downloading ~1 MB
 * of WASM left the whole spawn-and-post round trip idle; kicking it off here
 * overlaps the two. `doInit` simply awaits this.
 *
 * A rejection here is not unhandled: `doInit` awaits it and surfaces the error
 * to the caller as a failed `init`. The no-op catch keeps the runtime from
 * reporting it as unhandled in the window between module eval and that await.
 * @type {Promise<typeof import('../pkg/taladb_web')>}
 */
const wasmReady = (async () => {
  const wasm = await import(/* @vite-ignore */ '../pkg/taladb_web.js');
  await wasm.default();
  return wasm;
})();
wasmReady.catch(() => {});

/** The dbName that was successfully initialised (or is being initialised). */
let activeDbName = null;

/** The configJson passed during init (stored so snapshot reloads can use it). */
let activeConfigJson = null;

const isDev = typeof location !== 'undefined' && (location.hostname === 'localhost' || location.hostname === '127.0.0.1');
const log = isDev ? console.log.bind(console, '[TalaDB Worker]') : () => {};
const warn = isDev ? console.warn.bind(console, '[TalaDB Worker]') : () => {};

/**
 * Origin of the page that spawned this DedicatedWorker. Messages from any
 * other origin are silently dropped. Null only in edge environments where
 * `location` is unavailable, in which case the check is skipped.
 */
const WORKER_ORIGIN = typeof location !== 'undefined' ? location.origin : null;

/**
 * Resolving this releases the Web Lock and closes the sync handle.
 * Set inside doInit; called by the 'close' op or when the worker terminates.
 * @type {(() => void) | null}
 */
let releaseLock = null;

/**
 * BroadcastChannel used to notify sibling tabs of writes.
 * Created in doInit; null until the channel name is known.
 * @type {BroadcastChannel | null}
 */
let broadcastChannel = null;

/**
 * True when running in IDB-fallback mode (OPFS unavailable).
 * In this mode every write flushes a snapshot back to IndexedDB.
 * @type {boolean}
 */
let idbFallback = false;

/** Whether this worker is allowed to publish the authoritative IDB snapshot. */
let snapshotWriter = false;

/**
 * True when the database is opened with a passphrase (encrypted at rest in the
 * OPFS file). Encrypted databases NEVER write a snapshot to IndexedDB — an
 * exported snapshot is decrypted plaintext, so persisting it would defeat
 * encryption. Encrypted mode therefore requires exclusive OPFS and is
 * single-tab: the multi-tab IDB-snapshot fallback is refused, not silently
 * downgraded to plaintext.
 * @type {boolean}
 */
let encrypted = false;

// ---------------------------------------------------------------------------
// IndexedDB helpers (used only when OPFS is unavailable)
// ---------------------------------------------------------------------------

const IDB_DB_NAME = 'taladb';
const IDB_STORE = 'snapshots';
const IDB_VERSION = 1;

/** Open (or upgrade) the "taladb" IDB database and return the IDBDatabase. */
function idbOpen() {
  return new Promise((resolve, reject) => {
    const req = self.indexedDB.open(IDB_DB_NAME, IDB_VERSION);
    req.onupgradeneeded = () => {
      // Create the object store the very first time.
      if (!req.result.objectStoreNames.contains(IDB_STORE)) {
        req.result.createObjectStore(IDB_STORE);
      }
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

/**
 * Load a snapshot Uint8Array from IndexedDB for `dbName`.
 * Returns null if no snapshot is stored yet.
 * @param {string} dbName
 * @returns {Promise<Uint8Array | null>}
 */
async function idbLoadSnapshot(dbName) {
  if (!self.indexedDB) return null;
  try {
    const idb = await idbOpen();
    return new Promise((resolve) => {
      const tx = idb.transaction(IDB_STORE, 'readonly');
      const store = tx.objectStore(IDB_STORE);
      const req = store.get(dbName);
      req.onsuccess = () => resolve(req.result ?? null);
      req.onerror = () => resolve(null);
    });
  } catch {
    return null;
  }
}

/**
 * Persist a snapshot Uint8Array to IndexedDB for `dbName` (fire-and-forget).
 * @param {string} dbName
 * @param {Uint8Array} bytes
 */
async function idbSaveSnapshot(dbName, bytes) {
  if (!self.indexedDB) return;
  try {
    const idb = await idbOpen();
    await new Promise((resolve, reject) => {
      const tx = idb.transaction(IDB_STORE, 'readwrite');
      tx.objectStore(IDB_STORE).put(bytes, dbName);
      tx.oncomplete = resolve;
      tx.onerror = reject;
    });
  } catch { /* best-effort persistence — ignore failures */ }
}

// ---------------------------------------------------------------------------
// Debounced IDB snapshot
// ---------------------------------------------------------------------------

/**
 * Debounce + max-interval parameters for IDB snapshot persistence.
 *
 * On every write we notify sibling tabs immediately via BroadcastChannel,
 * but we defer the actual IDB persistence so that bulk inserts (insertMany,
 * rapid sequential inserts) only produce a single IDB write rather than one
 * per document.  A max-interval cap ensures data is never more than 5 s stale
 * in IDB even under continuous write load.
 */
/** IDB snapshot debounce, in ms. Overridable via config `durability.flush_ms`
 * at init (the OPFS/redb path uses `durability.flush_every_write` instead). */
let snapshotDebounceMs = 500;
const SNAPSHOT_MAX_INTERVAL_MS = 5000;

/** setTimeout handle for the pending debounced flush. */
let snapshotTimer = null;

/** Timestamp of the last completed IDB flush (ms since epoch). */
let lastSnapshotMs = 0;

/** Perform the IDB snapshot write and reset state. */
async function flushSnapshot() {
  clearTimeout(snapshotTimer);
  snapshotTimer = null;
  lastSnapshotMs = Date.now();
  if (db && activeDbName && snapshotWriter) {
    try {
      const bytes = db.exportSnapshot();
      await idbSaveSnapshot(activeDbName, bytes);
      broadcastChannel?.postMessage('taladb:snapshot-ready');
    } catch { /* best-effort — ignore failures */ }
  }
}

/**
 * Schedule (or immediately trigger) an IDB snapshot write.
 *
 * - If the last flush was more than SNAPSHOT_MAX_INTERVAL_MS ago, flush now.
 * - Otherwise debounce: reset the timer to fire snapshotDebounceMs from now.
 */
function scheduleSnapshot() {
  const now = Date.now();
  if (now - lastSnapshotMs > SNAPSHOT_MAX_INTERVAL_MS) {
    // Overdue — flush synchronously in the microtask queue.
    flushSnapshot().catch(() => {});
    return;
  }
  clearTimeout(snapshotTimer);
  snapshotTimer = setTimeout(() => { flushSnapshot().catch(() => {}); }, snapshotDebounceMs);
}

/** Apply `durability` config to a freshly-opened WorkerDB instance: OPFS/redb
 * durability from `flush_every_write`, and the IDB-fallback debounce from
 * `flush_ms`. Returns the instance for chaining. */
function applyDurability(inst) {
  let flushEveryWrite = true;
  try {
    const d = activeConfigJson ? JSON.parse(activeConfigJson)?.durability : null;
    if (d) {
      if (typeof d.flush_every_write === 'boolean') flushEveryWrite = d.flush_every_write;
      if (typeof d.flush_ms === 'number' && d.flush_ms >= 0) snapshotDebounceMs = d.flush_ms;
    }
  } catch { /* ignore malformed config */ }
  inst?.setDurability?.(!flushEveryWrite); // feature-detect (older WASM lacks it)
  return inst;
}

/**
 * True when this tab writes into an in-memory database that nothing else
 * persists — i.e. another tab holds the OPFS lock (`snapshotWriter === false`).
 *
 * Such a tab has no durable storage of its own: it does not own the OPFS file
 * and does not publish the IndexedDB snapshot. Every write it makes must be
 * forwarded to the tab that does, or it exists only in RAM and is discarded on
 * the next snapshot reload.
 */
function isTransientTab() {
  return idbFallback && !snapshotWriter;
}

/**
 * Forward a committed write to the tab holding the OPFS lock.
 *
 * Inserts travel as whole documents **with their `_id`s** so the primary can
 * upsert them by id — a plain re-insert would mint a new ULID and break any id
 * the application is already holding. Filter-based updates and deletes travel
 * as the filter and update themselves, and the primary re-evaluates them
 * against authoritative data, which is more accurate than resolving ids here
 * against a possibly-stale in-memory copy.
 *
 * Ordering is arrival order at the primary. Unlike the changeset merge this
 * replaces, there is no timestamp comparison — two tabs on one device share a
 * clock and an origin, so there is no skew for Last-Write-Wins to arbitrate.
 */
function forwardWriteToPrimary(op, args, result) {
  if (!isTransientTab() || !broadcastChannel || !db) return;
  try {
    let payload = null;
    if (op === 'insert' || op === 'insertMany') {
      // Read the documents back so they carry the `_id`s just assigned.
      // `insert` answers with a bare ULID, `insertMany` with a JSON array —
      // parsing the bare id as JSON threw, and the catch below turned every
      // single-document insert in a non-primary tab into a silent data loss.
      const ids = op === 'insert' ? [result] : JSON.parse(result);
      const docs = ids
        .map((id) => JSON.parse(db.findOne(args.collection, JSON.stringify({ _id: id }))))
        .filter((d) => d !== null);
      if (docs.length === 0) return;
      payload = { kind: 'upsert', collection: args.collection, docsJson: JSON.stringify(docs) };
    } else {
      payload = {
        kind: 'replay',
        op,
        collection: args.collection,
        filterJson: args.filterJson,
        updateJson: args.updateJson,
      };
    }
    // Held until the primary acknowledges, so `promoteToPrimary` can replay
    // whatever never landed. Dropping the oldest keeps a tab that has been
    // running without a primary from growing this without bound.
    const seq = ++writeSeq;
    if (unackedWrites.size >= MAX_UNACKED) {
      const oldest = unackedWrites.keys().next().value;
      unackedWrites.delete(oldest);
      warn('Unacknowledged write buffer is full — the oldest forwarded write was dropped');
    }
    unackedWrites.set(seq, payload);
    broadcastChannel.postMessage({ type: 'taladb:tab-write', origin: TAB_ID, seq, ...payload });
  } catch (err) {
    // A failed forward must not fail the caller's write — it already committed
    // locally. Surface it loudly, because the write is now at risk of loss.
    warn('Failed to forward write to the primary tab — it may not be persisted:', err);
  }
}

/**
 * Apply one forwarded write payload to the local (authoritative) database.
 * Returns the number of documents affected.
 */
function applyForwardedWrite(payload) {
  if (payload.kind === 'upsert') {
    return JSON.parse(db.upsertManyWithIds(payload.collection, payload.docsJson)).length;
  }
  if (payload.kind !== 'replay') return 0;
  switch (payload.op) {
    case 'updateOne':
      return db.updateOne(payload.collection, payload.filterJson, payload.updateJson) ? 1 : 0;
    case 'updateMany':
      return db.updateMany(payload.collection, payload.filterJson, payload.updateJson);
    case 'deleteOne':
      return db.deleteOne(payload.collection, payload.filterJson) ? 1 : 0;
    case 'deleteMany':
      return db.deleteMany(payload.collection, payload.filterJson);
    default:
      warn('Unknown forwarded write op:', payload.op);
      return 0;
  }
}

/**
 * Notify sibling tabs of a write and schedule IDB persistence.
 * Must be called after every mutating op.
 */
function onWriteCommitted(op, args, result) {
  broadcastChannel?.postMessage('taladb:changed');
  if (op) forwardWriteToPrimary(op, args, result);
  // Debounced IDB flush — keeps other tabs' fallback instances in sync via
  // BroadcastChannel + snapshotDirty reload without writing to IDB on every op.
  if (snapshotWriter) scheduleSnapshot();
}

// ---------------------------------------------------------------------------
// Dedicated Worker message handler
// ---------------------------------------------------------------------------

self.onmessage = async (e) => {
  // Reject messages from unexpected origins. In a DedicatedWorker only the
  // creating page can post messages (browser-enforced), but this guard is a
  // defence-in-depth measure in case the worker is ever repurposed.
  if (WORKER_ORIGIN && e.origin && e.origin !== WORKER_ORIGIN) {
    return;
  }
  const { id, op, ...args } = e.data;
  try {
    const result = await dispatch(op, args);
    self.postMessage({ id, result: result ?? null });
  } catch (err) {
    self.postMessage({ id, error: String(err?.message ?? err) });
  }
};

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

async function dispatch(op, args) {
  // In IDB-fallback mode, reload the snapshot before each op when the primary
  // tab has signalled a write via BroadcastChannel. This keeps reads fresh.
  if (idbFallback && snapshotDirty && op !== 'init' && db && activeDbName && WorkerDB) {
    snapshotDirty = false;
    try {
      const fresh = await idbLoadSnapshot(activeDbName);
      if (fresh) {
        // The new instance's generation counter starts from zero; fold the old
        // one's into the offsets so live-query pollers still see a change.
        carryGenerationsForward();
        db = activeConfigJson
          ? WorkerDB.openWithConfigAndSnapshot(fresh, activeConfigJson)
          : WorkerDB.openWithSnapshot(fresh);
      }
    } catch { /* ignore reload errors — stale read is acceptable */ }
  }

  if (op === 'init') {
    const { dbName, configJson, passphrase } = args;

    if (activeDbName !== null && activeDbName !== dbName) {
      throw new Error(
        `TalaDB worker already initialised for "${activeDbName}". ` +
        `Cannot open "${dbName}" in the same worker instance.`
      );
    }

    if (!initPromises.has(dbName)) {
      activeDbName = dbName;
      activeConfigJson = configJson ?? null;
      initPromises.set(dbName, doInit(dbName, configJson ?? null, passphrase ?? null));
    }
    await initPromises.get(dbName);
    return null;
  }

  if (!db) throw new Error('TalaDB worker not initialised — call init first');

  switch (op) {
    case 'insert': {
      const result = db.insert(args.collection, args.docJson);
      onWriteCommitted('insert', args, result);
      return result;
    }

    case 'insertMany': {
      const result = db.insertMany(args.collection, args.docsJson);
      onWriteCommitted('insertMany', args, result);
      return result;
    }

    case 'find':
      return db.find(args.collection, args.filterJson ?? 'null');

    case 'findOne':
      return db.findOne(args.collection, args.filterJson ?? 'null');

    case 'updateOne': {
      const result = db.updateOne(args.collection, args.filterJson, args.updateJson);
      onWriteCommitted('updateOne', args, result);
      return result;
    }

    case 'updateMany': {
      const result = db.updateMany(args.collection, args.filterJson, args.updateJson);
      onWriteCommitted('updateMany', args, result);
      return result;
    }

    case 'deleteOne': {
      const result = db.deleteOne(args.collection, args.filterJson);
      onWriteCommitted('deleteOne', args, result);
      return result;
    }

    case 'deleteMany': {
      const result = db.deleteMany(args.collection, args.filterJson);
      onWriteCommitted('deleteMany', args, result);
      return result;
    }

    case 'count':
      return db.count(args.collection, args.filterJson ?? 'null');

    // Cheap change-detection for subscribe(): one integer, no query.
    case 'writeGeneration': {
      const raw = db.writeGeneration(args.collection);
      generationRaw.set(args.collection, raw);
      return (generationBase.get(args.collection) ?? 0) + raw;
    }

    case 'aggregate':
      return db.aggregate(args.collection, args.pipelineJson ?? '[]');

    case 'createIndex':
      db.createIndex(args.collection, args.field);
      return null;

    case 'dropIndex':
      db.dropIndex(args.collection, args.field);
      return null;

    case 'createCompoundIndex':
      db.createCompoundIndex(args.collection, args.fieldsJson);
      return null;

    case 'dropCompoundIndex':
      db.dropCompoundIndex(args.collection, args.fieldsJson);
      return null;

    case 'createFtsIndex':
      db.createFtsIndex(args.collection, args.field);
      return null;

    case 'dropFtsIndex':
      db.dropFtsIndex(args.collection, args.field);
      return null;

    case 'listIndexes':
      return db.listIndexes(args.collection);

    case 'createVectorIndex':
      db.createVectorIndex(
        args.collection,
        args.field,
        args.dimensions,
        args.metric ?? null,
        args.indexType ?? null,
        args.hnswM ?? null,
        args.hnswEfConstruction ?? null,
      );
      return null;

    case 'dropVectorIndex':
      db.dropVectorIndex(args.collection, args.field);
      return null;

    case 'upgradeVectorIndex':
      db.upgradeVectorIndex(args.collection, args.field);
      return null;

    case 'findNearest':
      return db.findNearest(
        args.collection,
        args.field,
        args.queryJson,
        args.topK,
        args.filterJson ?? 'null',
      );

    case 'searchText':
      return db.searchText(
        args.collection,
        args.field,
        args.query,
        args.topK,
        args.filterJson ?? 'null',
        args.optionsJson ?? 'null',
      );

    case 'hybridSearch':
      return db.hybridSearch(
        args.collection,
        args.textField,
        args.text,
        args.vectorField,
        args.vectorJson,
        args.topK,
        args.filterJson ?? 'null',
        args.optionsJson ?? 'null',
      );

    case 'listCollections':
      return db.listCollections();

    case 'compact':
      // Compact the storage file, reclaiming freed space. No-op on IDB fallback.
      db.compact();
      return null;

    case 'flush':
      // "Save now": force batched OPFS writes durable (no-op under immediate
      // durability) and write the IDB fallback snapshot immediately.
      db.flush?.();
      await flushSnapshot();
      return null;

    case 'userVersion':
      // Current application migration version (backs openDB({ migrations })).
      return db.userVersion();

    case 'setUserVersion':
      db.setUserVersion(args.version);
      return null;

    case 'close':
      // Flush any pending debounced snapshot before releasing the lock so
      // no writes are lost when the tab closes or navigates away.
      await flushSnapshot();
      // Release the Web Lock and close the sync handle gracefully.
      if (releaseLock) { releaseLock(); releaseLock = null; }
      broadcastChannel?.close();
      broadcastChannel = null;
      // Cleared before `db`: a promotion waiting on the lock reads these to
      // decide whether this worker still wants the file.
      idbFallback = false;
      snapshotWriter = false;
      unackedWrites.clear();
      generationBase.clear();
      generationRaw.clear();
      db = null;
      return null;

    default:
      throw new Error(`Unknown op: ${op}`);
  }
}

/**
 * Load the 16-byte key-derivation salt from an OPFS sidecar file, or create it
 * on first open. The salt is not secret (it defends against precomputed-hash
 * attacks) but must be stable across opens, so it lives beside the DB file.
 * @returns {Promise<Uint8Array>} the 16-byte salt
 */
async function loadOrCreateSalt(root, saltFileName) {
  const fh = await root.getFileHandle(saltFileName, { create: true });
  const h = await fh.createSyncAccessHandle();
  try {
    const size = h.getSize();
    if (size === 16) {
      const salt = new Uint8Array(16);
      h.read(salt, { at: 0 });
      return salt;
    }
    if (size === 0) {
      const salt = new Uint8Array(16);
      self.crypto.getRandomValues(salt);
      h.write(salt, { at: 0 });
      h.flush();
      return salt;
    }
    throw new Error(`invalid TalaDB salt file (${size} bytes, expected 16)`);
  } finally {
    h.close();
  }
}

// ---------------------------------------------------------------------------
// Initialisation — load WASM, acquire lock, open OPFS file
// ---------------------------------------------------------------------------

/**
 * Wait for the OPFS lock this tab lost at open, and take over when it frees.
 *
 * ## Why a tab has to be able to take over
 *
 * A tab that loses the lock runs an in-memory copy and forwards every write to
 * the tab that owns the file. That arrangement has one failure mode, and it is
 * total: when the owning tab goes away, the survivors keep accepting writes
 * that nobody persists. Closing the last "real" tab silently turned the others
 * into scratch memory.
 *
 * ## What happens to writes made while there was no primary
 *
 * The OPFS file is authoritative on promotion — this tab's in-memory copy is
 * discarded rather than merged, because merging two divergent document sets has
 * no correct answer here. What is *not* discarded is `unackedWrites`: writes
 * the primary never acknowledged, which are therefore absent from the file.
 * Those are replayed, in order, against the freshly-opened OPFS database. A
 * write the old primary *did* apply was acknowledged and is not in the map, so
 * nothing is applied twice — which matters for `$inc`, where a double apply is
 * silently wrong rather than merely redundant.
 */
async function waitForPromotion(lockName, fileHandle, openWithOpfs) {
  try {
    await navigator.locks.request(lockName, async () => {
      // A close() between losing the race and winning the lock means this
      // worker is done — take the lock only to release it again.
      if (!idbFallback || !db || !activeDbName) return;

      let syncHandle;
      try {
        syncHandle = await fileHandle.createSyncAccessHandle();
      } catch (e) {
        warn('Promoted to OPFS owner but the sync handle could not be opened:', e);
        return;
      }

      const pending = [...unackedWrites.entries()].sort((a, b) => a[0] - b[0]);
      try {
        // The instance swap resets the engine's generation counter.
        carryGenerationsForward();
        db = openWithOpfs(syncHandle);
        idbFallback = false;
        snapshotWriter = true;
        snapshotDirty = false;
        unackedWrites.clear();
        let replayed = 0;
        for (const [, payload] of pending) {
          try {
            replayed += applyForwardedWrite(payload);
          } catch (err) {
            warn('Failed to replay a write during promotion:', err);
          }
        }
        log(
          `Promoted to OPFS owner${replayed ? ` (replayed ${replayed} unacknowledged write(s))` : ''}`,
        );
        // Tell the other tabs to re-read: this tab's data is now the file's.
        onWriteCommitted();
      } catch (e) {
        try { syncHandle.close(); } catch { /* best-effort */ }
        warn('Promotion to OPFS owner failed — staying in fallback mode:', e);
        return;
      }

      // Hold the lock for as long as this worker owns the file.
      await new Promise((res) => { releaseLock = res; });
      try { syncHandle.close(); } catch { /* best-effort */ }
      db = null;
    });
  } catch (e) {
    warn('Waiting for the OPFS lock failed:', e);
  }
}

async function doInit(dbName, configJson, passphrase = null) {
  encrypted = typeof passphrase === 'string' && passphrase.length > 0;

  const opfsAvailable = canUseOpfs();

  // Compiling ~1 MB of WASM and setting up OPFS are independent, and OPFS setup
  // is four awaited round trips (getDirectory → getFileHandle → lock →
  // createSyncAccessHandle). Starting both now lets the storage latency hide
  // behind the compile instead of following it.
  //
  // `wasmReady` was already kicked off at module scope — see its declaration.
  const opfsSetup = opfsAvailable
    ? (async () => {
        const root = await navigator.storage.getDirectory();
        const fileName = `taladb_${dbName.replaceAll(/[/\\:]/g, '_')}.redb`;
        return { root, fileName, fileHandle: await root.getFileHandle(fileName, { create: true }) };
      })().catch(() => null)
    : Promise.resolve(null);

  const wasm = await wasmReady;

  // Hoist to module scope so snapshot reloads in dispatch() can use it.
  WorkerDB = wasm.WorkerDB;

  // Open the BroadcastChannel now that we know the db name.
  if (typeof BroadcastChannel !== 'undefined') {
    broadcastChannel = new BroadcastChannel(`taladb:${dbName}`);
    broadcastChannel.onmessage = async (e) => {
      if (e.data === 'taladb:changed' && idbFallback) {
        // Wait for snapshot-ready: changed is emitted before the primary's
        // asynchronous IDB transaction has committed.
      } else if (e.data === 'taladb:request-snapshot' && !idbFallback && !encrypted && db && activeDbName) {
        // Primary (OPFS) tab: a new tab asked for a snapshot — export and save it.
        // Never for encrypted DBs: an exported snapshot is decrypted plaintext.
        try {
          const bytes = db.exportSnapshot();
          await idbSaveSnapshot(activeDbName, bytes);
          broadcastChannel.postMessage('taladb:snapshot-ready');
          log('Exported snapshot for waiting tab');
        } catch { /* ignore */ }
      } else if (e.data === 'taladb:snapshot-ready' && pendingSnapshotResolve) {
        // Fallback tab: primary tab finished saving — wake up the waiting doInit.
        const resolve = pendingSnapshotResolve;
        pendingSnapshotResolve = null;
        resolve();
      } else if (e.data === 'taladb:snapshot-ready' && idbFallback) {
        snapshotDirty = true;
      } else if (e.data?.type === 'taladb:tab-write-ack' && e.data.origin === TAB_ID) {
        // The primary applied one of our forwarded writes: it is now somebody
        // else's durable responsibility, so stop holding it for replay.
        unackedWrites.delete(e.data.seq);
      } else if (e.data?.type === 'taladb:tab-write' && db && snapshotWriter && !encrypted) {
        // Apply a write forwarded by a tab that has no durable storage of its
        // own. Guarded on `snapshotWriter` so only the tab that actually owns
        // persistence applies it — otherwise several fallback tabs would each
        // apply the same write to their private in-memory copies.
        //
        // `!encrypted`: encrypted databases are single-tab by construction, so
        // there is no legitimate forwarded write. Accepting one would let any
        // same-origin script inject documents without knowing the passphrase.
        try {
          const n = applyForwardedWrite(e.data);
          // Acknowledged whatever the count — the write has been evaluated
          // against authoritative data, and a filter that matched nothing here
          // will not match anything on a replay either. Sent before the
          // notification below so the originating tab can release it promptly.
          if (e.data.origin !== undefined) {
            broadcastChannel.postMessage({
              type: 'taladb:tab-write-ack',
              origin: e.data.origin,
              seq: e.data.seq,
            });
          }
          if (n > 0) {
            log(`Applied ${n} forwarded write(s) from another tab`);
            // No op/args: this write must not be forwarded onward. Notifying
            // and snapshotting is the whole point — it is what lets the
            // originating tab read its own write back.
            onWriteCommitted();
          }
        } catch (err) {
          warn('Failed to apply a forwarded write from another tab:', err);
        }
      }
    };
    log('BroadcastChannel opened:', `taladb:${dbName}`);
  }

  // Helpers to open DB with or without a config. The config-aware
  // constructors carry durability and encryption settings.
  function openWithSnapshot(snapshot) {
    const inst = configJson
      ? WorkerDB.openWithConfigAndSnapshot(snapshot, configJson)
      : WorkerDB.openWithSnapshot(snapshot);
    return applyDurability(inst);
  }
  // Salt for key derivation, loaded/created in an OPFS sidecar (encrypted mode
  // only). Passed to the WASM open so the derived key is stable across opens.
  let salt = null;
  function openWithOpfs(syncHandle) {
    // openWithConfigAndOpfs(handle, configJson, passphrase?, salt?)
    return applyDurability(
      WorkerDB.openWithConfigAndOpfs(syncHandle, configJson ?? null, passphrase, salt),
    );
  }

  /**
   * Open an in-memory database seeded from the last IndexedDB snapshot, and
   * keep flushing snapshots back. The degradation path whenever OPFS is
   * unusable. Never valid for encrypted databases — the snapshot is plaintext.
   */
  async function openIdbFallback(name) {
    const snapshot = await idbLoadSnapshot(name);
    db = openWithSnapshot(snapshot);
    idbFallback = true;
    snapshotWriter = true;
    if (snapshot) {
      log(`Restored from IndexedDB snapshot (${snapshot.byteLength} bytes)`);
    } else {
      log('New in-memory database — writes will be persisted to IndexedDB');
    }
  }

  // `opfsSetup` resolves to null when OPFS is unusable — either the capability
  // check said so up front, or getDirectory/getFileHandle actually failed.
  const opfs = await opfsSetup;
  if (!opfs) {
    if (encrypted) {
      throw new Error(
        'TalaDB encryption requires OPFS, which is unavailable in this browser context. ' +
        'Refusing to open — the in-memory/IndexedDB fallback cannot encrypt at rest.'
      );
    }
    warn('OPFS unavailable — falling back to IndexedDB-backed in-memory');
    await openIdbFallback(dbName);
    return;
  }

  const { root, fileName, fileHandle } = opfs;

  // Encrypted mode: the 16-byte key-derivation salt lives in an OPFS sidecar
  // file next to the DB. It is loaded/created only AFTER this tab has won the
  // exclusive lock (or determined no locking applies), so two tabs racing to
  // open never collide on the salt file's exclusive access handle.

  if (!('locks' in navigator)) {
    // Web Locks not available — open directly (single-tab safe only).
    warn('Web Locks unavailable — multi-tab write safety disabled');
    if (encrypted) salt = await loadOrCreateSalt(root, `${fileName}.salt`);
    let syncHandle;
    try {
      syncHandle = await fileHandle.createSyncAccessHandle();
    } catch (e) {
      // `createSyncAccessHandle` can exist on the prototype and still throw —
      // Firefox without storage access, a cross-origin iframe, a revoked
      // permission. The removed probe file used to surface that as "OPFS
      // unavailable"; catching it here keeps the same graceful degradation
      // rather than failing the open outright.
      if (encrypted) throw e; // the IDB fallback cannot encrypt at rest
      warn('OPFS sync access unavailable — falling back to IndexedDB:', e);
      await openIdbFallback(dbName);
      return;
    }
    try {
      db = openWithOpfs(syncHandle);
    } catch (e) {
      // A failed open (e.g. wrong passphrase) must release the exclusive OPFS
      // access handle, or every retry fails with "Access Handles cannot be
      // created" until the page reloads.
      try { syncHandle.close(); } catch { /* best-effort */ }
      throw e;
    }
    snapshotWriter = !encrypted; // encrypted DBs never write a plaintext IDB snapshot
    log(`Opened "${fileName}" via OPFS`);
    return;
  }

  // Try to acquire the exclusive OPFS lock immediately (ifAvailable).
  // If another tab already holds it, fall back to IDB snapshot right away so
  // this tab is immediately usable instead of blocking until the other tab closes.
  // The primary (OPFS) tab flushes a snapshot to IDB after every write, and
  // this tab's BroadcastChannel listener sets snapshotDirty so dispatch()
  // reloads the snapshot before the next read — keeping data fresh across tabs.
  const lockName = `taladb:${fileName}`;
  await new Promise((resolve, reject) => {
    navigator.locks.request(lockName, { ifAvailable: true }, async (lock) => {
      if (lock === null) {
        // Lock is held by another tab.
        if (encrypted) {
          // The IDB-snapshot fallback stores decrypted plaintext, so it's
          // refused for encrypted databases — they're single-tab. Reject
          // rather than downgrade.
          reject(new Error(
            'This encrypted TalaDB database is already open in another tab. ' +
            'Encrypted browser databases are single-tab (the multi-tab fallback would store plaintext).'
          ));
          return;
        }
        // Unencrypted: use IDB snapshot so this tab loads immediately.
        warn('OPFS lock held by another tab — falling back to IndexedDB snapshot (live-sync via BroadcastChannel)');
        let snapshot = await idbLoadSnapshot(dbName);

        if (!snapshot && broadcastChannel) {
          // No IDB snapshot yet — ask the primary tab to export one now.
          log('No IDB snapshot — requesting one from the primary tab...');
          const gotIt = await new Promise(res => {
            pendingSnapshotResolve = res;
            setTimeout(() => { pendingSnapshotResolve = null; res(false); }, 2000);
            broadcastChannel.postMessage('taladb:request-snapshot');
          });
          if (gotIt !== false) snapshot = await idbLoadSnapshot(dbName);
        }

        db = openWithSnapshot(snapshot ?? null);
        idbFallback = true;
        snapshotWriter = false;
        if (snapshot) {
          log(`Restored from IDB snapshot (${snapshot.byteLength} bytes)`);
        } else {
          log('No IDB snapshot yet — starting with empty in-memory database');
        }
        // Queue for the lock in the background. The holder releases it when its
        // tab closes or calls db.close(), and this tab is then promoted to the
        // OPFS file. Without this a tab that lost the race stayed transient for
        // its whole life: once the primary went away, every write it made had
        // nowhere to be persisted and was silently discarded.
        void waitForPromotion(lockName, fileHandle, openWithOpfs);
        resolve();
        return; // Do not hold the lock; returning releases it back to the queue.
      }

      // Acquired the lock — use OPFS.
      let syncHandle = null;
      try {
        if (encrypted) salt = await loadOrCreateSalt(root, `${fileName}.salt`);
        try {
          syncHandle = await fileHandle.createSyncAccessHandle();
        } catch (e) {
          // See the no-locks branch: the method can exist and still throw, and
          // that used to be caught by the probe file. Degrade to IndexedDB
          // instead of failing the open.
          if (encrypted) throw e; // the IDB fallback cannot encrypt at rest
          warn('OPFS sync access unavailable — falling back to IndexedDB:', e);
          await openIdbFallback(dbName);
          resolve();
          return; // releases the Web Lock — this tab is not the OPFS holder
        }
        db = openWithOpfs(syncHandle);
        snapshotWriter = !encrypted; // encrypted DBs never write a plaintext IDB snapshot
        log(`Opened "${fileName}" via OPFS (Web Locks)`);
        resolve(); // signal doInit complete — caller can proceed

        // Hold the lock by keeping this async callback alive.
        // Resolved by the 'close' op or when the worker is terminated.
        await new Promise(res => { releaseLock = res; });

        syncHandle.close();
        db = null;
      } catch (e) {
        // A failed open (e.g. wrong passphrase) must release the exclusive
        // OPFS access handle, or every retry fails with "Access Handles
        // cannot be created" until the page reloads. Returning from this
        // callback also releases the Web Lock.
        if (syncHandle) {
          try { syncHandle.close(); } catch { /* best-effort */ }
        }
        reject(e);
      }
    });
  });
}

// ---------------------------------------------------------------------------
// OPFS capability probe
// ---------------------------------------------------------------------------

/**
 * Whether this context can do synchronous OPFS I/O.
 *
 * This used to create a uniquely-named probe file, open a sync access handle on
 * it, close it, and delete it — four sequential OPFS round trips on every cold
 * start, immediately before the real open performed the identical sequence on
 * the actual database file and would have failed the same way. The capability
 * check below is synchronous and free; anything the probe would have caught
 * (permissions, cross-origin isolation, a non-Worker scope) surfaces from the
 * real open, which `doInit` already handles by falling back to IndexedDB.
 *
 * @returns {boolean}
 */
function canUseOpfs() {
  return (
    typeof navigator !== 'undefined' &&
    typeof navigator.storage?.getDirectory === 'function' &&
    typeof FileSystemFileHandle !== 'undefined' &&
    typeof FileSystemFileHandle.prototype?.createSyncAccessHandle === 'function'
  );
}
