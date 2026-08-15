// Phase 8 — mixed collections: scalars, text and vectors in the same documents,
// under all three index types at once.
//
// Phases 6 and 7 each test one half in isolation. The interesting failures live
// where they meet: index maintenance is per-field, so a write that touches one
// field must update exactly the indexes covering it — no more (that was a 16×
// slowdown) and no fewer (that is a silently wrong answer).
//
// The strongest check here is the rebuild comparison: an index maintained
// incrementally through a mixed workload must answer exactly like one rebuilt
// from the same documents afterwards. BM25 in particular carries corpus
// statistics as running deltas, so drift shows up as a changed ranking.

/** Run a page-side call and report either its value or its error message. */
const ATTEMPT = `async (fn) => { try { return { ok: await fn() }; } catch (e) { return { err: String(e.message ?? e) }; } }`;

/** Build the mixed corpus in the page. Deterministic: same rows every run. */
const SEED = `() => {
  const D = 8;
  const TOPIC = ['ledger', 'harvest', 'compiler', 'estuary'];
  const mk = (s) => Array.from({ length: D }, (_, i) => Math.sin(s * (i + 1)) * 0.5 + 0.5);
  return Array.from({ length: 60 }, (_, i) => ({
    ref: 'R' + String(i).padStart(3, '0'),
    lang: ['en', 'fr'][i % 2],
    tier: ['free', 'pro', 'team'][i % 3],
    score: i % 20,
    tags: ['t' + (i % 5), i % 2 ? 'odd' : 'even'],
    body: TOPIC[i % 4] + ' notes for record ' + i + ' about ' + TOPIC[(i + 1) % 4],
    emb: mk(i + 1),
  }));
}`;

