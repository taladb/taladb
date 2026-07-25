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
 * compactTombstones { collection, beforeMs }       → number pruned
 * exportChangeset   { collectionsJson, sinceMs? }  → JSON changeset string
 * importChangeset   { changesetJson }              → number of applied changes
 * close             {}
 *
 * Multi-tab live queries (BroadcastChannel)
 * -----------------------------------------
 * When a write op (insert/insertMany/updateOne/updateMany/deleteOne/deleteMany) commits, the worker
 * posts a `"taladb:changed"` message on a BroadcastChannel named
 * `"taladb:<dbName>"`.  Other tabs listening on the same channel re-trigger
 * their active `subscribe()` pollers immediately, bypassing the 300 ms tick.
 *
 * Secondary-tab write propagation
 * --------------------------------
 * Fallback (non-OPFS) tabs can also make writes that need to reach the primary
 * tab's OPFS-backed database.  After every mutating op a fallback tab exports a
 * mini-changeset (all collections, since the previous sync point) and posts it
 * as `{ type: 'taladb:secondary-write', changeset }` on the BroadcastChannel.
 * The primary (OPFS) tab receives it, calls importChangeset(), and runs the
 * normal onWriteCommitted() path — so all other tabs are notified and the OPFS
 * file stays authoritative.  LWW merge handles any concurrent edits.
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
 * Per-worker-instance random token. Included in every `taladb:secondary-write`
 * BroadcastChannel message so the primary tab can reject unauthenticated
 * injections that omit a token.
 */
const SESSION_TOKEN = (typeof self.crypto !== 'undefined' && typeof self.crypto.randomUUID === 'function')
  ? self.crypto.randomUUID()
  : Array.from(self.crypto.getRandomValues(new Uint8Array(16)), b => b.toString(16).padStart(2, '0')).join('');

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

/**
 * Timestamp (ms) of the last changeset we exported and broadcast to the
 * primary tab.  Used as `sinceMs` for the next export so we only send the
 * delta, not the entire database on every write.
 * @type {number}
 */
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
 * When running as a fallback (non-OPFS) tab, export the changes made since
 * the last push and broadcast them to the primary tab for merging into OPFS.
 *
 * Uses exportChangeset with all known collections to keep the delta small.
 * The primary tab's BroadcastChannel handler calls importChangeset() and runs
 * its own onWriteCommitted(), which notifies all tabs (including this one) via
 * taladb:changed so reads stay consistent.
 */
function pushChangesetToPrimary() {
  if (!idbFallback || !db || !broadcastChannel || !activeDbName) return;
  try {
    // Collect all collection names from the current in-memory state.
    // exportChangeset accepts a JSON array of collection names.
    const collections = db.listCollections();
    // Wall-clock timestamps are not safe incremental cursors: a write can be
    // stamped before an export but commit after its snapshot. Replay is
    // idempotent under LWW and cannot skip such a write.
    const changeset = db.exportChangeset(collections, 0);
    // Only broadcast if there is actually something to send.
    const parsed = JSON.parse(changeset);
    if (parsed.length === 0) return;
    broadcastChannel.postMessage({ type: 'taladb:secondary-write', token: SESSION_TOKEN, changeset });
    log(`Broadcast ${parsed.length} change(s) to primary tab`);
  } catch (err) {
    warn('Failed to export changeset for primary tab:', err);
  }
}

/**
 * Notify sibling tabs of a write and schedule IDB persistence.
 * Must be called after every mutating op.
 */
