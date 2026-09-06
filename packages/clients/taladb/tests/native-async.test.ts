import { afterEach, expect, it, vi } from 'vitest';
import { openDB } from '../src/index';

afterEach(() => vi.unstubAllGlobals());

it('orders native writes, index reads, and close on the actual adapter async path', async () => {
  let finish!: (id: string) => void;
  const calls: string[] = [];
  const native = {
    callAsync: vi.fn((op: string) => {
      calls.push(op);
      if (op === 'insert') return new Promise<string>(r => { finish = r; });
      if (op === 'listIndexes') return Promise.resolve({ btree: ['title'], fts: [], vector: [] });
      return Promise.resolve(null);
    }),
    insert: vi.fn(() => { throw new Error('sync write used'); }),
    close: vi.fn(() => calls.push('close')),
  };
  vi.stubGlobal('nativeCallSyncHook', () => {});
  vi.stubGlobal('__TalaDB__', native);
  const db = await openDB('test');
  const col = db.collection('docs');
  const writing = col.insert({ title: 'a' });
  const indexes = col.listIndexes();
  const closing = db.close();
  await vi.waitFor(() => expect(calls).toEqual(['insert']));
  finish('id');
  await expect(writing).resolves.toBe('id');
  await expect(indexes).resolves.toEqual({ btree: ['title'], fts: [], vector: [] });
  await closing;
  expect(calls).toEqual(['insert', 'listIndexes', 'close']);
  expect(native.insert).not.toHaveBeenCalled();
  await expect(col.count()).rejects.toThrow('closed');
});

it('passes HNSW configuration and native errors through without claiming success', async () => {
  const callAsync = vi.fn(async (op: string) => {
    if (op === 'insert') throw new Error('DuplicateId');
  });
  vi.stubGlobal('nativeCallSyncHook', () => {});
  vi.stubGlobal('__TalaDB__', { callAsync, close() {} });
  const db = await openDB('test');
  const col = db.collection('docs');
  await col.createVectorIndex('v', { dimensions: 3, indexType: 'hnsw' });
  expect(callAsync).toHaveBeenCalledWith('createVectorIndex', ['docs', 'v', 3, { hnsw: { m: 32, ef_construction: 200 } }]);
  await expect(col.insert({})).rejects.toThrow('DuplicateId');
  await db.close();
});
