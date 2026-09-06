/**
 * One DedicatedWorker owns each database under a Web Lock. Other tabs send
 * requests to that owner and receive its actual result. They never keep a
 * writable snapshot. Requests are not automatically retried: a timeout can
 * mean the owner committed but its response was lost.
 */
let wasmReady;
function loadWasm() { return wasmReady ??= (async () => {
  const wasm = await import(/* @vite-ignore */ '../pkg/taladb_web.js');
  await wasm.default();
  return wasm;
})(); }
let db = null;
let WorkerDB = null;
let activeDbName = null;
let activeConfigJson = null;
let encrypted = false;
let owner = false;
let epoch = null;
let ownerEpoch = null;
let channel = null;
let releaseLock = null;
let syncHandle = null;
let lockAbort = null;
let closed = false;
let backend = null;
let immediate = true;
let flushMs = 500;
let dirty = false;
let flushTimer = null;
let storageError = null;
let queue = Promise.resolve();
let queued = 0;
let sequence = 0;
const clientId = `${Date.now()}-${Math.random()}`;
const pending = new Map();
const MAX_PENDING = 128;
const MAX_REQUEST_BYTES = 32 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES = 32 * 1024 * 1024;
const mutations = new Set(['insert', 'insertMany', 'updateOne', 'updateMany', 'deleteOne', 'deleteMany',
  'createIndex', 'dropIndex', 'createCompoundIndex', 'dropCompoundIndex', 'createFtsIndex', 'dropFtsIndex',
  'createVectorIndex', 'dropVectorIndex', 'upgradeVectorIndex', 'setUserVersion']);

function enqueue(work) {
  if (queued >= MAX_PENDING) return Promise.reject(new Error('TalaDB request queue is full'));
  queued++;
  const result = queue.then(work);
  queue = result.catch(() => {}).finally(() => { queued--; });
  return result;
}
function rejectPending(message) {
  for (const p of pending.values()) { clearTimeout(p.timer); p.reject(new Error(message)); }
  pending.clear();
}
function announce() {
  channel?.postMessage({ type: 'taladb:owner', epoch, encrypted });
}
function remote(op, args) {
  if (!channel || closed) return Promise.reject(new Error('TalaDB storage owner is unavailable'));
  if (pending.size >= MAX_PENDING) return Promise.reject(new Error('TalaDB remote request queue is full'));
  const id = ++sequence;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error('TalaDB owner response timed out; mutation outcome is unknown. Do not blindly retry non-idempotent writes.'));
    }, 30000);
    pending.set(id, { resolve, reject, timer });
    try { channel.postMessage({ type: 'taladb:request', client: clientId, id, epoch: ownerEpoch, op, args }); }
    catch (error) { pending.delete(id); clearTimeout(timer); reject(error); }
  });
}
function connectChannel() {
  if (typeof BroadcastChannel === 'undefined') return;
  channel = new BroadcastChannel(`taladb:${activeDbName}`);
  channel.onmessage = ({ data }) => {
    if (!data || typeof data !== 'object') return;
    if (data.type === 'taladb:owner' && !owner) {
      if (ownerEpoch && ownerEpoch !== data.epoch) rejectPending('TalaDB storage owner changed; outstanding mutation outcomes are unknown');
      ownerEpoch = data.epoch;
      return;
    }
    if (data.type === 'taladb:response' && data.client === clientId) {
      const p = pending.get(data.id);
      if (!p) return;
      pending.delete(data.id); clearTimeout(p.timer);
      if (data.error) p.reject(new Error(data.error)); else p.resolve(data.result);
      return;
    }
    if (data.type !== 'taladb:request' || !owner || closed) return;
    const respond = result => channel?.postMessage({ type: 'taladb:response', client: data.client, id: data.id, ...result });
    // Encrypted stores intentionally remain single-tab; never return their data
    // through the unauthenticated same-origin BroadcastChannel.
    if (encrypted) { respond({ error: 'Encrypted TalaDB databases are single-tab' }); return; }
    enqueue(async () => {
      if (!owner || closed || (data.epoch && data.epoch !== epoch)) throw new Error('TalaDB storage owner changed; request was not executed');
      if (['init', 'close'].includes(data.op)) throw new Error('Remote lifecycle operation is not allowed');
      return executeOwned(data.op, data.args ?? {});
    }).then(result => respond({ result }), error => respond({ error: String(error.message ?? error) }));
  };
}

