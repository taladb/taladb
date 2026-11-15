import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { test } from 'node:test';
import assert from 'node:assert/strict';

// The production worker runs unchanged except for its WASM import. This fake
// engine/storage isolates ownership, async ordering, and acknowledgement logic.
const source = readFileSync(new URL('../worker/taladb.worker.js', import.meta.url), 'utf8')
  .replace("import { adaptiveConfigJson } from './cache-config.js';", readFileSync(new URL('../worker/cache-config.js', import.meta.url), 'utf8').replace('export function', 'function'))
  .replace(/let wasmReady;[\s\S]*?\}\)\(\); \}/, 'function loadWasm() { return Promise.resolve({ WorkerDB: MockDB }); }');
class MockDB {
  constructor(snapshot) {
    Object.assign(this, snapshot ? JSON.parse(new TextDecoder().decode(snapshot)) : { docs: [], indexes: [], version: 0, generation: 0 });
  }
  static openWithSnapshot(bytes) { return new MockDB(bytes); }
  static openWithConfigAndSnapshot(bytes, config) { const db = new MockDB(bytes); db.config = JSON.parse(config); return db; }
  setDurability() {} flush() {} free() {}
  insert(_, json) {
    const doc = JSON.parse(json); doc._id ??= `id-${this.docs.length}`;
    if (this.docs.some(d => d._id === doc._id)) throw new Error('DuplicateId');
    this.docs.push(doc); this.generation++; return doc._id;
  }
  find() { return JSON.stringify(this.docs); }
  createIndex(_, field) { this.indexes.push(field); }
  listIndexes() { return JSON.stringify({ btree: this.indexes, fts: [], vector: [] }); }
  userVersion() { return this.version; }
  setUserVersion(v) { this.version = v; }
  writeGeneration() { return this.generation; }
  exportSnapshot() { return new TextEncoder().encode(JSON.stringify(this)); }
}
function environment({ opfsError = false, readError = false, deviceMemory } = {}) {
  const channels = new Map(), locks = new Set(), waiting = new Map(), snapshots = new Map();
  let fail = false;
  class Channel {
    constructor(name) { this.name = name; const peers = channels.get(name) ?? new Set(); peers.add(this); channels.set(name, peers); }
    postMessage(data) { for (const p of channels.get(this.name) ?? []) if (p !== this) queueMicrotask(() => p.onmessage?.({ data })); }
    close() { channels.get(this.name)?.delete(this); this.onmessage = null; }
  }
  const navigator = { deviceMemory, locks: { async request(name, options, callback) {
    if (locks.has(name)) {
      if (options.ifAvailable) return callback(null);
      await new Promise((resolve, reject) => {
        const list = waiting.get(name) ?? []; list.push(resolve); waiting.set(name, list);
        options.signal?.addEventListener('abort', () => reject(new Error('aborted')), { once: true });
      });
      if (options.signal?.aborted) throw new Error('aborted');
    }
    locks.add(name);
    try { return await callback({ name }); }
    finally { locks.delete(name); waiting.get(name)?.shift()?.(); }
  } } };
  if (opfsError) navigator.storage = { getDirectory: async () => { throw new Error('OPFS denied'); } };
  const indexedDB = { open() {
    const request = {};
    request.result = { close() {}, transaction(_, mode) {
      const tx = { objectStore() { return {
        get(name) { const req = {}; queueMicrotask(() => { if (readError) { tx.error = new Error('IDB read failed'); tx.onabort?.(); return; } req.result = snapshots.get(name); req.onsuccess?.(); queueMicrotask(() => tx.oncomplete?.()); }); return req; },
        put(bytes, name) { queueMicrotask(() => {
          // A worker killed mid-transaction: IDB never commits the put.
          if (fail === 'hang') return;
          if (fail) { tx.error = new Error('QuotaExceededError'); tx.onabort?.(); }
          else { snapshots.set(name, bytes); tx.oncomplete?.(); }
        }); },
      }; } };
      return tx;
    } };
    queueMicrotask(() => request.onsuccess?.()); return request;
  } };
  const workers = [];
  function worker() {
    const ctx = vm.createContext({ MockDB, navigator, self: { indexedDB }, BroadcastChannel: Channel,
      AbortController, console, setTimeout, clearTimeout, Uint8Array });
    vm.runInContext(source, ctx);
    const run = code => vm.runInContext(code, ctx);
    const call = (op, args = {}) => { ctx.command = { op, args }; return run('((c) => enqueue(() => dispatch(c.op, c.args)))(command)'); };
    const w = { run, call }; workers.push(w); return w;
  }
  // Terminating a worker runs none of its code: no close(), no flush. The lock
  // and its BroadcastChannel go away with it.
  const terminate = async w => {
    w.terminated = true;
    w.run('channel.close(); channel = null; clearTimeout(flushTimer); closed = true; releaseLock?.()');
    await new Promise(r => setTimeout(r, 0));
  };
  return { worker, terminate, snapshots, failWrites: (mode = true) => { fail = mode; }, allowWrites: () => { fail = false; }, async close() {
    for (const w of workers.reverse()) if (!w.terminated) await w.call('close').catch(() => {});
  } };
}
test('automatic cache hints reach the engine without changing multi-tab configuration identity', async () => {
  const e = environment({ deviceMemory: 8 });
  try {
    const a = e.worker(), b = e.worker();
    await a.call('init', { dbName: 'memory-test' });
    assert.equal(a.run('db.config.vector_cache_memory_bytes'), 8 * 1024 ** 3);
    assert.equal(a.run('activeConfigJson'), null);
    await b.call('init', { dbName: 'memory-test' });
    assert.equal(await b.call('isPrimary'), false);
    await a.call('close');
    for (let i = 0; i < 100 && !b.run('owner'); i++) await new Promise(r => setTimeout(r, 1));
    assert.equal(b.run('owner'), true);
    assert.equal(b.run('db.config.vector_cache_memory_bytes'), 8 * 1024 ** 3);
  } finally { await e.close(); }
});