export async function run(page, r, browser) {
  await page.evaluate(
    async (seedSrc) => {
      window.db = await window.taladb.openDB('mixed.db');
      window.seed = eval(seedSrc);
      window.D = 8;
      window.mk = (s) => Array.from({ length: 8 }, (_, i) => Math.sin(s * (i + 1)) * 0.5 + 0.5);
      /** Every query kind the collection supports, in one snapshot. */
      window.snapshot = async (c) => ({
        scalar: (await c.find({ tier: 'pro' })).map((d) => d.ref).sort(),
        range: (await c.find({ score: { $gte: 10, $lt: 15 } })).map((d) => d.ref).sort(),
        array: (await c.find({ tags: 't3' })).map((d) => d.ref).sort(),
        agg: await c.aggregate([
          { $group: { _id: '$tier', n: { $count: {} }, total: { $sum: '$score' } } },
          { $sort: { _id: 1 } },
        ]),
        text: (await c.searchText('body', 'compiler notes', 8)).map((h) => [
          h.document.ref,
          Math.round(h.score * 1e6),
        ]),
        contains: (await c.find({ body: { $contains: 'estuary' } })).length,
        vector: (await c.findNearest('emb', window.mk(7), 8)).map((h) => [
          h.document.ref,
          Math.round(h.score * 1e6),
        ]),
        hybrid: (
          await c.hybridSearch(
            { textField: 'body', text: 'compiler notes' },
            { vectorField: 'emb', vector: window.mk(7) },
            8,
          )
        ).map((h) => h.document.ref),
        count: await c.count(),
      });
    },
    SEED,
  );

  // --- building up ------------------------------------------------------------

  await r.test('adding an index type never changes another kind of answer', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.db.collection('mixed');
      await c.insertMany(window.seed().map((d) => ({ ...d })));

      // Scalar answers, before any index exists at all.
      const plain = {
        scalar: (await c.find({ tier: 'pro' })).map((d) => d.ref).sort(),
        range: (await c.find({ score: { $gte: 10, $lt: 15 } })).map((d) => d.ref).sort(),
        array: (await c.find({ tags: 't3' })).map((d) => d.ref).sort(),
        agg: await c.aggregate([
          { $group: { _id: '$tier', n: { $count: {} } }, },
          { $sort: { _id: 1 } },
        ]),
      };

      await c.createIndex('tier');
      await c.createIndex('score');
      await c.createIndex('tags');
      const afterBtree = {
        scalar: (await c.find({ tier: 'pro' })).map((d) => d.ref).sort(),
        range: (await c.find({ score: { $gte: 10, $lt: 15 } })).map((d) => d.ref).sort(),
        array: (await c.find({ tags: 't3' })).map((d) => d.ref).sort(),
        agg: await c.aggregate([
          { $group: { _id: '$tier', n: { $count: {} } }, },
          { $sort: { _id: 1 } },
        ]),
      };

      await c.createFtsIndex('body');
      const afterFts = {
        scalar: (await c.find({ tier: 'pro' })).map((d) => d.ref).sort(),
        range: (await c.find({ score: { $gte: 10, $lt: 15 } })).map((d) => d.ref).sort(),
        array: (await c.find({ tags: 't3' })).map((d) => d.ref).sort(),
        agg: await c.aggregate([
          { $group: { _id: '$tier', n: { $count: {} } }, },
          { $sort: { _id: 1 } },
        ]),
        text: (await c.searchText('body', 'compiler notes', 8)).map((h) => h.document.ref),
      };

      await c.createVectorIndex('emb', { dimensions: window.D, metric: 'cosine' });
      const afterVector = await window.snapshot(c);

      window.mixed = c;
      return { plain, afterBtree, afterFts, afterVector, indexes: await c.listIndexes() };
    });

    r.eq(out.afterBtree.scalar, out.plain.scalar, 'a b-tree index does not change a scalar answer');
    r.eq(out.afterBtree.range, out.plain.range, 'a b-tree index does not change a range answer');
    r.eq(out.afterBtree.array, out.plain.array, 'a b-tree index does not change containment');
    r.eq(out.afterBtree.agg, out.plain.agg, 'a b-tree index does not change an aggregation');
    r.eq(out.afterFts.scalar, out.plain.scalar, 'a text index does not disturb the scalar answers');
    r.eq(out.afterFts.agg, out.plain.agg, 'a text index does not disturb an aggregation');
    r.eq(
      out.afterVector.scalar,
      out.plain.scalar,
      'a vector index does not disturb the scalar answers',
    );
    r.eq(
      out.afterVector.text.map((t) => t[0]),
      out.afterFts.text,
      'a vector index does not disturb the text ranking',
    );
    r.ok(out.afterVector.text.length > 0, 'the text retriever actually returns hits');
    r.ok(out.afterVector.vector.length === 8, 'the vector retriever actually returns hits');
    r.eq(out.indexes.btree?.sort(), ['score', 'tags', 'tier'], 'all three b-tree indexes are listed');
    r.eq(out.indexes.fts, ['body'], 'the text index is listed');
    r.eq(out.indexes.vector, ['emb'], 'the vector index is listed');
  });

  // --- per-field maintenance --------------------------------------------------

  await r.test('a write updates exactly the indexes covering the fields it touched', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.mixed;
      const before = await window.snapshot(c);

      // 1. Touch a scalar only. Text and vector rankings must be untouched.
      await c.updateMany({ tier: 'free' }, { $set: { flagged: true } });
      const afterScalar = await window.snapshot(c);
      const flagged = (await c.find({ flagged: true })).length;

      // 2. Touch the text only, on one document. The vector ranking must not
      //    move, and the scalar answers must not move.
      await c.updateOne({ ref: 'R005' }, { $set: { body: 'estuary estuary estuary rewritten' } });
      const afterText = await window.snapshot(c);

      // 3. Touch the vector only, on one document. The text ranking must not
      //    move. R009 is given the query vector itself — note R006 already
      //    carries it (seed row i holds mk(i+1)), so the two tie at score 1 and
      //    the assertion below is about the pair, not a single winner.
      await c.updateOne({ ref: 'R009' }, { $set: { emb: window.mk(7) } });
      const afterVector = await window.snapshot(c);

      return { before, afterScalar, afterText, afterVector, flagged };
    });

    r.eq(out.flagged, 20, 'the scalar update touched the rows it claimed');
    r.eq(out.afterScalar.text, out.before.text, 'a scalar update leaves the text ranking identical');
    r.eq(
      out.afterScalar.vector,
      out.before.vector,
      'a scalar update leaves the vector ranking identical',
    );
    r.eq(out.afterScalar.hybrid, out.before.hybrid, 'a scalar update leaves hybrid identical');

    r.eq(
      out.afterText.vector,
      out.afterScalar.vector,
      'a text update leaves the vector ranking identical',
    );
    r.eq(out.afterText.scalar, out.before.scalar, 'a text update leaves the scalar answers identical');
    r.ok(
      JSON.stringify(out.afterText.text) !== JSON.stringify(out.afterScalar.text),
      'the text update did change the text ranking',
    );
    r.eq(out.afterText.contains, out.before.contains + 1, 'the rewritten body joined the $contains set');

    r.eq(
      out.afterVector.text,
      out.afterText.text,
      'a vector update leaves the text ranking identical',
    );
    r.eq(out.afterVector.scalar, out.before.scalar, 'a vector update leaves the scalar answers identical');
    r.eq(
      out.afterVector.vector.slice(0, 2).map((v) => v[0]).sort(),
      ['R006', 'R009'],
      'the updated vector joined the exact match at the top',
    );
    r.eq(
      out.afterVector.vector.slice(0, 2).map((v) => v[1]),
      [1e6, 1e6],
      'both exact matches score 1',
    );
    r.ok(
      !out.before.vector.slice(0, 2).map((v) => v[0]).includes('R009'),
      'R009 was not already at the top before the update',
    );
  });

  await r.test('a delete removes the document from every index at once', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.mixed;
      const target = await c.findOne({ ref: 'R012' });
      await c.deleteOne({ ref: 'R012' });
      const s = await window.snapshot(c);
      return {
        hadIt: !!target,
        count: s.count,
        inScalar: s.scalar.includes('R012'),
        inArray: s.array.includes('R012'),
        inText: s.text.some((t) => t[0] === 'R012'),
        inVector: s.vector.some((t) => t[0] === 'R012'),
        inHybrid: s.hybrid.includes('R012'),
        // A deleted document must not be reachable by a direct lookup either.
        found: await c.findOne({ ref: 'R012' }),
      };
    });
    r.eq(out.hadIt, true, 'the document existed before the delete');
    r.eq(out.count, 59, 'the count dropped by one');
    r.eq(out.found, null, 'the document is gone from the collection');
    r.eq(out.inScalar, false, 'gone from the scalar index');
    r.eq(out.inArray, false, 'gone from the array index');
    r.eq(out.inText, false, 'gone from the text index');
    r.eq(out.inVector, false, 'gone from the vector index');
    r.eq(out.inHybrid, false, 'gone from hybrid retrieval');
  });

  // --- the rebuild invariant --------------------------------------------------

  await r.test('incrementally maintained indexes match a rebuild from the documents', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.mixed;

      // A mixed workload: inserts, scalar updates, text rewrites, vector
      // rewrites and deletes, interleaved, all against live indexes.
      for (let i = 0; i < 20; i++) {
        await c.insert({
          ref: `X${String(i).padStart(3, '0')}`,
          lang: i % 2 ? 'en' : 'fr',
          tier: ['free', 'pro', 'team'][i % 3],
          score: i,
          tags: [`t${i % 5}`, 'extra'],
          body: `compiler ledger appendix ${i}`,
          emb: window.mk(100 + i),
        });
      }
      await c.updateMany({ tier: 'team' }, { $inc: { score: 1 } });
      await c.updateOne({ ref: 'X003' }, { $set: { body: 'harvest harvest appendix rewritten' } });
      await c.updateOne({ ref: 'X004' }, { $set: { emb: window.mk(3) } });
      await c.deleteMany({ ref: { $in: ['X001', 'X002', 'R020'] } });
      await c.updateMany({ tags: 'extra' }, { $push: { tags: 'late' } });

      const maintained = await window.snapshot(c);

      // Now drop every index and rebuild from the documents that remain. The
      // answers must be identical — including BM25 scores, whose corpus counts
      // were carried as running deltas through all of the above.
      await c.dropVectorIndex('emb');
      await c.dropFtsIndex('body');
      await c.dropIndex('tier');
      await c.dropIndex('score');
      await c.dropIndex('tags');

      await c.createIndex('tier');
      await c.createIndex('score');
      await c.createIndex('tags');
      await c.createFtsIndex('body');
      await c.createVectorIndex('emb', { dimensions: window.D, metric: 'cosine' });

      const rebuilt = await window.snapshot(c);
      const differing = Object.keys(maintained).filter(
        (k) => JSON.stringify(maintained[k]) !== JSON.stringify(rebuilt[k]),
      );
      return { maintained, rebuilt, differing };
    });

    r.eq(out.differing, [], 'every query kind agrees between maintained and rebuilt indexes');
    // 60 seeded, less R012 from the delete test, plus 20 inserts, less 3 deletes.
    r.eq(out.maintained.count, 76, 'the workload left the expected number of documents');
    r.ok(out.maintained.text.length > 0, 'the text comparison was not vacuous');
    r.ok(out.maintained.vector.length > 0, 'the vector comparison was not vacuous');
    r.ok(out.maintained.hybrid.length > 0, 'the hybrid comparison was not vacuous');
    r.note(`compared ${Object.keys(out.maintained).length} query kinds`);
  });

  // --- dropping one kind ------------------------------------------------------

  await r.test('dropping one index type leaves the other two working', async (r) => {
    const out = await page.evaluate(
      async (attemptSrc) => {
        const attempt = eval(attemptSrc);
        const c = window.mixed;
        const before = await window.snapshot(c);

        await c.dropVectorIndex('emb');
        const noVector = {
          scalar: (await c.find({ tier: 'pro' })).map((d) => d.ref).sort(),
          text: (await c.searchText('body', 'compiler notes', 8)).map((h) => [
            h.document.ref,
            Math.round(h.score * 1e6),
          ]),
          vector: await attempt(() => c.findNearest('emb', window.mk(7), 8)),
          hybrid: await attempt(() =>
            c.hybridSearch(
              { textField: 'body', text: 'compiler notes' },
              { vectorField: 'emb', vector: window.mk(7) },
              8,
            ),
          ),
        };

        await c.createVectorIndex('emb', { dimensions: window.D, metric: 'cosine' });
        await c.dropFtsIndex('body');
        const noText = {
          scalar: (await c.find({ tier: 'pro' })).map((d) => d.ref).sort(),
          vector: (await c.findNearest('emb', window.mk(7), 8)).map((h) => [
            h.document.ref,
            Math.round(h.score * 1e6),
          ]),
          text: await attempt(() => c.searchText('body', 'compiler notes', 8)),
        };

        await c.createFtsIndex('body');
        const restored = await window.snapshot(c);
        return { before, noVector, noText, restored };
      },
      ATTEMPT,
    );

    r.eq(out.noVector.scalar, out.before.scalar, 'dropping the vector index spares the scalar answers');
    r.eq(out.noVector.text, out.before.text, 'dropping the vector index spares the text ranking');
    r.ok(out.noVector.vector.err, 'vector search errors once its index is gone');
    r.ok(out.noVector.hybrid.err, 'hybrid search errors when its vector half is gone');
    r.eq(out.noText.scalar, out.before.scalar, 'dropping the text index spares the scalar answers');
    r.eq(out.noText.vector, out.before.vector, 'dropping the text index spares the vector ranking');
    r.ok(out.noText.text.err, 'text search errors once its index is gone');
    r.eq(out.restored, out.before, 'recreating both indexes restores every answer exactly');
  });

  // --- the shape an application actually uses ---------------------------------

  await r.test('a retrieval-shaped flow: filter, fuse, fetch, aggregate', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.mixed;
      const q = window.mk(7);

      // 1. Retrieve within a tenant/locale slice, the way a real RAG read does.
      const hits = await c.hybridSearch(
        { textField: 'body', text: 'compiler ledger' },
        { vectorField: 'emb', vector: q },
        5,
        { lang: 'en' },
      );

      // 2. Follow the top hit back to the full document by _id.
      const top = hits[0];
      const full = await c.findOne({ _id: top.document._id });

      // 3. Aggregate over the same slice the retrieval was scoped to.
      const byTier = await c.aggregate([
        { $match: { lang: 'en' } },
        { $group: { _id: '$tier', n: { $count: {} } } },
        { $sort: { _id: 1 } },
      ]);
      const slice = await c.count({ lang: 'en' });

      // 4. A projection that drops the embedding — what you send over a wire.
      const light = await c.aggregate([
        { $match: { ref: top.document.ref } },
        { $project: { emb: 0 } },
      ]);

      return {
        langs: [...new Set(hits.map((h) => h.document.lang))],
        n: hits.length,
        topRef: top?.document.ref,
        fullMatches: full?.ref === top?.document.ref,
        fullHasEmb: Array.isArray(full?.emb),
        byTier,
        slice,
        lightHasEmb: 'emb' in (light[0] ?? {}),
        lightHasBody: 'body' in (light[0] ?? {}),
        tierSum: byTier.reduce((a, g) => a + g.n, 0),
      };
    });
    r.eq(out.langs, ['en'], 'every retrieved document is inside the filtered slice');
    r.eq(out.n, 5, 'the retrieval returned a full page');
    r.eq(out.fullMatches, true, 'the top hit resolves back to its document by _id');
    r.eq(out.fullHasEmb, true, 'the fetched document still carries its embedding');
    r.eq(out.lightHasEmb, false, '$project exclusion drops the embedding');
    r.eq(out.lightHasBody, true, '$project exclusion keeps the other fields');
    r.eq(out.tierSum, out.slice, 'the aggregation covers exactly the filtered slice');
    r.note(`top=${out.topRef} slice=${out.slice} tiers=${JSON.stringify(out.byTier)}`);
  });

  // --- durability of the mixed shape ------------------------------------------

  await r.test('a mixed collection survives a reload with all three index types', async (r) => {
    const before = await page.evaluate(async () => {
      const out = await window.snapshot(window.mixed);
      out.indexes = await window.mixed.listIndexes();
      await window.db.close();
      return out;
    });
    await page.reload({ waitUntil: 'load' });
    await page.waitForFunction('window.__ready === true');
    const after = await page.evaluate(async () => {
      window.db = await window.taladb.openDB('mixed.db');
      window.D = 8;
      window.mk = (s) => Array.from({ length: 8 }, (_, i) => Math.sin(s * (i + 1)) * 0.5 + 0.5);
      window.snapshot = async (c) => ({
        scalar: (await c.find({ tier: 'pro' })).map((d) => d.ref).sort(),
        range: (await c.find({ score: { $gte: 10, $lt: 15 } })).map((d) => d.ref).sort(),
        array: (await c.find({ tags: 't3' })).map((d) => d.ref).sort(),
        agg: await c.aggregate([
          { $group: { _id: '$tier', n: { $count: {} }, total: { $sum: '$score' } } },
          { $sort: { _id: 1 } },
        ]),
        text: (await c.searchText('body', 'compiler notes', 8)).map((h) => [
          h.document.ref,
          Math.round(h.score * 1e6),
        ]),
        contains: (await c.find({ body: { $contains: 'estuary' } })).length,
        vector: (await c.findNearest('emb', window.mk(7), 8)).map((h) => [
          h.document.ref,
          Math.round(h.score * 1e6),
        ]),
        hybrid: (
          await c.hybridSearch(
            { textField: 'body', text: 'compiler notes' },
            { vectorField: 'emb', vector: window.mk(7) },
            8,
          )
        ).map((h) => h.document.ref),
        count: await c.count(),
      });
      window.mixed = window.db.collection('mixed');
      const out = await window.snapshot(window.mixed);
      out.indexes = await window.mixed.listIndexes();
      return out;
    });
    const differing = Object.keys(before).filter(
      (k) => JSON.stringify(before[k]) !== JSON.stringify(after[k]),
    );
    r.eq(differing, [], 'every query kind answers identically after the reload');
    r.eq(after.indexes.vector, ['emb'], 'the vector index survived');
    r.eq(after.indexes.fts, ['body'], 'the text index survived');
    r.eq(after.indexes.btree?.sort(), ['score', 'tags', 'tier'], 'the b-tree indexes survived');
  });

  await r.test('a mixed write forwarded from a second tab lands in every index', async (r) => {
    const B = await browser.newPage();
    try {
      await B.goto(page.url(), { waitUntil: 'load' });
      await B.waitForFunction('window.__ready === true');
      await B.evaluate(async () => {
        window.db = await window.taladb.openDB('mixed.db');
        window.mixed = window.db.collection('mixed');
        window.mk = (s) => Array.from({ length: 8 }, (_, i) => Math.sin(s * (i + 1)) * 0.5 + 0.5);
      });

      const roles = {
        a: await page.evaluate(() => window.db.isPrimary()),
        b: await B.evaluate(() => window.db.isPrimary()),
      };

      // One document carrying all three kinds of indexed field, written by the
      // tab that does not own the file.
      await B.evaluate(() =>
        window.mixed.insert({
          ref: 'FWD1',
          lang: 'en',
          tier: 'pro',
          score: 11,
          tags: ['t3', 'forwarded'],
          body: 'compiler notes forwarded across a tab boundary',
          emb: window.mk(7),
        }),
      );

      const deadline = Date.now() + 5000;
      let seen = null;
      for (;;) {
        seen = await page.evaluate(async () => {
          const c = window.mixed;
          return {
            scalar: (await c.find({ tags: 'forwarded' })).map((d) => d.ref),
            text: (await c.searchText('body', 'forwarded boundary', 5)).map((h) => h.document.ref),
            vector: (await c.findNearest('emb', window.mk(7), 3)).map((h) => h.document.ref),
            range: (await c.find({ score: { $gte: 10, $lt: 15 } })).map((d) => d.ref),
          };
        });
        if (seen.scalar.includes('FWD1') || Date.now() > deadline) break;
        await new Promise((res) => setTimeout(res, 100));
      }

      r.eq(roles.a, true, 'the original tab owns the file');
      r.eq(roles.b, false, 'the second tab forwards its writes');
      r.eq(seen.scalar, ['FWD1'], 'the forwarded document is in the array index');
      r.ok(seen.range.includes('FWD1'), 'the forwarded document is in the range index');
      r.ok(seen.text.includes('FWD1'), 'the forwarded document is in the text index');
      r.ok(seen.vector.includes('FWD1'), 'the forwarded document is in the vector index');
    } finally {
      await B.close();
    }
  });
}