function idbOpen() {
  return new Promise((resolve, reject) => {
    if (!self.indexedDB) { reject(new Error('IndexedDB is unavailable')); return; }
    const req = self.indexedDB.open('taladb', 1);
    let blocked = false;
    req.onupgradeneeded = () => {
      if (!req.result.objectStoreNames.contains('snapshots')) req.result.createObjectStore('snapshots');
    };
    req.onsuccess = () => {
      if (blocked) req.result.close();
      else resolve(req.result);
    };
    req.onerror = () => reject(req.error ?? new Error('IndexedDB open failed'));
    req.onblocked = () => {
      blocked = true;
      reject(new Error('IndexedDB open is blocked'));
    };
  });
}
async function idbLoadSnapshot(name) {
  const idb = await idbOpen();
  try {
    return await new Promise((resolve, reject) => {
      const tx = idb.transaction('snapshots', 'readonly');
      const req = tx.objectStore('snapshots').get(name);
      let value = null;
      req.onsuccess = () => { value = req.result ?? null; };
      tx.oncomplete = () => resolve(value);
      tx.onerror = tx.onabort = () => reject(tx.error ?? new Error('IndexedDB snapshot read failed'));
    });
  } finally { idb.close(); }
}
async function idbSaveSnapshot(name, bytes) {
  const idb = await idbOpen();
  try {
    await new Promise((resolve, reject) => {
      const tx = idb.transaction('snapshots', 'readwrite');
      tx.objectStore('snapshots').put(bytes, name);
      tx.oncomplete = resolve;
      tx.onerror = tx.onabort = () => reject(tx.error ?? new Error('IndexedDB snapshot write failed'));
    });
  } finally { idb.close(); }
}
async function flushSnapshot() {
  clearTimeout(flushTimer); flushTimer = null;
  if (!dirty || backend !== 'indexeddb') return;
  try {
    const bytes = db.exportSnapshot(MAX_SNAPSHOT_BYTES);
    if (bytes.byteLength > MAX_SNAPSHOT_BYTES) throw new Error('TalaDB IndexedDB fallback is limited to 32 MiB; use OPFS for larger databases');
    await idbSaveSnapshot(activeDbName, bytes);
    dirty = false; storageError = null;
  } catch (e) { storageError = String(e.message ?? e); throw e; }
}
function scheduleFlush() {
  // A fixed timer from the first dirty write cannot be postponed indefinitely.
  if (flushTimer !== null) return;
  flushTimer = setTimeout(() => {
    flushTimer = null;
    enqueue(flushSnapshot).catch(e => { storageError = String(e.message ?? e); });
  }, flushMs);
}
async function executeOwned(op, args) {
  if (!owner || !db) throw new Error('TalaDB storage owner is unavailable');
  if (op === 'hello') return { epoch, encrypted, config: activeConfigJson };
  if (op === 'capabilities') return { storage: backend, durableWrites: immediate,
    maxSnapshotBytes: backend === 'indexeddb' ? MAX_SNAPSHOT_BYTES : null, storageError,
    hnsw: false, owner: true };
  if (op === 'flush') { db.flush(); await flushSnapshot(); return null; }
  if (storageError && immediate) throw new Error(`TalaDB persistence failed; reopen the database: ${storageError}`);
  if (JSON.stringify(args).length > MAX_REQUEST_BYTES) throw new Error('TalaDB request exceeds 32 MiB; split the batch');
  const result = executeOp(op, args);
  if (mutations.has(op)) {
    if (backend === 'indexeddb') {
      dirty = true;
      if (immediate) await flushSnapshot(); else scheduleFlush();
    }
    channel?.postMessage('taladb:changed');
  }
  return result;
}
async function openOwner(passphrase) {
  WorkerDB = (await loadWasm()).WorkerDB;
  const durability = JSON.parse(activeConfigJson ?? '{}').durability ?? {};
  immediate = durability.flush_every_write !== false;
  flushMs = durability.flush_ms ?? 500;
  if (!Number.isFinite(flushMs) || flushMs < 0) throw new Error('durability.flush_ms must be a nonnegative finite number');
  // Fall back only when the API is absent. Permission, quota, and handle
  // errors must not silently select a different (possibly empty) database.
  const root = await navigator.storage?.getDirectory?.();
  const file = root ? await root.getFileHandle(`taladb_${activeDbName}.redb`, { create: true }) : null;
  if (file?.createSyncAccessHandle) syncHandle = await file.createSyncAccessHandle();
  if (syncHandle) {
    try {
      const salt = encrypted ? await loadOrCreateSalt(root, `taladb_${activeDbName}.redb.salt`) : null;
      db = WorkerDB.openWithConfigAndOpfs(syncHandle, activeConfigJson, passphrase, salt);
      backend = 'opfs';
    } catch (e) { syncHandle.close(); syncHandle = null; throw e; }
  } else {
    if (encrypted) throw new Error('TalaDB encryption requires OPFS; the IndexedDB fallback cannot encrypt at rest');
    const bytes = await idbLoadSnapshot(activeDbName);
    if (bytes && bytes.byteLength > MAX_SNAPSHOT_BYTES) throw new Error('TalaDB IndexedDB snapshot exceeds 32 MiB');
    db = activeConfigJson ? WorkerDB.openWithConfigAndSnapshot(bytes, activeConfigJson) : WorkerDB.openWithSnapshot(bytes);
    backend = 'indexeddb';
  }
  db.setDurability(!immediate);
  owner = true; epoch = `${clientId}-${++sequence}`; ownerEpoch = epoch;
  storageError = null; dirty = false;
  announce();
}
function disposeOwner() {
  owner = false;
  // Rust must drop its database while the OPFS handle is still usable.
  db?.free(); db = null;
  syncHandle?.close(); syncHandle = null;
}
async function init(args) {
  if (activeDbName !== null) throw new Error('TalaDB worker is already initialized');
  if (typeof args.dbName !== 'string' || !args.dbName || /[/\\:]/.test(args.dbName)) {
    throw new Error('TalaDB browser database name must be nonempty and cannot contain /, backslash, or :');
  }
  if (!navigator.locks?.request) throw new Error('TalaDB persistent storage requires Web Locks for safe ownership');
  activeDbName = args.dbName; activeConfigJson = args.configJson ?? null;
  encrypted = typeof args.passphrase === 'string' && args.passphrase.length > 0;
  connectChannel();
  let acquired = false;
  await new Promise((resolve, reject) => {
    navigator.locks.request(`taladb:taladb_${activeDbName}.redb`, { ifAvailable: true }, async lock => {
      if (!lock) { resolve(); return; }
      try {
        await openOwner(args.passphrase ?? null);
        acquired = true;
        const held = new Promise(r => { releaseLock = r; });
        resolve(); await held;
      } catch (e) { reject(e); }
      finally { disposeOwner(); }
    }).catch(reject);
  });
  if (acquired) return null;
  if (encrypted) throw new Error('Encrypted TalaDB databases are single-tab');
  if (!channel) throw new Error('TalaDB multi-tab access requires BroadcastChannel');
  const hello = await remote('hello', {});
  ownerEpoch = hello.epoch;
  if (hello.encrypted) throw new Error('Encrypted TalaDB databases are single-tab');
  if (hello.config !== activeConfigJson) throw new Error('TalaDB tabs must use the same database configuration');
  lockAbort = new AbortController();
  // Keep the ownership request pending. No speculative writes are replayed.
  navigator.locks.request(`taladb:taladb_${activeDbName}.redb`, { signal: lockAbort.signal }, async () => {
    rejectPending('TalaDB owner changed; outstanding mutation outcomes are unknown');
    if (closed) return;
    await enqueue(() => openOwner(null));
    const held = new Promise(r => { releaseLock = r; });
    if (closed) releaseLock();
    await held; disposeOwner();
  }).catch(e => { if (!closed) storageError = String(e.message ?? e); });
  return null;
}
async function dispatch(op, args) {
  if (op === 'init') return init(args);
  if (closed || activeDbName === null) throw new Error('TalaDB database is closed or not initialized');
  if (op === 'isPrimary') return owner;
  if (op === 'close') {
    let failure;
    try { if (owner) { db.flush(); await flushSnapshot(); } }
    catch (e) { failure = e; }
    closed = true; clearTimeout(flushTimer); lockAbort?.abort();
    rejectPending('TalaDB database closed; outstanding mutation outcomes are unknown');
    channel?.close(); channel = null;
    if (releaseLock) { releaseLock(); releaseLock = null; }
    else disposeOwner();
    if (failure) throw failure;
    return null;
  }
  const result = await (owner ? executeOwned(op, args) : remote(op, args));
  return op === 'capabilities' ? { ...result, owner } : result;
}
self.onmessage = ({ data }) => {
  const { id, op, ...args } = data;
  enqueue(() => dispatch(op, args)).then(
    result => self.postMessage({ id, result: result ?? null }),
    error => self.postMessage({ id, error: String(error.message ?? error) }),
  );
};
function executeOp(op, args) {
  switch (op) {
    case 'insert': {
      const result = db.insert(args.collection, args.docJson);
      return result;
    }

    case 'insertMany': {
      const result = db.insertMany(args.collection, args.docsJson);
      return result;
    }

    case 'find':
      return db.find(args.collection, args.filterJson ?? 'null');

    case 'findOne':
      return db.findOne(args.collection, args.filterJson ?? 'null');

    case 'updateOne': {
      const result = db.updateOne(args.collection, args.filterJson, args.updateJson);
      return result;
    }

    case 'updateMany': {
      const result = db.updateMany(args.collection, args.filterJson, args.updateJson);
      return result;
    }

    case 'deleteOne': {
      const result = db.deleteOne(args.collection, args.filterJson);
      return result;
    }

    case 'deleteMany': {
      const result = db.deleteMany(args.collection, args.filterJson);
      return result;
    }

    case 'count':
      return db.count(args.collection, args.filterJson ?? 'null');

    // Cheap change-detection for subscribe(): one integer, no query.
    case 'writeGeneration': return `${epoch}:${db.writeGeneration(args.collection)}`;

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

    case 'compact': db.compact(); return null;
    case 'userVersion': return db.userVersion();
    case 'setUserVersion': db.setUserVersion(args.version); return null;
    default: throw new Error(`Unknown TalaDB operation: ${op}`);
  }
}

async function loadOrCreateSalt(root, saltFileName) {
  const fh = await root.getFileHandle(saltFileName, { create: true });
  const h = await fh.createSyncAccessHandle();
  try {
    const size = h.getSize();
    if (size === 16) {
      const salt = new Uint8Array(16);
      if (h.read(salt, { at: 0 }) !== 16) throw new Error('Incomplete TalaDB salt read');
      return salt;
    }
    if (size === 0) {
      const salt = new Uint8Array(16);
      self.crypto.getRandomValues(salt);
      if (h.write(salt, { at: 0 }) !== 16) throw new Error('Incomplete TalaDB salt write');
      h.flush();
      return salt;
    }
    throw new Error(`invalid TalaDB salt file (${size} bytes, expected 16)`);
  } finally {
    h.close();
  }
}