test('IDB fallback elects one owner and secondary requests receive authoritative results', async () => {
  const e = environment();
  try {
    const a = e.worker(), b = e.worker();
    await a.call('init', { dbName: 'test' }); await b.call('init', { dbName: 'test' });
    assert.equal(await a.call('isPrimary'), true); assert.equal(await b.call('isPrimary'), false);
    assert.equal(b.run('db'), null);
    assert.equal((await b.call('capabilities')).owner, false);
    await a.call('insert', { collection: 'docs', docJson: '{"_id":"fixed"}' });
    await assert.rejects(b.call('insert', { collection: 'docs', docJson: '{"_id":"fixed"}' }), /DuplicateId/);
    assert.equal(JSON.parse(await b.call('find', { collection: 'docs' })).length, 1);
    await b.call('createIndex', { collection: 'docs', field: 'title' });
    await b.call('setUserVersion', { version: 3 });
    await b.call('flush');
    assert.deepEqual(JSON.parse(await a.call('listIndexes', { collection: 'docs' })).btree, ['title']);
    assert.equal(await a.call('userVersion'), 3);
  } finally { await e.close(); }
});
test('a quota abort rejects both a durable write and explicit flush', async () => {
  const e = environment();
  try {
    const w = e.worker(); await w.call('init', { dbName: 'test' });
    e.failWrites();
    await assert.rejects(w.call('insert', { collection: 'docs', docJson: '{}' }), /QuotaExceeded/);
    await assert.rejects(w.call('flush'), /QuotaExceeded/);
    assert.equal(w.run('dirty'), true);
    await assert.rejects(w.call('find', { collection: 'docs' }), /persistence failed/);
  } finally { await e.close(); }
});
test('owner failover reloads committed DDL and data without replaying writes', async () => {
  const e = environment();
  try {
    const a = e.worker(), b = e.worker();
    await a.call('init', { dbName: 'test' }); await b.call('init', { dbName: 'test' });
    await b.call('insert', { collection: 'docs', docJson: '{"_id":"once"}' });
    await b.call('createIndex', { collection: 'docs', field: 'title' });
    const before = await b.call('writeGeneration', { collection: 'docs' });
    await a.call('close');
    for (let i = 0; i < 100 && !b.run('owner'); i++) await new Promise(r => setTimeout(r, 1));
    assert.equal(b.run('owner'), true);
    assert.notEqual(await b.call('writeGeneration', { collection: 'docs' }), before);
    assert.equal(JSON.parse(await b.call('find', { collection: 'docs' })).length, 1);
    assert.deepEqual(JSON.parse(await b.call('listIndexes', { collection: 'docs' })).btree, ['title']);
  } finally { await e.close(); }
});
test('the queue orders async persistence before later reads', async () => {
  const e = environment();
  try {
    const w = e.worker(); await w.call('init', { dbName: 'test' });
    w.run(`globalThis.releaseSave = null; idbSaveSnapshot = () => new Promise(r => { releaseSave = r; });`);
    const insert = w.call('insert', { collection: 'docs', docJson: '{}' });
    await new Promise(r => setTimeout(r, 0));
    let readDone = false;
    const read = w.call('find', { collection: 'docs' }).then(v => { readDone = true; return v; });
    await new Promise(r => setTimeout(r, 0)); assert.equal(readDone, false);
    w.run('releaseSave()'); await insert;
    assert.equal(JSON.parse(await read).length, 1);
  } finally { await e.close(); }
});

