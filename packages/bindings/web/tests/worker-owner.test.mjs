import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { test } from 'node:test';
import assert from 'node:assert/strict';

// The production worker runs unchanged except for its WASM import. This fake
// engine/storage isolates ownership, async ordering, and acknowledgement logic.
const source = readFileSync(new URL('../worker/taladb.worker.js', import.meta.url), 'utf8')
  .replace(/let wasmReady;[\s\S]*?\}\)\(\); \}/, 'function loadWasm() { return Promise.resolve({ WorkerDB: MockDB }); }');
class MockDB {
  constructor(snapshot) {
    Object.assign(this, snapshot ? JSON.parse(new TextDecoder().decode(snapshot)) : { docs: [], indexes: [], version: 0, generation: 0 });
  }
  static openWithSnapshot(bytes) { return new MockDB(bytes); }
  static openWithConfigAndSnapshot(bytes) { return new MockDB(bytes); }
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
function environment({ opfsError = false, readError = false } = {}) {
  const channels = new Map(), locks = new Set(), waiting = new Map(), snapshots = new Map();
  let fail = false;
  class Channel {
    constructor(name) { this.name = name; const peers = channels.get(name) ?? new Set(); peers.add(this); channels.set(name, peers); }
    postMessage(data) { for (const p of channels.get(this.name) ?? []) if (p !== this) queueMicrotask(() => p.onmessage?.({ data })); }
    close() { channels.get(this.name)?.delete(this); this.onmessage = null; }
  }
  const navigator = { locks: { async request(name, options, callback) {
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
  return { worker, failWrites: () => { fail = true; }, async close() {
    for (const w of workers.reverse()) await w.call('close').catch(() => {});
  } };
}
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
