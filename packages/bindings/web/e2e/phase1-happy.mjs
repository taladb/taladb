// Phase 1 — document database happy path, single tab, OPFS.
export async function run(page, r) {
  await r.test('open + collection + index creation', async (r) => {
    const out = await page.evaluate(async () => {
      const db = await window.taladb.openDB('happy.db');
      window.db = db;
      const c = db.collection('users');
      window.users = c;
      await c.createIndex('age');
      await c.createIndex('email');
      await c.createCompoundIndex(['role', 'age']);
      return await c.listIndexes();
    });
    r.ok(out.btree?.includes('age'), 'age index listed');
    r.ok(out.btree?.includes('email'), 'email index listed');
    r.note(`indexes=${JSON.stringify(out)}`);
  });

  await r.test('insert returns a ULID and the doc reads back', async (r) => {
    const out = await page.evaluate(async () => {
      const id = await window.users.insert({
        name: 'Alice',
        email: 'alice@example.com',
        age: 30,
        role: 'admin',
      });
      const doc = await window.users.findOne({ _id: id });
      const byEmail = await window.users.findOne({ email: 'alice@example.com' });
      return { id, doc, byEmail };
    });
    r.ok(/^[0-9A-HJKMNP-TV-Z]{26}$/.test(out.id), 'id is a ULID');
    r.eq(out.doc?.name, 'Alice', 'findOne by _id');
    r.eq(out.byEmail?._id, out.id, 'findOne by indexed field');
    r.ok(typeof out.doc?._changed_at === 'number', '_changed_at stamped');
  });

  await r.test('insertMany + count', async (r) => {
    const out = await page.evaluate(async () => {
      const docs = [];
      for (let i = 0; i < 500; i++) {
        docs.push({
          name: `User ${i}`,
          email: `u${i}@example.com`,
          age: 18 + (i % 50),
          role: i % 10 === 0 ? 'admin' : 'user',
          tags: ['a', `t${i % 5}`],
          profile: { city: ['Manila', 'Cebu', 'Davao'][i % 3], score: i / 7 },
        });
      }
      const t0 = performance.now();
      const ids = await window.users.insertMany(docs);
      const ms = performance.now() - t0;
      return {
        n: ids.length,
        unique: new Set(ids).size,
        total: await window.users.count(),
        admins: await window.users.count({ role: 'admin' }),
        ms: Math.round(ms),
      };
    });
    r.eq(out.n, 500, 'insertMany returned 500 ids');
    r.eq(out.unique, 500, 'ids unique');
    r.eq(out.total, 501, 'count()');
    r.eq(out.admins, 51, 'count(filter)'); // 50 seeded + Alice
    r.note(`${out.ms}ms for 500`);
  });

  await r.test('queries: eq / range / $in / $or / $and / $ne / nested', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.users;
      return {
        eq: (await c.find({ role: 'admin' })).length,
        range: (await c.find({ age: { $gte: 30, $lt: 40 } })).length,
        in: (await c.find({ age: { $in: [18, 19, 20] } })).length,
        or: (await c.find({ $or: [{ age: 18 }, { age: 19 }] })).length,
        and: (await c.find({ $and: [{ role: 'user' }, { age: { $gte: 60 } }] })).length,
        ne: (await c.find({ role: { $ne: 'user' } })).length,
        nested: (await c.find({ 'profile.city': 'Cebu' })).length,
        arrayContains: (await c.find({ tags: 't1' })).length,
        arrayContainsShared: (await c.find({ tags: 'a' })).length,
        arrayIn: (await c.find({ tags: { $in: ['t1', 't2'] } })).length,
        arrayExact: (await c.find({ tags: ['a', 't1'] })).length,
        arrayNe: (await c.find({ tags: { $ne: 'a' } })).length,
        exists: (await c.find({ email: { $exists: true } })).length,
        gtNested: (await c.find({ 'profile.score': { $gt: 70 } })).length,
      };
    });
    // 500 seeded + Alice(admin, 30, no tags/profile)
    r.eq(out.eq, 51, '$eq on role');
    r.eq(out.in, 30, '$in over ages 18-20');
    r.eq(out.or, 20, '$or');
    r.eq(out.ne, 51, '$ne');
    r.ok(out.nested > 0, 'dotted nested field');
    r.eq(out.arrayContains, 100, 'array containment matches an element (t1 on every 5th of 500)');
    r.eq(out.arrayContainsShared, 500, "every seeded doc carries the 'a' tag");
    r.eq(out.arrayIn, 200, '$in over array elements');
    r.eq(out.arrayExact, 100, 'exact whole-array equality still works');
    r.eq(out.arrayNe, 1, '$ne excludes every document carrying the element (only Alice remains)');
    r.eq(out.exists, 501, '$exists');
    r.note(JSON.stringify(out));
  });

  await r.test('update: $set / $inc / $unset, one and many', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.users;
      const one = await c.updateOne({ email: 'alice@example.com' }, { $set: { age: 31 } });
      const alice = await c.findOne({ email: 'alice@example.com' });
      const many = await c.updateMany({ role: 'admin' }, { $set: { verified: true } });
      const verified = await c.count({ verified: true });
      await c.updateOne({ email: 'alice@example.com' }, { $inc: { age: 2 } });
      const inced = await c.findOne({ email: 'alice@example.com' });
      await c.updateOne({ email: 'alice@example.com' }, { $unset: { verified: '' } });
      const unset = await c.findOne({ email: 'alice@example.com' });
      const noMatch = await c.updateOne({ email: 'nobody@example.com' }, { $set: { age: 1 } });
      return {
        one,
        aliceAge: alice.age,
        many,
        verified,
        incedAge: inced.age,
        stillHasVerified: 'verified' in unset,
        noMatch,
      };
    });
    r.eq(out.one, true, 'updateOne returns true');
    r.eq(out.aliceAge, 31, '$set applied');
    r.eq(out.many, 51, 'updateMany count');
    r.eq(out.verified, 51, 'updateMany applied');
    r.eq(out.incedAge, 33, '$inc applied');
    r.eq(out.stillHasVerified, false, '$unset removed field');
    r.eq(out.noMatch, false, 'updateOne no match → false');
  });

  await r.test('delete: one and many', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.users;
      const before = await c.count();
      const one = await c.deleteOne({ email: 'u1@example.com' });
      const noMatch = await c.deleteOne({ email: 'u1@example.com' });
      const many = await c.deleteMany({ age: 18 });
      return { before, one, noMatch, many, after: await c.count() };
    });
    r.eq(out.one, true, 'deleteOne true');
    r.eq(out.noMatch, false, 'deleteOne no match → false');
    r.ok(out.many > 0, 'deleteMany count');
    r.eq(out.after, out.before - 1 - out.many, 'count reflects deletes');
  });

  await r.test('aggregate: $match/$sort/$skip/$limit/$project/$group', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.users;
      const page1 = await c.aggregate([
        { $match: { role: 'user' } },
        { $sort: { age: -1 } },
        { $limit: 5 },
        { $project: { name: 1, age: 1 } },
      ]);
      const page2 = await c.aggregate([
        { $match: { role: 'user' } },
        { $sort: { age: -1 } },
        { $skip: 5 },
        { $limit: 5 },
        { $project: { name: 1, age: 1 } },
      ]);
      const grouped = await c.aggregate([
        { $group: { _id: '$role', n: { $sum: 1 }, avgAge: { $avg: '$age' } } },
        { $sort: { n: -1 } },
      ]);
      const excluded = await c.aggregate([
        { $match: { role: 'admin' } },
        { $limit: 1 },
        { $project: { tags: 0, profile: 0 } },
      ]);
      return { page1, page2, grouped, excluded: excluded[0] };
    });
    r.eq(out.page1.length, 5, '$limit');
    r.ok(
      out.page1.every((d) => Object.keys(d).every((k) => ['_id', 'name', 'age'].includes(k))),
      '$project inclusion keeps only named fields',
    );
    r.ok(
      out.page1[0].age >= out.page1[4].age && out.page1[4].age >= out.page2[0].age,
      '$sort desc across pages',
    );
    r.ok(
      new Set([...out.page1, ...out.page2].map((d) => d._id)).size === 10,
      'paging does not repeat documents',
    );
    r.ok(out.grouped.length >= 2, '$group produced buckets');
    r.ok(
      out.excluded && !('tags' in out.excluded) && 'name' in out.excluded,
      '$project exclusion keeps other fields',
    );
    r.note(`groups=${JSON.stringify(out.grouped)}`);
  });

  await r.test('full-text search (BM25)', async (r) => {
    const out = await page.evaluate(async () => {
      const db = window.db;
      const posts = db.collection('posts');
      window.posts = posts;
      await posts.insertMany([
        { title: 'Local first databases', body: 'An embedded vector database in the browser' },
        { title: 'Vector search primer', body: 'Cosine similarity and embeddings explained' },
        { title: 'Cooking with rice', body: 'A recipe for garlic fried rice' },
      ]);
      await posts.createFtsIndex('body');
      const hits = await posts.searchText('body', 'vector database', 5);
      const filtered = await posts.searchText('body', 'rice', 5);
      const none = await posts.searchText('body', 'zzzznotaword', 5);
      return {
        hits: hits.map((h) => ({ t: h.document.title, s: h.score })),
        filtered: filtered.map((h) => h.document.title),
        none: none.length,
      };
    });
    r.ok(out.hits.length > 0, 'fts returns hits');
    r.ok(out.hits[0]?.s > 0, 'score is positive');
    r.eq(out.filtered, ['Cooking with rice'], 'fts term match');
    r.eq(out.none, 0, 'fts miss returns empty');
  });

  await r.test('vector search (flat index)', async (r) => {
    const out = await page.evaluate(async () => {
      const items = window.db.collection('items');
      window.items = items;
      const D = 32;
      const mk = (seed) => Array.from({ length: D }, (_, i) => Math.sin(seed * (i + 1)) * 0.5 + 0.5);
      const docs = Array.from({ length: 200 }, (_, i) => ({ idx: i, embedding: mk(i + 1) }));
      await items.insertMany(docs);
      await items.createVectorIndex('embedding', { dimensions: D, metric: 'cosine' });
      const q = mk(7);
      const t0 = performance.now();
      const near = await items.findNearest('embedding', q, 5);
      const ms = performance.now() - t0;
      const filtered = await items.findNearest('embedding', q, 5, { idx: { $lt: 3 } });
      return {
        top: near[0]?.document.idx,
        n: near.length,
        scores: near.map((x) => Math.round(x.score * 1000) / 1000),
        filteredIdx: filtered.map((x) => x.document.idx),
        ms: Math.round(ms),
      };
    });
    r.eq(out.top, 6, 'nearest neighbour is the query vector itself (idx 6 ⇒ seed 7)');
    r.eq(out.n, 5, 'topK honoured');
    r.ok(
      out.filteredIdx.every((i) => i < 3),
      'filtered vector search respects the filter',
    );
    r.note(`${out.ms}ms over 200×32`);
  });

  await r.test('collection shapes: document-only, vector-only, and mixed', async (r) => {
    const out = await page.evaluate(async () => {
      const D = 16;
      const mk = (s) => Array.from({ length: D }, (_, i) => Math.sin(s * (i + 1)));
      const db = window.db;

      // 1. Document-only — no vector index anywhere on the collection.
      const docsOnly = db.collection('shape_docs');
      await docsOnly.createIndex('n');
      await docsOnly.insertMany(Array.from({ length: 50 }, (_, i) => ({ n: i, s: `row ${i}` })));

      // 2. Vector-only — documents that carry nothing but an embedding.
      const vecOnly = db.collection('shape_vecs');
      await vecOnly.insertMany(Array.from({ length: 50 }, (_, i) => ({ emb: mk(i + 1) })));
      await vecOnly.createVectorIndex('emb', { dimensions: D, metric: 'cosine' });

      // 3. Mixed — scalars, text and an embedding in one document.
      const mixed = db.collection('shape_mixed');
      await mixed.insertMany(
        Array.from({ length: 50 }, (_, i) => ({
          n: i,
          city: ['Manila', 'Cebu'][i % 2],
          body: `document ${i} about ${['rust', 'vectors'][i % 2]}`,
          emb: mk(i + 1),
        })),
      );
      await mixed.createIndex('city');
      await mixed.createFtsIndex('body');
      await mixed.createVectorIndex('emb', { dimensions: D, metric: 'cosine' });

      const q = mk(7);
      const mixedKnn = await mixed.findNearest('emb', q, 3);
      const mixedFiltered = await mixed.findNearest('emb', q, 5, { city: 'Cebu' });

      // A mixed collection must serve all three query kinds over the same rows.
      const results = {
        docsOnlyFind: (await docsOnly.find({ n: { $gte: 45 } })).length,
        docsOnlyCount: await docsOnly.count(),
        vecOnlyKnn: (await vecOnly.findNearest('emb', q, 3)).map((h) => h.document.emb.length),
        vecOnlyFind: (await vecOnly.find()).length,
        vecOnlyFields: Object.keys((await vecOnly.findOne({})) ?? {}).sort(),
        mixedKnnTop: mixedKnn[0]?.document.n,
        mixedFilteredCities: [...new Set(mixedFiltered.map((h) => h.document.city))],
        mixedFts: (await mixed.searchText('body', 'vectors', 5)).length,
        mixedScalar: (await mixed.find({ city: 'Manila' })).length,
        mixedAgg: await mixed.aggregate([
          { $group: { _id: '$city', n: { $sum: 1 } } },
          { $sort: { _id: 1 } },
        ]),
      };

      // Updating a scalar on a mixed document must not disturb its embedding.
      await mixed.updateMany({ city: 'Cebu' }, { $set: { flagged: true } });
      const afterUpdate = await mixed.findNearest('emb', q, 3);
      results.knnStableAfterScalarUpdate =
        afterUpdate[0]?.document.n === mixedKnn[0]?.document.n &&
        JSON.stringify(afterUpdate[0]?.document.emb) === JSON.stringify(mixedKnn[0]?.document.emb);
      results.ftsStableAfterScalarUpdate = (await mixed.searchText('body', 'vectors', 5)).length;
      return results;
    });
    r.eq(out.docsOnlyFind, 5, 'document-only collection queries normally');
    r.eq(out.vecOnlyFind, 50, 'vector-only collection is still a document collection');
    r.eq(out.vecOnlyKnn, [16, 16, 16], 'vector-only knn returns the embeddings');
    r.eq(out.mixedKnnTop, 6, 'mixed collection knn finds the right row');
    r.eq(out.mixedFilteredCities, ['Cebu'], 'mixed collection knn respects a scalar filter');
    r.ok(out.mixedFts > 0, 'mixed collection full-text search works');
    r.eq(out.mixedScalar, 25, 'mixed collection scalar index works');
    r.eq(out.mixedAgg.length, 2, 'mixed collection aggregation works');
    r.ok(out.knnStableAfterScalarUpdate, 'a scalar update leaves the embedding intact');
    r.ok(out.ftsStableAfterScalarUpdate > 0, 'a scalar update leaves the text index intact');
  });

  await r.test('live query fires on same-tab writes', async (r) => {
    const out = await page.evaluate(async () => {
      const live = window.db.collection('live');
      const snapshots = [];
      const unsub = live.subscribe({ kind: 'x' }, (docs) => snapshots.push(docs.length));
      const wait = (ms) => new Promise((res) => setTimeout(res, ms));
      await wait(400);
      await live.insert({ kind: 'x', n: 1 });
      await wait(500);
      await live.insert({ kind: 'x', n: 2 });
      await wait(500);
      await live.deleteMany({ kind: 'x' });
      await wait(500);
      unsub();
      const after = snapshots.length;
      await live.insert({ kind: 'x', n: 3 });
      await wait(500);
      return { snapshots, leakedAfterUnsub: snapshots.length !== after };
    });
    r.eq(out.snapshots, [0, 1, 2, 0], 'live query saw every transition');
    r.eq(out.leakedAfterUnsub, false, 'unsubscribe stops delivery');
  });

  await r.test('persistence across reload (OPFS)', async (r) => {
    const before = await page.evaluate(async () => {
      const n = await window.users.count();
      await window.db.close();
      return n;
    });
    await page.reload({ waitUntil: 'load' });
    await page.waitForFunction('window.__ready === true');
    const after = await page.evaluate(async () => {
      const db = await window.taladb.openDB('happy.db');
      window.db = db;
      window.users = db.collection('users');
      window.posts = db.collection('posts');
      window.items = db.collection('items');
      return {
        users: await window.users.count(),
        indexes: await window.users.listIndexes(),
        collections: await db.listCollectionNames?.(),
      };
    });
    r.eq(after.users, before, 'documents survived the reload');
    r.ok(after.indexes.btree?.includes('age'), 'indexes survived the reload');
    r.note(`collections=${JSON.stringify(after.collections)}`);
  });
}
