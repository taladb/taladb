// Phase 5 — performance. Numbers are wall-clock in the browser, through the
// worker boundary, i.e. what an application actually experiences.
const N = Number(process.env.PERF_N ?? 10000);

export async function run(page, r) {
  await r.test('open latency (cold vs warm)', async (r) => {
    const cold = await page.evaluate(async () => {
      const t0 = performance.now();
      const db = await window.taladb.openDB('perf.db');
      const ms = performance.now() - t0;
      window.db = db;
      window.c = db.collection('docs');
      return ms;
    });
    const warm = await page.evaluate(async () => {
      await window.db.close();
      const t0 = performance.now();
      const db = await window.taladb.openDB('perf.db');
      const ms = performance.now() - t0;
      window.db = db;
      window.c = db.collection('docs');
      return ms;
    });
    r.note(`cold ${Math.round(cold)}ms, warm ${Math.round(warm)}ms`);
    r.ok(cold < 3000, 'cold open under 3s');
  });

  await r.test(`bulk insert ${N} documents`, async (r) => {
    const out = await page.evaluate(async (n) => {
      const docs = Array.from({ length: n }, (_, i) => ({
        idx: i,
        name: `Item ${i}`,
        city: ['Manila', 'Cebu', 'Davao', 'Iloilo'][i % 4],
        price: (i * 37) % 5000,
        rating: (i % 50) / 10,
        body: `Document number ${i} about ${['rust', 'wasm', 'vector', 'search'][i % 4]} things`,
      }));
      const t0 = performance.now();
      await window.c.insertMany(docs);
      const insert = performance.now() - t0;
      const t1 = performance.now();
      await window.c.createIndex('price');
      await window.c.createIndex('city');
      const index = performance.now() - t1;
      return { insert, index, count: await window.c.count() };
    }, N);
    r.eq(out.count, N, 'all documents stored');
    r.note(
      `insertMany ${Math.round(out.insert)}ms (${Math.round(N / (out.insert / 1000))}/s), index build ${Math.round(out.index)}ms`,
    );
    r.ok(out.insert < 15000, 'bulk insert completes in reasonable time');
  });

  await r.test('point/range reads: indexed vs unindexed', async (r) => {
    const out = await page.evaluate(async () => {
      const bench = async (fn, iters = 20) => {
        await fn();
        const t0 = performance.now();
        for (let i = 0; i < iters; i++) await fn();
        return (performance.now() - t0) / iters;
      };
      return {
        indexedEq: await bench(() => window.c.find({ price: 3700 })),
        indexedRange: await bench(() => window.c.find({ price: { $gte: 4900 } })),
        unindexed: await bench(() => window.c.find({ idx: 4242 }), 5),
        count: await bench(() => window.c.count({ city: 'Cebu' })),
        countAll: await bench(() => window.c.count()),
        findOne: await bench(() => window.c.findOne({ price: 1000 })),
      };
    });
    const f = (x) => `${x.toFixed(1)}ms`;
    r.note(
      `indexed= ${f(out.indexedEq)} range=${f(out.indexedRange)} unindexed=${f(out.unindexed)} count(idx)=${f(out.count)} count(all)=${f(out.countAll)}`,
    );
    r.ok(out.indexedEq < out.unindexed, 'the index beats a full scan');
  });

  await r.test('paged $sort uses the index (regression guard)', async (r) => {
    const out = await page.evaluate(async () => {
      const bench = async (fn, iters = 10) => {
        await fn();
        const t0 = performance.now();
        for (let i = 0; i < iters; i++) await fn();
        return (performance.now() - t0) / iters;
      };
      const indexed = await bench(() =>
        window.c.aggregate([
          { $sort: { price: -1 } },
          { $skip: 100 },
          { $limit: 24 },
          { $project: { name: 1, price: 1 } },
        ]),
      );
      const unindexed = await bench(
        () =>
          window.c.aggregate([
            { $sort: { rating: -1 } },
            { $skip: 100 },
            { $limit: 24 },
            { $project: { name: 1 } },
          ]),
        5,
      );
      const deepPage = await bench(() =>
        window.c.aggregate([{ $sort: { price: -1 } }, { $skip: 9000 }, { $limit: 24 }]),
      );
      return { indexed, unindexed, deepPage };
    });
    r.note(
      `indexed-sort page ${out.indexed.toFixed(1)}ms, unindexed ${out.unindexed.toFixed(1)}ms, deep page ${out.deepPage.toFixed(1)}ms`,
    );
    r.ok(out.indexed < 40, `indexed paged sort stays fast (${out.indexed.toFixed(1)}ms)`);
  });

  await r.test('aggregation and search throughput', async (r) => {
    const out = await page.evaluate(async () => {
      const bench = async (fn, iters = 5) => {
        await fn();
        const t0 = performance.now();
        for (let i = 0; i < iters; i++) await fn();
        return (performance.now() - t0) / iters;
      };
      const t0 = performance.now();
      await window.c.createFtsIndex('body');
      const ftsBuild = performance.now() - t0;
      return {
        ftsBuild,
        group: await bench(() =>
          window.c.aggregate([
            { $group: { _id: '$city', n: { $sum: 1 }, avg: { $avg: '$price' } } },
          ]),
        ),
        fts: await bench(() => window.c.searchText('body', 'vector search', 10)),
        matchSortLimit: await bench(() =>
          window.c.aggregate([
            { $match: { city: 'Cebu' } },
            { $sort: { price: -1 } },
            { $limit: 10 },
          ]),
        ),
      };
    });
    r.note(
      `fts build ${Math.round(out.ftsBuild)}ms, $group ${out.group.toFixed(1)}ms, searchText ${out.fts.toFixed(1)}ms, match+sort+limit ${out.matchSortLimit.toFixed(1)}ms`,
    );
  });

  await r.test('vector search at 5k × 128', async (r) => {
    const out = await page.evaluate(async () => {
      const D = 128;
      const M = 5000;
      const v = window.db.collection('vec');
      const mk = (s) => Array.from({ length: D }, (_, i) => Math.sin(s * (i + 1)));
      const t0 = performance.now();
      await v.insertMany(Array.from({ length: M }, (_, i) => ({ i, emb: mk(i + 1) })));
      const ingest = performance.now() - t0;
      const t1 = performance.now();
      await v.createVectorIndex('emb', { dimensions: D, metric: 'cosine' });
      const build = performance.now() - t1;
      const q = mk(123);
      await v.findNearest('emb', q, 10);
      const t2 = performance.now();
      for (let i = 0; i < 10; i++) await v.findNearest('emb', q, 10);
      const search = (performance.now() - t2) / 10;
      const t3 = performance.now();
      for (let i = 0; i < 10; i++) await v.findNearest('emb', q, 10, { i: { $lt: 1000 } });
      const filtered = (performance.now() - t3) / 10;
      return { ingest, build, search, filtered };
    });
    r.note(
      `ingest ${Math.round(out.ingest)}ms, index ${Math.round(out.build)}ms, knn ${out.search.toFixed(1)}ms, filtered knn ${out.filtered.toFixed(1)}ms`,
    );
    r.ok(out.search < 250, `5k×128 knn under 250ms (${out.search.toFixed(1)}ms)`);
  });

  await r.test('write path: update/delete many', async (r) => {
    const out = await page.evaluate(async () => {
      const t0 = performance.now();
      const n = await window.c.updateMany({ city: 'Iloilo' }, { $set: { flagged: true } });
      const upd = performance.now() - t0;
      const t1 = performance.now();
      const d = await window.c.deleteMany({ city: 'Davao' });
      const del = performance.now() - t1;
      return { n, upd, d, del };
    });
    r.note(
      `updateMany ${out.n} docs in ${Math.round(out.upd)}ms, deleteMany ${out.d} docs in ${Math.round(out.del)}ms`,
    );
  });

  await r.test('webhook overhead on the write path', async (r) => {
    const out = await page.evaluate(async () => {
      const bench = async (col, iters) => {
        const t0 = performance.now();
        for (let i = 0; i < iters; i++) await col.insert({ i, s: 'x'.repeat(50) });
        return (performance.now() - t0) / iters;
      };
      const plain = await bench(window.db.collection('nohook'), 50);
      await window.db.close();
      const hooked = await window.taladb.openDB('perf.db', {
        webhook: { enabled: true, endpoint: `${location.origin}/hook` },
      });
      const withHook = await bench(hooked.collection('hooked'), 50);
      const bulkT0 = performance.now();
      await hooked
        .collection('hooked')
        .updateMany({}, { $set: { touched: true } });
      const bulkUpdate = performance.now() - bulkT0;
      const stats = hooked.webhookStats();
      await hooked.flushWebhook(20000);
      window.db = hooked;
      return { plain, withHook, bulkUpdate, stats };
    });
    r.note(
      `insert ${out.plain.toFixed(2)}ms → ${out.withHook.toFixed(2)}ms with webhook (+${(out.withHook - out.plain).toFixed(2)}ms), updateMany(50) ${Math.round(out.bulkUpdate)}ms`,
    );
    r.ok(out.withHook < out.plain + 5, 'webhook adds under 5ms per insert');
  });

  await r.test('idle live query costs nothing measurable', async (r) => {
    const out = await page.evaluate(async () => {
      const live = window.db.collection('docs');
      let fires = 0;
      const unsub = live.subscribe({ city: 'Manila' }, () => fires++);
      await new Promise((res) => setTimeout(res, 2000));
      // Time a read while the poller is running — if the poller were re-running
      // the whole query every tick this would be visibly slower.
      const t0 = performance.now();
      for (let i = 0; i < 10; i++) await live.count({ city: 'Manila' });
      const perRead = (performance.now() - t0) / 10;
      unsub();
      return { fires, perRead };
    });
    r.eq(out.fires, 1, 'an idle live query fires once (the initial snapshot) and then stays quiet');
    r.note(`read latency under an active subscription: ${out.perRead.toFixed(1)}ms`);
  });
}
