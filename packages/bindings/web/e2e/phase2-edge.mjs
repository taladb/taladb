// Phase 2 — edge cases on the document database.
export async function run(page, r) {
  await page.evaluate(async () => {
    window.db = await window.taladb.openDB('edge.db');
    window.c = window.db.collection('edge');
  });

  await r.test('empty collection: find / findOne / count / aggregate', async (r) => {
    const out = await page.evaluate(async () => {
      const e = window.db.collection('never_written');
      return {
        find: await e.find(),
        findOne: await e.findOne({ a: 1 }),
        count: await e.count(),
        agg: await e.aggregate([{ $match: {} }, { $sort: { a: 1 } }, { $limit: 10 }]),
        del: await e.deleteMany({}),
        upd: await e.updateMany({}, { $set: { a: 1 } }),
      };
    });
    r.eq(out.find, [], 'find → []');
    r.eq(out.findOne, null, 'findOne → null');
    r.eq(out.count, 0, 'count → 0');
    r.eq(out.agg, [], 'aggregate → []');
    r.eq(out.del, 0, 'deleteMany → 0');
    r.eq(out.upd, 0, 'updateMany → 0');
  });

  await r.test('value types round-trip (unicode, floats, null, nested, arrays)', async (r) => {
    const out = await page.evaluate(async () => {
      const doc = {
        emoji: '🇵🇭 kumusta — “quotes” \\ backslash \n newline \t tab',
        zero: 0,
        neg: -42,
        float: 3.141592653589793,
        big: Number.MAX_SAFE_INTEGER,
        small: Number.MIN_SAFE_INTEGER,
        tiny: 1e-9,
        bool: false,
        nil: null,
        arr: [1, 'two', { three: 3 }, [4]],
        deep: { a: { b: { c: { d: 'bottom' } } } },
        empty: {},
        emptyArr: [],
        longStr: 'x'.repeat(100000),
      };
      const id = await window.c.insert(doc);
      const back = await window.c.findOne({ _id: id });
      const cmp = {};
      for (const k of Object.keys(doc)) {
        cmp[k] = JSON.stringify(back[k]) === JSON.stringify(doc[k]);
      }
      return { cmp, longLen: back.longStr?.length, id };
    });
    for (const [k, v] of Object.entries(out.cmp)) r.ok(v, `${k} round-trips`);
    r.eq(out.longLen, 100000, '100k string preserved');
  });

  await r.test('null vs missing field', async (r) => {
    const out = await page.evaluate(async () => {
      const n = window.db.collection('nulls');
      await n.insertMany([{ k: 'has-null', v: null }, { k: 'missing' }, { k: 'has-value', v: 1 }]);
      return {
        eqNull: (await n.find({ v: null })).map((d) => d.k),
        existsTrue: (await n.find({ v: { $exists: true } })).map((d) => d.k),
        existsFalse: (await n.find({ v: { $exists: false } })).map((d) => d.k),
        neNull: (await n.find({ v: { $ne: null } })).map((d) => d.k),
      };
    });
    r.eq(out.eqNull, ['has-null'], '{v: null} matches only the explicit null');
    r.eq(out.existsTrue.sort(), ['has-null', 'has-value'], '$exists: true');
    r.eq(out.existsFalse, ['missing'], '$exists: false');
    r.note(JSON.stringify(out));
  });

  await r.test('caller-supplied _id is honoured; re-seeding is idempotent', async (r) => {
    const out = await page.evaluate(async () => {
      const k = window.db.collection('ids');
      const attempt = async (fn) => {
        try {
          return { ok: await fn() };
        } catch (e) {
          return { err: String(e.message ?? e).slice(0, 130) };
        }
      };
      const derived = window.taladb.deriveDocId('ids', 'sku-1');
      const inserted = await k.insert({ _id: derived, name: 'Kettle' });
      const found = await k.findOne({ _id: derived });
      const dup = await attempt(() => k.insert({ _id: derived, name: 'Kettle again' }));
      const nonUlid = await attempt(() => k.insert({ _id: 'sku-1' }));
      const nonString = await attempt(() => k.insert({ _id: 42 }));

      // The hydration flow the release recommends: fetch, insertMany, re-run.
      const rows = [
        { sku: 'a', price: 1 },
        { sku: 'b', price: 2 },
      ];
      const hydrate = () =>
        window.db.collection('hydrated').insertMany(
          rows.map((row) => ({ ...row, _id: window.taladb.deriveDocId('hydrated', row.sku) })),
        );
      await hydrate();
      const second = await attempt(hydrate);

      return {
        derived,
        inserted,
        found: found ? { id: found._id, name: found.name } : null,
        dup,
        nonUlid,
        nonString,
        secondHydrate: second,
        hydratedCount: await window.db.collection('hydrated').count(),
        stillOriginal: (await k.findOne({ _id: derived }))?.name,
      };
    });
    r.eq(out.inserted, out.derived, 'insert reports the id it was given');
    r.eq(out.found?.id, out.derived, 'the document is retrievable by that id');
    r.ok(out.dup.err, 'a duplicate id is refused rather than duplicated');
    r.eq(out.stillOriginal, 'Kettle', 'the refused insert did not overwrite');
    r.ok(out.nonUlid.err?.includes('deriveDocId'), 'a non-ULID id points at deriveDocId');
    r.ok(out.nonString.err, 'a non-string id is refused');
    r.eq(out.hydratedCount, 2, 're-running a seed does not duplicate rows');
    r.note(JSON.stringify(out).slice(0, 300));
  });

  await r.test('sort stability + paging over ties', async (r) => {
    const out = await page.evaluate(async () => {
      const t = window.db.collection('ties');
      await t.insertMany(
        Array.from({ length: 300 }, (_, i) => ({ n: i, bucket: i % 3, same: 1 })),
      );
      await t.createIndex('bucket');
      const seen = [];
      for (let skip = 0; skip < 300; skip += 25) {
        const pageDocs = await t.aggregate([
          { $sort: { bucket: 1 } },
          { $skip: skip },
          { $limit: 25 },
          { $project: { n: 1 } },
        ]);
        seen.push(...pageDocs.map((d) => d._id));
      }
      // Same again on a completely tied field.
      const seenTied = [];
      for (let skip = 0; skip < 300; skip += 25) {
        const pageDocs = await t.aggregate([
          { $sort: { same: 1 } },
          { $skip: skip },
          { $limit: 25 },
        ]);
        seenTied.push(...pageDocs.map((d) => d._id));
      }
      return {
        total: seen.length,
        unique: new Set(seen).size,
        tiedTotal: seenTied.length,
        tiedUnique: new Set(seenTied).size,
      };
    });
    r.eq(out.unique, 300, 'indexed-field paging visits each doc exactly once');
    r.eq(out.tiedUnique, 300, 'fully-tied paging visits each doc exactly once');
  });

  await r.test('$sort with a missing field, and mixed types', async (r) => {
    const out = await page.evaluate(async () => {
      const m = window.db.collection('mixed');
      await m.insertMany([
        { k: 'a', v: 10 },
        { k: 'b' },
        { k: 'c', v: 'string' },
        { k: 'd', v: 2 },
        { k: 'e', v: null },
      ]);
      const asc = await m.aggregate([{ $sort: { v: 1 } }]);
      const desc = await m.aggregate([{ $sort: { v: -1 } }]);
      return { asc: asc.map((d) => d.k), desc: desc.map((d) => d.k), n: asc.length };
    });
    r.eq(out.n, 5, 'sort keeps every document, including the one missing the field');
    r.ok(
      out.asc.indexOf('d') < out.asc.indexOf('a'),
      'numeric order within the sorted field (2 before 10)',
    );
    r.note(`asc=${out.asc} desc=${out.desc}`);
  });

  await r.test('reserved / unusual collection names', async (r) => {
    const out = await page.evaluate(async () => {
      const attempt = async (name) => {
        try {
          const col = window.db.collection(name);
          await col.insert({ a: 1 });
          return 'ok';
        } catch (e) {
          return String(e.message ?? e).slice(0, 90);
        }
      };
      return {
        underscore: await attempt('_internal'),
        empty: await attempt(''),
        spaces: await attempt('with spaces'),
        unicode: await attempt('コレクション'),
        long: await attempt('n'.repeat(300)),
      };
    });
    r.ok(out.underscore !== 'ok', '_-prefixed collection is rejected');
    r.ok(out.empty !== 'ok', 'empty collection name is rejected');
    r.note(JSON.stringify(out));
  });

  await r.test('malformed queries surface as errors, not silent wrong answers', async (r) => {
    const out = await page.evaluate(async () => {
      const attempt = async (fn) => {
        try {
          return { ok: await fn() };
        } catch (e) {
          return { err: String(e.message ?? e).slice(0, 120) };
        }
      };
      const c = window.c;
      return {
        unknownOp: await attempt(() => c.find({ a: { $bogus: 1 } })),
        badUpdate: await attempt(() => c.updateMany({}, { $bogus: { a: 1 } })),
        emptyUpdate: await attempt(() => c.updateMany({}, {})),
        rawUpdate: await attempt(() => c.updateOne({}, { a: 1 })),
        badPipeline: await attempt(() => c.aggregate([{ $nope: {} }])),
        mixedProject: await attempt(() =>
          c.aggregate([{ $project: { a: 1, b: 0 } }]),
        ),
        badRegex: await attempt(() => c.find({ emoji: { $regex: '([' } })),
        negLimit: await attempt(() => c.aggregate([{ $limit: -1 }])),
      };
    });
    r.note(JSON.stringify(out).slice(0, 500));
    r.ok(out.mixedProject.err, 'mixing $project inclusion and exclusion errors');
    r.ok(out.unknownOp.err, 'unknown filter operator errors');
    r.ok(out.badPipeline.err, 'unknown aggregate stage errors');
  });

  await r.test('non-JSON values (undefined, Date, NaN, Infinity, function)', async (r) => {
    const out = await page.evaluate(async () => {
      const w = window.db.collection('weird');
      const attempt = async (doc) => {
        try {
          const id = await w.insert(doc);
          const back = await w.findOne({ _id: id });
          return { stored: back };
        } catch (e) {
          return { err: String(e.message ?? e).slice(0, 100) };
        }
      };
      return {
        undef: await attempt({ a: undefined, b: 1 }),
        date: await attempt({ d: new Date(0) }),
        nan: await attempt({ n: NaN }),
        inf: await attempt({ i: Infinity }),
        fn: await attempt({ f: () => 1, g: 2 }),
        bigint: await (async () => {
          try {
            return { stored: await w.insert({ big: 1n }) };
          } catch (e) {
            return { err: String(e.message ?? e).slice(0, 100) };
          }
        })(),
      };
    });
    r.note(JSON.stringify(out).slice(0, 600));
    r.ok(out.nan.err || out.nan.stored?.n === null, 'NaN is rejected or normalised, not corrupt');
    r.ok(out.inf.err || out.inf.stored?.i === null, 'Infinity is rejected or normalised');
  });

  await r.test('deleteMany({}) clears the collection, indexes survive', async (r) => {
    const out = await page.evaluate(async () => {
      const d = window.db.collection('wipe');
      await d.createIndex('n');
      await d.insertMany(Array.from({ length: 50 }, (_, i) => ({ n: i })));
      const deleted = await d.deleteMany({});
      const after = await d.count();
      await d.insert({ n: 999 });
      return {
        deleted,
        after,
        found: (await d.find({ n: 999 })).length,
        indexes: await d.listIndexes(),
      };
    });
    r.eq(out.deleted, 50, 'deleteMany({}) reports every document');
    r.eq(out.after, 0, 'collection is empty');
    r.eq(out.found, 1, 'index still serves reads after a wipe');
  });

  await r.test('concurrent writes from one tab do not interleave badly', async (r) => {
    const out = await page.evaluate(async () => {
      const con = window.db.collection('concurrent');
      const ids = await Promise.all(
        Array.from({ length: 100 }, (_, i) => con.insert({ i })),
      );
      const counters = window.db.collection('counters');
      const cid = await counters.insert({ n: 0 });
      await Promise.all(
        Array.from({ length: 50 }, () => counters.updateOne({ _id: cid }, { $inc: { n: 1 } })),
      );
      const counter = await counters.findOne({ _id: cid });
      return {
        n: ids.length,
        unique: new Set(ids).size,
        stored: await con.count(),
        counter: counter.n,
      };
    });
    r.eq(out.unique, 100, '100 concurrent inserts → 100 distinct ids');
    r.eq(out.stored, 100, 'all concurrent inserts persisted');
    r.eq(out.counter, 50, '50 concurrent $inc all landed');
  });

  await r.test('vector index edge cases', async (r) => {
    const out = await page.evaluate(async () => {
      const v = window.db.collection('vecs');
      const attempt = async (fn) => {
        try {
          return { ok: await fn() };
        } catch (e) {
          return { err: String(e.message ?? e).slice(0, 110) };
        }
      };
      await v.createVectorIndex('emb', { dimensions: 4, metric: 'cosine' });
      const wrongDim = await attempt(() => v.insert({ emb: [1, 2, 3] }));
      const wrongDimStored = await v.count({ k: { $exists: false } });
      const nullVector = await attempt(() => v.insert({ emb: null, k: 'null-emb' }));
      const noVector = await attempt(() => v.insert({ k: 'no-emb' }));
      await v.insertMany([
        { emb: [1, 0, 0, 0], k: 'a' },
        { emb: [0, 1, 0, 0], k: 'b' },
      ]);
      const backfillMismatch = await attempt(async () => {
        const b = window.db.collection('backfill');
        await b.insertMany([{ e: [1, 2, 3, 4] }, { e: [1, 2] }]);
        await b.createVectorIndex('e', { dimensions: 4 });
        return 'index built';
      });
      return {
        wrongDim,
        wrongDimStored,
        nullVector,
        noVector,
        backfillMismatch,
        queryWrongDim: await attempt(() => v.findNearest('emb', [1, 2], 2)),
        topKOverflow: (await v.findNearest('emb', [1, 0, 0, 0], 100)).length,
        topKZero: await attempt(() => v.findNearest('emb', [1, 0, 0, 0], 0)),
        zeroVector: await attempt(() => v.findNearest('emb', [0, 0, 0, 0], 2)),
        noIndex: await attempt(() =>
          window.db.collection('novec').findNearest('nope', [1, 2, 3, 4], 2),
        ),
        hnsw: await attempt(() =>
          v.createVectorIndex('emb2', { dimensions: 4, indexType: 'hnsw' }),
        ),
      };
    });
    r.note(JSON.stringify(out).slice(0, 600));
    r.ok(out.wrongDim.err, 'inserting a wrong-dimension vector errors');
    r.eq(out.wrongDimStored, 0, 'the rejected document was not stored either');
    r.ok(out.nullVector.ok, 'a null vector (not embedded yet) is still allowed');
    r.ok(out.noVector.ok, 'a document without the vector field is still allowed');
    r.ok(
      out.backfillMismatch.ok,
      'building an index over pre-existing mismatched vectors still succeeds (by design)',
    );
    r.ok(out.queryWrongDim.err, 'querying with a wrong-dimension vector errors');
    r.eq(out.topKOverflow, 2, 'topK larger than the collection returns everything');
    r.eq(out.topKZero.ok, [], 'topK of 0 returns nothing');
    r.ok(out.noIndex.err, 'findNearest without an index errors');
    r.ok(out.hnsw.err?.includes('HNSW'), 'HNSW is refused in the browser with a clear message');
  });

  await r.test('use-after-close errors instead of hanging', async (r) => {
    const out = await page.evaluate(async () => {
      const db2 = await window.taladb.openDB('closeme.db');
      const col = db2.collection('x');
      await col.insert({ a: 1 });
      await db2.close();
      const timeout = new Promise((res) => setTimeout(() => res('HUNG'), 3000));
      const attempt = Promise.resolve()
        .then(() => col.find())
        .then(() => 'RESOLVED')
        .catch((e) => `ERR: ${String(e.message ?? e).slice(0, 80)}`);
      const closeAgain = await db2
        .close()
        .then(() => 'ok')
        .catch((e) => `ERR: ${String(e.message ?? e).slice(0, 60)}`);
      return { after: await Promise.race([attempt, timeout]), closeAgain };
    });
    r.ok(out.after !== 'HUNG', 'a read after close settles rather than hanging');
    r.note(JSON.stringify(out));
  });

  await r.test('two databases in one tab stay isolated', async (r) => {
    const out = await page.evaluate(async () => {
      const a = await window.taladb.openDB('iso-a.db');
      const b = await window.taladb.openDB('iso-b.db');
      await a.collection('t').insert({ from: 'a' });
      await b.collection('t').insert({ from: 'b' });
      const res = {
        a: (await a.collection('t').find()).map((d) => d.from),
        b: (await b.collection('t').find()).map((d) => d.from),
      };
      await a.close();
      await b.close();
      return res;
    });
    r.eq(out.a, ['a'], 'db A sees only its own documents');
    r.eq(out.b, ['b'], 'db B sees only its own documents');
  });
}
