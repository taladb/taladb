import { expect, it, vi } from 'vitest';
import { createVectorClient, vectorIndexRequest } from '../src/vector-client';

it('cancels between batches without publishing or leaving a build running', async () => {
  const controller = new AbortController();
  const send = vi.fn(async (request: Record<string, unknown>) => {
    if (request.op === 'beginBuild') return { id: 'one', state: 'building', processed: 0, total: 100 };
    if (request.op === 'stepBuild') return { id: 'one', state: 'building', processed: 10, total: 100 };
    if (request.op === 'cancelBuild') return { id: 'one', state: 'cancelled', processed: 10, total: 100 };
    throw new Error('unexpected operation');
  });
  const client = createVectorClient(send);
  await expect(client.rebuildVectorIndex('v', { signal: controller.signal, batchSize: 10,
    onProgress: p => { if (p.processed === 10) controller.abort(); },
  })).rejects.toMatchObject({ name: 'AbortError' });
  expect(send.mock.calls.map(([r]) => r.op)).toEqual(['beginBuild', 'stepBuild', 'cancelBuild']);
});

it('rejects invalid graph creation options before changing an index', () => {
  expect(() => vectorIndexRequest('v', { dimensions: 2, indexType: 'hnsw', hnswM: 1 })).toThrow('hnswM');
  expect(() => vectorIndexRequest('v', { dimensions: 2, metric: 'dot', indexType: 'hnsw' })).toThrow('cosine or euclidean');
  expect(() => vectorIndexRequest('v', { dimensions: 2, metric: 'euclidean', indexType: 'hnsw', quantization: 'binary' })).toThrow('binary quantization');
});

it('rejects invalid numeric inputs before JSON could convert NaN to null', async () => {
  const send = vi.fn();
  const client = createVectorClient(send);
  await expect(client.searchVectors('v', [1, 0], 5, undefined, { scoreThreshold: NaN })).rejects.toThrow('finite');
  await expect(client.searchVectors('v', [1, Infinity], 5)).rejects.toThrow('finite');
  await expect(client.searchVectors('v', [1, 0], 5, undefined, { offset: -1 })).rejects.toThrow('integer');
  await expect(client.rebuildVectorIndex('v', { batchSize: 10000 })).rejects.toThrow('batchSize');
  expect(send).not.toHaveBeenCalled();
});

it('propagates rebuild conflicts and cleans up when progress callbacks throw', async () => {
  const send = vi.fn(async (request: Record<string, unknown>) => {
    if (request.op === 'beginBuild') return { id: 'one', state: 'building', processed: 0, total: 100 };
    if (request.op === 'cancelBuild') return { id: 'one', state: 'cancelled' };
    return { id: 'one', state: 'failed', error: 'vectors changed' };
  });
  const client = createVectorClient(send);
  await expect(client.rebuildVectorIndex('v')).rejects.toThrow('vectors changed');
  await expect(client.rebuildVectorIndex('v', { onProgress() { throw new Error('UI error'); } })).rejects.toThrow('UI error');
  expect(send).toHaveBeenCalledWith({ op: 'cancelBuild', field: 'v', id: 'one' });
});