function onWriteCommitted() {
  broadcastChannel?.postMessage('taladb:changed');
  // Fallback tab: push local changes to the primary (OPFS) tab so they are
  // merged into the authoritative database file.
  pushChangesetToPrimary();
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
      onWriteCommitted();
      return result;
    }

    case 'insertMany': {
      const result = db.insertMany(args.collection, args.docsJson);
      onWriteCommitted();
      return result;
    }

    case 'replaceManyWithIds': {
      const result = db.replaceManyWithIds(args.collection, args.docsJson, args.origin);
      onWriteCommitted();
      return result;
    }

    case 'deleteManyWithIds': {
      const result = db.deleteManyWithIds(args.collection, args.idsJson, args.origin);
      onWriteCommitted();
      return result;
    }

    case 'find':
      return db.find(args.collection, args.filterJson ?? 'null');

    case 'findOne':
      return db.findOne(args.collection, args.filterJson ?? 'null');

    case 'updateOne': {
      const result = db.updateOne(args.collection, args.filterJson, args.updateJson);
      onWriteCommitted();
      return result;
    }

    case 'updateMany': {
      const result = db.updateMany(args.collection, args.filterJson, args.updateJson);
      onWriteCommitted();
      return result;
    }

    case 'deleteOne': {
      const result = db.deleteOne(args.collection, args.filterJson);
      onWriteCommitted();
      return result;
    }

    case 'deleteMany': {
      const result = db.deleteMany(args.collection, args.filterJson);
      onWriteCommitted();
      return result;
    }

    case 'count':
      return db.count(args.collection, args.filterJson ?? 'null');

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

    case 'syncStatus':
      return db.syncStatus();

    case 'flushSync': {
      const deadline = Date.now() + (args.timeoutMs ?? 5000);
      while (db.syncPending() > 0 && Date.now() < deadline) {
        await new Promise(resolve => setTimeout(resolve, 20));
      }
      return db.syncPending() === 0;
    }

    case 'compactTombstones':
      // Prune tombstones older than beforeMs from a collection.
      // Returns the count of tombstones removed.
      return db.compactTombstones(args.collection, args.beforeMs ?? 0);

    case 'exportChangeset':
      // Export a LWW changeset for the given collections since sinceMs.
      // Returns a JSON string the caller can POST to a sync server.
      return db.exportChangeset(args.collectionsJson, args.sinceMs ?? 0);

    case 'importChangeset': {
      // Apply a remote changeset (JSON string from sync server) using LWW.
      // Triggers onWriteCommitted so multi-tab peers get notified.
      const applied = db.importChangeset(args.changesetJson);
      if (applied > 0) onWriteCommitted();
      return applied;
    }

    case 'importChangesetValidated': {
      // Tolerant validated import: normalize/skip/quarantine per schema, LWW.
      // Returns a JSON string { applied, skipped, quarantined }.
      const reportJson = db.importChangesetValidated(args.changesetJson, args.schemasJson);
      const report = JSON.parse(reportJson);
      // Quarantined documents are written to the quarantine table, so a batch
      // that only quarantines still dirties the database. Without this, the IDB
      // fallback would never snapshot them and they would be lost on reload —
      // the opposite of the "never dropped on the floor" guarantee.
      if ((report.applied ?? 0) > 0 || (report.quarantined ?? 0) > 0) onWriteCommitted();
      return reportJson;
    }

    case 'quarantined':
      // JSON array of documents set aside by a validated import.
      return db.quarantined(args.collection);

    case 'userVersion':
      // Current application migration version (backs openDB({ migrations })).
      return db.userVersion();

    case 'setUserVersion':
      db.setUserVersion(args.version);
      return null;

    case 'close':
      // Give accepted HTTP push events a bounded opportunity to finish.
      {
        const deadline = Date.now() + 5000;
        while (db.syncPending() > 0 && Date.now() < deadline) {
          await new Promise(resolve => setTimeout(resolve, 20));
        }
      }
      // Flush any pending debounced snapshot before releasing the lock so
      // no writes are lost when the tab closes or navigates away.
      await flushSnapshot();
      // Release the Web Lock and close the sync handle gracefully.
      if (releaseLock) { releaseLock(); releaseLock = null; }
      broadcastChannel?.close();
      broadcastChannel = null;
      idbFallback = false;
      snapshotWriter = false;
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