test('storage read failures never open an empty replacement database', async () => {
  for (const options of [{ opfsError: true }, { readError: true }]) {
    const e = environment(options);
    try {
      const w = e.worker();
      await assert.rejects(w.call('init', { dbName: 'test' }), /OPFS denied|IDB read failed/);
      assert.equal(w.run('db'), null);
    } finally { await e.close(); }
  }
});
test('OPFS mutations do not export or persist full snapshots', async () => {
  const e = environment();
  try {
    const w = e.worker();
    w.run(`navigator.storage = { getDirectory: async () => ({ getFileHandle: async () => ({ createSyncAccessHandle: async () => ({ close() {} }) }) }) };
      MockDB.openWithConfigAndOpfs = () => new MockDB();`);
    await w.call('init', { dbName: 'test' });
    w.run(`db.exportSnapshot = () => { throw new Error('unexpected export'); };`);
    await w.call('insert', { collection: 'docs', docJson: '{}' });
    await w.call('flush');
    assert.equal((await w.call('capabilities')).storage, 'opfs');
  } finally { await e.close(); }
});

const settle = p => p.then(v => ({ ok: v }), e => ({ error: String(e.message ?? e) }));
const within = (p, ms) => Promise.race([p, new Promise(r => setTimeout(() => r({ timedOut: true }), ms))]);
async function untilOwner(w) {
  for (let i = 0; i < 200 && !w.run('owner'); i++) await new Promise(r => setTimeout(r, 1));
  return w.run('owner');
}

test('requests queued during takeover fail fast instead of stalling the new owner', async () => {
  const e = environment();
  try {
    const a = e.worker(), b = e.worker();
    await a.call('init', { dbName: 'test' }); await b.call('init', { dbName: 'test' });
    await b.call('insert', { collection: 'docs', docJson: '{"_id":"acked"}' });
    // The owner stops answering, then dies while B has one request in flight
    // and another queued behind it.
    a.run('channel.onmessage = null');
    const inFlight = settle(b.call('insert', { collection: 'docs', docJson: '{"_id":"lost-reply"}' }));
    const queued = settle(b.call('insert', { collection: 'docs', docJson: '{"_id":"queued"}' }));
    await new Promise(r => setTimeout(r, 5));
    await e.terminate(a);
    const first = await within(inFlight, 1000), second = await within(queued, 1000);
    assert.match(first.error ?? '', /outcome(s)? (is|are) unknown/, 'an interrupted request stays explicitly unknown');
    assert.ok(!second.timedOut, 'a queued request must not wait out the 30 s RPC timeout');
    assert.match(second.error ?? '', /not executed/, 'a request never sent is a definite failure');
    assert.equal(await untilOwner(b), true);
    const ids = JSON.parse(await b.call('find', { collection: 'docs' })).map(d => d._id);
    assert.deepEqual(ids, ['acked'], 'nothing is replayed and the acknowledged write survives');
  } finally { await e.close(); }
});

