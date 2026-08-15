// Phase 6 — the vector database, end to end in a real browser.
//
// Phase 1 proves a flat cosine index answers a query. This phase covers the rest
// of the surface TalaDB is positioned on: the three metrics, the write paths that
// keep an index honest after the data moves, hybrid retrieval, and the two places
// a vector index can silently stop existing — a reload, and a second tab.
import { newTab, sleep } from './lib.mjs';

/** Run a page-side call and report either its value or its error message. */
const ATTEMPT = `async (fn) => { try { return { ok: await fn() }; } catch (e) { return { err: String(e.message ?? e) }; } }`;

/** Poll a page-side expression until it matches, or time out. */
async function until(page, fn, predicate, timeout = 4000) {
  const deadline = Date.now() + timeout;
  let last;
  for (;;) {
    last = await page.evaluate(fn);
    if (predicate(last)) return last;
    if (Date.now() > deadline) return last;
    await sleep(100);
  }
}

export async function run(page, r, browser) {
  await page.evaluate(async () => {
    window.db = await window.taladb.openDB('vectors.db');
  });

  // --- metrics ----------------------------------------------------------------

  await r.test('cosine, dot and euclidean rank the same data differently', async (r) => {
    const out = await page.evaluate(async () => {
      // Four vectors chosen so that every metric disagrees about the winner:
      // `a` is the query itself, `d` points the same way but ten times longer,
      // `c` is 45° off, `b` is orthogonal. Cosine cannot separate a from d,
      // dot prefers d for its magnitude, euclidean punishes d for the distance.
      const docs = [
        { k: 'a', emb: [1, 0, 0, 0] },
        { k: 'b', emb: [0, 1, 0, 0] },
        { k: 'c', emb: [1, 1, 0, 0] },
        { k: 'd', emb: [10, 0, 0, 0] },
      ];
      const q = [1, 0, 0, 0];
      const rank = async (metric) => {
        const c = window.db.collection(`metric_${metric}`);
        await c.insertMany(docs.map((d) => ({ ...d })));
        await c.createVectorIndex('emb', { dimensions: 4, metric });
        const hits = await c.findNearest('emb', q, 4);
        return {
          order: hits.map((h) => h.document.k),
          scores: hits.map((h) => Math.round(h.score * 1000) / 1000),
        };
      };
      return {
        cosine: await rank('cosine'),
        dot: await rank('dot'),
        euclidean: await rank('euclidean'),
      };
    });

    // Cosine ignores magnitude: a and d are the same direction, both score 1.
    r.eq(out.cosine.order.slice(0, 2).sort(), ['a', 'd'], 'cosine ties the two collinear vectors');
    r.eq(out.cosine.scores.slice(0, 2), [1, 1], 'a collinear vector scores exactly 1');
    r.eq(out.cosine.order[3], 'b', 'cosine puts the orthogonal vector last');

    // Dot is magnitude-sensitive, so the long vector wins outright.
    r.eq(out.dot.order[0], 'd', 'dot prefers the longer collinear vector');
    r.eq(out.dot.scores[0], 10, 'dot score is the raw inner product');
    r.eq(out.dot.order[3], 'b', 'dot puts the orthogonal vector last');

    // Euclidean is distance-based: the exact match wins and d is now the worst.
    r.eq(out.euclidean.order[0], 'a', 'euclidean prefers the exact match');
    r.eq(out.euclidean.scores[0], 1, 'a zero-distance match scores 1');
    r.eq(out.euclidean.order[3], 'd', 'euclidean puts the distant vector last');

    r.note(
      `cosine=${out.cosine.order.join('')} dot=${out.dot.order.join('')} euclidean=${out.euclidean.order.join('')}`,
    );
  });

  // --- write paths ------------------------------------------------------------

  await r.test('insert, update and delete stay visible to findNearest', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.db.collection('vec_writes');
      const q = [1, 0, 0, 0];
      await c.insertMany([
        { k: 'far', emb: [0, 1, 0, 0] },
        { k: 'mid', emb: [0.7, 0.7, 0, 0] },
      ]);
      await c.createVectorIndex('emb', { dimensions: 4, metric: 'cosine' });

      const top = async () => (await c.findNearest('emb', q, 5)).map((h) => h.document.k);
      const initial = await top();

      // A document inserted after the index exists must be indexed on the way in.
      await c.insert({ k: 'new', emb: [1, 0, 0, 0] });
      const afterInsert = await top();

      // Updating the embedding must move the document, not leave a stale entry.
      await c.updateMany({ k: 'far' }, { $set: { emb: [0.99, 0.14, 0, 0] } });
      const afterUpdate = await top();
      const farRank = afterUpdate.indexOf('far');

      // A document that is deleted must leave the index entirely.
      await c.deleteMany({ k: 'new' });
      const afterDelete = await top();

      // Clearing the embedding to null must retire it from the index while the
      // document itself survives as an ordinary row.
      await c.updateMany({ k: 'mid' }, { $set: { emb: null } });
      const afterNull = await top();

      return {
        initial,
        afterInsert,
        afterUpdate,
        farRank,
        afterDelete,
        afterNull,
        rows: await c.count(),
      };
    });

    r.eq(out.initial, ['mid', 'far'], 'backfilled documents rank by similarity');
    r.eq(out.afterInsert[0], 'new', 'a post-index insert is searchable immediately');
    r.eq(out.afterInsert.length, 3, 'the insert did not displace the existing entries');
    r.eq(out.farRank, 1, 'an updated embedding moves to its new position');
    r.ok(!out.afterDelete.includes('new'), 'a deleted document leaves the index');
    r.eq(out.afterDelete.length, 2, 'exactly one entry was removed');
    r.ok(!out.afterNull.includes('mid'), 'nulling the embedding retires the index entry');
    r.eq(out.rows, 2, 'the nulled document is still a row in the collection');
  });

  await r.test('backfill and incremental indexing produce the same ranking', async (r) => {
    const out = await page.evaluate(async () => {
      const D = 12;
      const mk = (s) => Array.from({ length: D }, (_, i) => Math.sin(s * (i + 1)));
      const docs = Array.from({ length: 60 }, (_, i) => ({ n: i, emb: mk(i + 1) }));
      const q = mk(9);

      // Index last: every vector arrives through the backfill in createVectorIndex.
      const backfilled = window.db.collection('vec_backfilled');
      await backfilled.insertMany(docs.map((d) => ({ ...d })));
      await backfilled.createVectorIndex('emb', { dimensions: D, metric: 'cosine' });

      // Index first: every vector arrives through the insert path instead.
      const incremental = window.db.collection('vec_incremental');
      await incremental.createVectorIndex('emb', { dimensions: D, metric: 'cosine' });
      await incremental.insertMany(docs.map((d) => ({ ...d })));

      const shape = async (c) =>
        (await c.findNearest('emb', q, 10)).map((h) => [h.document.n, Math.round(h.score * 1e6)]);
      return { backfilled: await shape(backfilled), incremental: await shape(incremental) };
    });
    r.eq(out.backfilled[0]?.[0], 8, 'backfilled index finds the query vector itself');
    r.eq(
      out.incremental,
      out.backfilled,
      'the two index build orders answer identically',
    );
  });

  await r.test('dropVectorIndex removes search but keeps the documents', async (r) => {
    const out = await page.evaluate(
      async (attemptSrc) => {
        const attempt = eval(attemptSrc);
        const c = window.db.collection('vec_drop');
        const q = [1, 0, 0, 0];
        await c.insertMany([
          { k: 'a', emb: [1, 0, 0, 0] },
          { k: 'b', emb: [0, 1, 0, 0] },
        ]);
        await c.createVectorIndex('emb', { dimensions: 4, metric: 'cosine' });
        const before = (await c.listIndexes()).vector;

        await c.dropVectorIndex('emb');
        const after = (await c.listIndexes()).vector;
        const searchAfterDrop = await attempt(() => c.findNearest('emb', q, 2));
        const rowsAfterDrop = await c.count();
        const docAfterDrop = await c.findOne({ k: 'a' });

        // Recreating must rebuild from the documents that were left behind.
        await c.createVectorIndex('emb', { dimensions: 4, metric: 'cosine' });
        const recreated = (await c.findNearest('emb', q, 2)).map((h) => h.document.k);
        return { before, after, searchAfterDrop, rowsAfterDrop, docAfterDrop, recreated };
      },
      ATTEMPT,
    );
    r.eq(out.before, ['emb'], 'the index is listed while it exists');
    r.eq(out.after, [], 'the index is delisted after the drop');
    r.ok(out.searchAfterDrop.err, 'findNearest errors once the index is gone');
    r.eq(out.rowsAfterDrop, 2, 'dropping the index keeps every document');
    r.ok(Array.isArray(out.docAfterDrop?.emb), 'the embedding field survives the drop');
    r.eq(out.recreated, ['a', 'b'], 'recreating the index rebuilds it from the documents');
  });

  // --- hybrid retrieval -------------------------------------------------------

  // A corpus built so the two retrievers disagree on purpose. `kw` carries the
  // rare query token but points away from the query vector; `vec` is the query
  // vector but shares no vocabulary with it; `both` is decent at each. Fusion
  // exists precisely to surface `both` over two specialists.
  const seedHybrid = async () =>
    page.evaluate(async () => {
      const c = window.db.collection('hybrid');
      await c.insertMany([
        { k: 'kw', lang: 'en', body: 'the zkrollup prover changelog', emb: [0, 1, 0, 0] },
        { k: 'vec', lang: 'en', body: 'unrelated prose about kitchen utensils', emb: [1, 0, 0, 0] },
        { k: 'both', lang: 'en', body: 'a zkrollup guide', emb: [0.92, 0.39, 0, 0] },
        { k: 'noise', lang: 'en', body: 'nothing to see in this row at all', emb: [0, 0, 1, 0] },
        { k: 'other', lang: 'fr', body: 'zkrollup zkrollup zkrollup', emb: [1, 0, 0, 0] },
      ]);
      await c.createFtsIndex('body');
      await c.createVectorIndex('emb', { dimensions: 4, metric: 'cosine' });
      window.hybrid = c;
    });

  await r.test('hybrid search fuses the keyword and vector rankings', async (r) => {
    await seedHybrid();
    const out = await page.evaluate(async () => {
      const c = window.hybrid;
      const q = [1, 0, 0, 0];
      const fused = await c.hybridSearch(
        { textField: 'body', text: 'zkrollup guide' },
        { vectorField: 'emb', vector: q },
        4,
      );
      return {
        order: fused.map((h) => h.document.k),
        ranks: fused.map((h) => [h.document.k, h.textRank, h.vectorRank]),
        scores: fused.map((h) => h.score),
        textOnly: (await c.searchText('body', 'zkrollup guide', 4)).map((h) => h.document.k),
        vectorOnly: (await c.findNearest('emb', q, 4)).map((h) => h.document.k),
      };
    });

    r.eq(out.order[0], 'both', 'the document both retrievers rank well comes first');
    const both = out.ranks.find((x) => x[0] === 'both');
    r.ok(
      both && both[1] !== null && both[2] !== null,
      'a fused hit reports a rank from each retriever',
    );
    // `vec` shares no vocabulary with the query, so it can only have arrived
    // through the vector side — its text rank must be null rather than a
    // fabricated position.
    const vec = out.ranks.find((x) => x[0] === 'vec');
    r.ok(vec, 'a vector-only match is still fused into the results');
    r.eq(vec?.[1], null, 'a document the text retriever missed reports a null text rank');
    r.ok(typeof vec?.[2] === 'number', 'that same document reports its vector rank');
    r.ok(
      out.scores.every((s, i) => i === 0 || s <= out.scores[i - 1]),
      'fused scores descend',
    );
    r.ok(out.scores[0] > 0, 'the fused score is positive');
    r.note(`fused=${out.order.join(',')} text=${out.textOnly.join(',')} vec=${out.vectorOnly.join(',')}`);
  });

  await r.test('hybrid weights collapse onto each single retriever', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.hybrid;
      const q = [1, 0, 0, 0];
      const text = { textField: 'body', text: 'zkrollup guide' };
      const vector = { vectorField: 'emb', vector: q };
      const ks = (hits) => hits.map((h) => h.document.k);
      return {
        vectorOnly: ks(await c.hybridSearch(text, vector, 3, undefined, { textWeight: 0 })),
        pureVector: ks(await c.findNearest('emb', q, 3)),
        textOnly: ks(await c.hybridSearch(text, vector, 3, undefined, { vectorWeight: 0 })),
        pureText: ks(await c.searchText('body', 'zkrollup guide', 3)),
        // rrfK flattens the advantage of the very top ranks but must not reorder
        // a result set where one document leads on both sides.
        bigK: ks(await c.hybridSearch(text, vector, 3, undefined, { rrfK: 1000 })),
        smallK: ks(await c.hybridSearch(text, vector, 3, undefined, { rrfK: 1 })),
      };
    });
    r.eq(out.vectorOnly, out.pureVector, 'textWeight 0 reproduces the vector ranking');
    r.eq(out.textOnly, out.pureText, 'vectorWeight 0 reproduces the text ranking');
    r.eq(out.bigK[0], 'both', 'a large rrfK keeps the doubly-ranked document first');
    r.eq(out.smallK[0], 'both', 'a small rrfK keeps the doubly-ranked document first');
  });

  await r.test('a hybrid filter applies to both retrievers', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.hybrid;
      const q = [1, 0, 0, 0];
      const hits = await c.hybridSearch(
        { textField: 'body', text: 'zkrollup' },
        { vectorField: 'emb', vector: q },
        5,
        { lang: 'fr' },
      );
      const unfiltered = await c.hybridSearch(
        { textField: 'body', text: 'zkrollup' },
        { vectorField: 'emb', vector: q },
        5,
      );
      return {
        langs: [...new Set(hits.map((h) => h.document.lang))],
        keys: hits.map((h) => h.document.k),
        unfilteredKeys: unfiltered.map((h) => h.document.k),
      };
    });
    // `other` is French: it is the strongest keyword match and tied-best on the
    // vector side, so it would dominate unfiltered. Under the filter it must be
    // the only survivor — a filter honoured by just one retriever would leak the
    // English rows back in through the other.
    r.eq(out.langs, ['fr'], 'every fused hit satisfies the filter');
    r.eq(out.keys, ['other'], 'only the matching document survives the filter');
    r.ok(out.unfilteredKeys.length > 1, 'the same query is broader without the filter');
  });

  // --- durability -------------------------------------------------------------

  await r.test('vectors and their indexes survive a reload (OPFS)', async (r) => {
    const before = await page.evaluate(async () => {
      const c = window.db.collection('vec_writes');
      const out = {
        writes: (await c.findNearest('emb', [1, 0, 0, 0], 5)).map((h) => h.document.k),
        hybrid: (
          await window.hybrid.hybridSearch(
            { textField: 'body', text: 'zkrollup guide' },
            { vectorField: 'emb', vector: [1, 0, 0, 0] },
            4,
          )
        ).map((h) => h.document.k),
        indexes: await window.hybrid.listIndexes(),
      };
      await window.db.close();
      return out;
    });

    await page.reload({ waitUntil: 'load' });
    await page.waitForFunction('window.__ready === true');

    const after = await page.evaluate(async () => {
      const db = await window.taladb.openDB('vectors.db');
      window.db = db;
      window.hybrid = db.collection('hybrid');
      const c = db.collection('vec_writes');
      return {
        writes: (await c.findNearest('emb', [1, 0, 0, 0], 5)).map((h) => h.document.k),
        hybrid: (
          await window.hybrid.hybridSearch(
            { textField: 'body', text: 'zkrollup guide' },
            { vectorField: 'emb', vector: [1, 0, 0, 0] },
            4,
          )
        ).map((h) => h.document.k),
        indexes: await window.hybrid.listIndexes(),
        // A vector written after the reload must land in the reloaded index.
        appended: await (async () => {
          await c.insert({ k: 'post-reload', emb: [1, 0, 0, 0] });
          return (await c.findNearest('emb', [1, 0, 0, 0], 1)).map((h) => h.document.k);
        })(),
      };
    });

    r.eq(after.writes, before.writes, 'the vector ranking is identical after the reload');
    r.eq(after.hybrid, before.hybrid, 'the hybrid ranking is identical after the reload');
    r.eq(after.indexes.vector, before.indexes.vector, 'the vector index survived');
    r.eq(after.indexes.fts, before.indexes.fts, 'the text index survived');
    r.eq(after.appended, ['post-reload'], 'the reloaded index accepts new vectors');
  });

  // --- multi-tab --------------------------------------------------------------

  await r.test('vector writes from a second tab reach the owner index', async (r) => {
    const A = page;
    const B = await newTab(browser, { label: 'B' });
    try {
      await A.evaluate(async () => {
        const c = window.db.collection('vec_tabs');
        await c.createVectorIndex('emb', { dimensions: 4, metric: 'cosine' });
        await c.insert({ k: 'a-owned', emb: [0, 1, 0, 0] });
        await window.db.flush?.();
        window.tabs = c;
      });

      await B.evaluate(async () => {
        window.db = await window.taladb.openDB('vectors.db');
        window.tabs = window.db.collection('vec_tabs');
      });

      const roles = {
        a: await A.evaluate(() => window.db.isPrimary()),
        b: await B.evaluate(() => window.db.isPrimary()),
      };

      // A single-document insert from a non-owner tab is the exact shape that
      // used to be swallowed on the forwarding path.
      await B.evaluate(() => window.tabs.insert({ k: 'b-forwarded', emb: [1, 0, 0, 0] }));

      const seenByOwner = await until(
        A,
        async () => (await window.tabs.findNearest('emb', [1, 0, 0, 0], 3)).map((h) => h.document.k),
        (ks) => ks.includes('b-forwarded'),
      );
      const seenByWriter = await until(
        B,
        async () => (await window.tabs.findNearest('emb', [1, 0, 0, 0], 3)).map((h) => h.document.k),
        (ks) => ks.includes('b-forwarded'),
      );

      r.eq(roles.a, true, 'the first tab owns the OPFS file');
      r.eq(roles.b, false, 'the second tab forwards its writes');
      r.eq(seenByOwner[0], 'b-forwarded', 'the forwarded vector is indexed in the owner tab');
      r.ok(seenByWriter.includes('b-forwarded'), 'the writing tab sees its own vector');
      r.eq(seenByOwner.length, 2, 'no document was duplicated by the forwarding');
    } finally {
      await B.close();
    }
  });
}
