// Persistent ANN through the production worker, OPFS, and multi-tab protocol.
import { newTab } from './lib.mjs';

export async function run(page, r, browser) {
  await r.test('browser ANN persists across reload and maintains writes', async r => {
    const before = await page.evaluate(async () => {
      window.ann = await window.taladb.openDB('ann-lifecycle.db');
      const c = window.ann.collection('docs');
      await c.insertMany(Array.from({ length: 48 }, (_, i) => ({ i, parent: Math.floor(i / 3), keep: i % 6 === 0, v: [1, i / 48, -i / 48, 0] })));
      await c.createVectorIndex('v', { dimensions: 4, indexType: 'hnsw', hnswM: 8, quantization: 'scalar' });
      const hit = await c.searchVectors('v', [1, 0, 0, 0], 3, undefined, { mode: 'ann' });
      return { path: hit.execution.path, status: await c.vectorIndexStatus('v'), capabilities: await window.ann.storageInfo() };
    });
    r.eq(before.path, 'hnsw', 'the graph actually serves the query');
    r.eq(before.status.indexedVectors, 48, 'all vectors are indexed');
    r.eq(before.capabilities.hnsw, true, 'browser reports ANN capability');
    await page.reload({ waitUntil: 'networkidle0' });
    const after = await page.evaluate(async () => {
      window.ann = await window.taladb.openDB('ann-lifecycle.db');
      const c = window.ann.collection('docs');
      const reopened = await c.searchVectors('v', [1, 0, 0, 0], 3, undefined, { mode: 'ann' });
      await c.updateOne({ i: 0 }, { $set: { v: [0, 1, 0, 0] } });
      const updated = await c.searchVectors('v', [0, 1, 0, 0], 1, undefined, { mode: 'ann' });
      await c.deleteOne({ i: 0 });
      return { path: reopened.execution.path, updated: updated.hits[0].document.i, status: await c.vectorIndexStatus('v') };
    });
    r.eq(after.path, 'hnsw', 'reload uses persisted graph without rebuilding');
    r.eq(after.updated, 0, 'updated embedding is immediately searchable');
    r.eq(after.status.indexedVectors, 47, 'delete maintains live graph count');
    r.eq(after.status.state, 'ready', 'graph remains ready after writes');
  });

  await r.test('filtered ANN, grouping, range search and recall use the worker API', async r => {
    const out = await page.evaluate(async () => {
      const c = window.ann.collection('docs');
      const exact = await c.searchVectors('v', [1, 0, 0, 0], 4, { keep: true });
      const ann = await c.searchVectors('v', [1, 0, 0, 0], 4, { keep: true }, { mode: 'ann', efSearch: 64 });
      const grouped = await c.searchVectors('v', [1, 0, 0, 0], 3, undefined, { mode: 'ann', groupBy: 'parent', offset: 1 });
      const range = await c.findWithin('v', [1, 0, 0, 0], 0.95);
      const recall = await c.measureVectorRecall('v', [[1, 0, 0, 0], [1, 0.5, -0.5, 0]], 5, undefined, { efSearch: 64 });
      return { exact, ann, groups: grouped.hits.map(h => h.document.parent), range, recall };
    });
    r.eq(out.exact.execution.path, 'exact', 'filtered search stays exact by default');
    r.eq(out.ann.execution.path, 'hnsw', 'explicit filtered ANN traverses the graph');
    r.eq(out.ann.hits.map(h => h.document.i), out.exact.hits.map(h => h.document.i), 'filtered ANN agrees with exact at exhaustive ef');
    r.eq(out.groups, [1, 2, 3], 'grouping happens before pagination');
    r.ok(out.range.hits.every(h => h.score >= 0.95), 'range search enforces threshold');
    r.eq(out.recall.recallAtK, 1, 'recall is measured against exact ground truth');
  });

  await r.test('mobile-sized rebuild batches cancel and binary rebuild completes', async r => {
    const out = await page.evaluate(async () => {
      const c = window.ann.collection('docs');
      const controller = new AbortController();
      let aborted = false;
      try {
        await c.rebuildVectorIndex('v', { batchSize: 4, signal: controller.signal, onProgress: p => { if (p.processed >= 4) controller.abort(); } });
      } catch (e) { aborted = e.name === 'AbortError'; }
      const cancelled = await c.vectorIndexStatus('v');
      const progress = [];
      await c.rebuildVectorIndex('v', { batchSize: 8, quantization: 'binary', onProgress: p => progress.push(p.processed) });
      return { aborted, cancelled, progress, status: await c.vectorIndexStatus('v'), path: (await c.searchVectors('v', [1, 0, 0, 0], 1, undefined, { mode: 'ann' })).execution.path };
    });
    r.ok(out.aborted, 'AbortSignal cancels between batches');
    r.eq(out.cancelled.build.state, 'cancelled', 'cancel state is persisted');
    r.eq(out.cancelled.state, 'ready', 'cancellation preserves the active graph');
    r.ok(out.progress.length > 2, 'progress arrives for multiple batches');
    r.eq(out.status.options.quantization, 'binary', 'binary graph is published');
    r.eq(out.path, 'hnsw', 'binary graph serves search with rescoring');
  });

  await r.test('a second tab observes incremental graph mutations', async r => {
    const second = await newTab(browser, { label: 'ANN follower' });
    try {
      await second.evaluate(async () => {
        window.ann = await window.taladb.openDB('ann-lifecycle.db');
        await window.ann.collection('docs').insert({ i: 999, v: [-1, -1, 1, 0] });
      });
      const result = await page.evaluate(async () => window.ann.collection('docs').searchVectors('v', [-1, -1, 1, 0], 1, undefined, { mode: 'ann' }));
      r.eq(result.execution.path, 'hnsw', 'cross-tab write preserves ANN');
      r.eq(result.hits[0].document.i, 999, 'owner query sees follower insertion');
    } finally { await second.close(); }
  });
  await page.evaluate(() => window.ann.close());
}