async function doInit(dbName, configJson, passphrase = null) {
  const wasm = await import(/* @vite-ignore */ '../pkg/taladb_web.js');
  await wasm.default();

  // Hoist to module scope so snapshot reloads in dispatch() can use it.
  WorkerDB = wasm.WorkerDB;

  encrypted = typeof passphrase === 'string' && passphrase.length > 0;

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
      } else if (e.data?.type === 'taladb:secondary-write' && typeof e.data.token === 'string' && e.data.token.length > 0 && db && !encrypted) {
        // `!encrypted`: encrypted databases are single-tab, so there are no
        // legitimate secondary-tab writes — accepting them would let any
        // same-origin script inject documents into an encrypted DB without
        // knowing the passphrase.
        // Merge peer writes in every mode. This also makes multiple tabs
        // converge when OPFS is entirely unavailable and every tab uses IDB.
        // The token requirement rejects unauthenticated injection attempts that
        // omit the SESSION_TOKEN field (not a guarantee against same-origin attackers
        // who observe the token, but defence-in-depth against naive injection).
        try {
          const applied = db.importChangeset(e.data.changeset);
          if (applied > 0) {
            log(`Merged ${applied} change(s) from secondary tab`);
            onWriteCommitted();
          }
        } catch (err) {
          warn('Failed to import secondary-tab changeset:', err);
        }
      }
    };
    log('BroadcastChannel opened:', `taladb:${dbName}`);
  }

  // Helpers to open DB with or without sync config.
  // Uses the config-aware constructors when configJson is provided so that
  // HTTP push sync is wired up from the first write.
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

  const opfsAvailable = await checkOpfs();
  if (!opfsAvailable) {
    if (encrypted) {
      throw new Error(
        'TalaDB encryption requires OPFS, which is unavailable in this browser context. ' +
        'Refusing to open — the in-memory/IndexedDB fallback cannot encrypt at rest.'
      );
    }
    warn('OPFS unavailable — falling back to IndexedDB-backed in-memory');
    const snapshot = await idbLoadSnapshot(dbName);
    db = openWithSnapshot(snapshot);
    idbFallback = true;
    snapshotWriter = true;
    if (snapshot) {
      log(`Restored from IndexedDB snapshot (${snapshot.byteLength} bytes)`);
    } else {
      log('New in-memory database — writes will be persisted to IndexedDB');
    }
    return;
  }

  const root = await navigator.storage.getDirectory();
  const fileName = `taladb_${dbName.replaceAll(/[/\\:]/g, '_')}.redb`;
  const fileHandle = await root.getFileHandle(fileName, { create: true });

  // Encrypted mode: the 16-byte key-derivation salt lives in an OPFS sidecar
  // file next to the DB. It is loaded/created only AFTER this tab has won the
  // exclusive lock (or determined no locking applies), so two tabs racing to
  // open never collide on the salt file's exclusive access handle.

  if (!('locks' in navigator)) {
    // Web Locks not available — open directly (single-tab safe only).
    warn('Web Locks unavailable — multi-tab write safety disabled');
    if (encrypted) salt = await loadOrCreateSalt(root, `${fileName}.salt`);
    const syncHandle = await fileHandle.createSyncAccessHandle();
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
        resolve();
        return; // Do not hold the lock; returning releases it back to the queue.
      }

      // Acquired the lock — use OPFS.
      let syncHandle = null;
      try {
        if (encrypted) salt = await loadOrCreateSalt(root, `${fileName}.salt`);
        syncHandle = await fileHandle.createSyncAccessHandle();
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

async function checkOpfs() {
  try {
    const root = await navigator.storage.getDirectory();
    // Probe createSyncAccessHandle — only available in Dedicated Workers.
    // getDirectory() succeeding alone is not sufficient.
    // Use a unique filename so concurrent workers don't collide on the same
    // probe file (each createSyncAccessHandle is exclusive).
    const probeName = `_taladb_probe_${Date.now()}_${Math.random().toString(36).slice(2)}`;
    const probe = await root.getFileHandle(probeName, { create: true });
    if (typeof probe.createSyncAccessHandle !== 'function') return false;
    const handle = await probe.createSyncAccessHandle();
    handle.close();
    await root.removeEntry(probeName);
    return true;
  } catch {
    return false;
  }
}