test('a request carrying a previous owner epoch is rejected without executing', async () => {
  const e = environment();
  try {
    const a = e.worker(), b = e.worker();
    await a.call('init', { dbName: 'test' }); await b.call('init', { dbName: 'test' });
    const stale = b.run('ownerEpoch');
    await e.terminate(a);
    assert.equal(await untilOwner(b), true);
    const c = e.worker(); await c.call('init', { dbName: 'test' });
    c.run(`ownerEpoch = ${JSON.stringify(stale)}`);
    await assert.rejects(c.call('insert', { collection: 'docs', docJson: '{"_id":"stale"}' }), /not executed/);
    assert.equal(JSON.parse(await b.call('find', { collection: 'docs' })).length, 0);
  } finally { await e.close(); }
});

test('a durable write whose persistence failed reports an unknown outcome', async () => {
  const e = environment();
  try {
    const w = e.worker(); await w.call('init', { dbName: 'test' });
    await w.call('insert', { collection: 'docs', docJson: '{"_id":"committed"}' });
    e.failWrites();
    const failed = await settle(w.call('insert', { collection: 'docs', docJson: '{"_id":"applied-in-memory"}' }));
    assert.match(failed.error, /QuotaExceeded/, 'the storage error is preserved');
    // The write is already applied in memory, so a later successful barrier
    // persists it. Callers must therefore not read this as a definite failure.
    assert.match(failed.error, /outcome is unknown/);
    e.allowWrites();
    await w.call('flush');
    await w.call('close');
    const r = e.worker(); await r.call('init', { dbName: 'test' });
    assert.deepEqual(JSON.parse(await r.call('find', { collection: 'docs' })).map(d => d._id), ['committed', 'applied-in-memory']);
  } finally { await e.close(); }
});

test('eventual IDB writes: flush is the barrier and background failures are observable', async () => {
  const e = environment();
  try {
    const config = JSON.stringify({ durability: { flush_every_write: false, flush_ms: 5 } });
    const w = e.worker(); await w.call('init', { dbName: 'test', configJson: config });
    await w.call('insert', { collection: 'docs', docJson: '{"_id":"flushed"}' });
    await w.call('flush');
    e.failWrites();
    // Eventual writes are acknowledged before persistence and are not blocked
    // by a failed background flush; the failure is reported, not hidden.
    await w.call('insert', { collection: 'docs', docJson: '{"_id":"unflushed"}' });
    for (let i = 0; i < 100 && !(await w.call('capabilities')).storageError; i++) await new Promise(r => setTimeout(r, 2));
    const caps = await w.call('capabilities');
    assert.equal(caps.durableWrites, false);
    assert.match(caps.storageError ?? '', /QuotaExceeded/);
    await assert.rejects(w.call('flush'), /QuotaExceeded/);
    assert.equal(JSON.parse(await w.call('find', { collection: 'docs' })).length, 2);
    // The worker dies before any successful flush: the unflushed write is lost,
    // the flushed one is not, and the snapshot is never replaced with empty data.
    await e.terminate(w); e.allowWrites();
    const r = e.worker(); await r.call('init', { dbName: 'test', configJson: config });
    assert.deepEqual(JSON.parse(await r.call('find', { collection: 'docs' })).map(d => d._id), ['flushed']);
  } finally { await e.close(); }
});

test('an interrupted or oversized snapshot flush keeps the previous committed snapshot', async () => {
  const e = environment();
  try {
    const w = e.worker(); await w.call('init', { dbName: 'test' });
    await w.call('insert', { collection: 'docs', docJson: '{"_id":"committed"}' });
    const before = e.snapshots.get('test');
    // The size limit is enforced on export; nothing is written.
    w.run('const realExport = db.exportSnapshot.bind(db); db.exportSnapshot = () => new Uint8Array(MAX_SNAPSHOT_BYTES + 1);');
    await assert.rejects(w.call('insert', { collection: 'docs', docJson: '{"_id":"too-big"}' }), /32 MiB/);
    assert.equal(e.snapshots.get('test'), before);
    // The IDB transaction never completes because the worker is killed mid-flush.
    w.run('db.exportSnapshot = realExport');
    e.failWrites('hang');
    const pending = settle(w.call('flush'));
    await new Promise(r => setTimeout(r, 5));
    await e.terminate(w); e.allowWrites();
    const r = e.worker(); await r.call('init', { dbName: 'test' });
    assert.deepEqual(JSON.parse(await r.call('find', { collection: 'docs' })).map(d => d._id), ['committed']);
    void pending;
  } finally { await e.close(); }
});
