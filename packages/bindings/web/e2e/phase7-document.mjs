// Phase 7 — the document database, beyond the happy path.
//
// Phase 1 covers the CRUD spine and the common operators. This phase takes the
// rest of the document surface: the filter and update operators nothing else
// exercises, every `$group` accumulator, the index lifecycle, live aggregation,
// schema validation, the migration runner, and compaction.
//
// The load-bearing test here is "an index changes the plan, never the answer" —
// a wrong index is the one document-database bug that looks like success.

/** Run a page-side call and report either its value or its error message. */
const ATTEMPT = `async (fn) => { try { return { ok: await fn() }; } catch (e) { return { err: String(e.message ?? e) }; } }`;

/**
 * A catalogue small enough to reason about by hand — every expectation below is
 * counted off this list, not computed by re-implementing the query engine.
 *
 *   sku  cat    qty  price  tags               body
 *   A1   tools    5     10  metal, hand        a steel hammer for framing
 *   A2   tools    0     25  metal, power       a cordless power drill
 *   A3   tools   12      7  wood               a wooden mallet
 *   B1   paint    3     40  liquid             white emulsion paint
 *   B2   paint    8     15  liquid, spray      red spray paint
 *   C1   seeds  100      2  garden             tomato seeds for planting
 *   D1   misc     1      -  (none)             an item with no price at all
 */
const SEED = [
  { sku: 'A1', cat: 'tools', qty: 5, price: 10, tags: ['metal', 'hand'], body: 'a steel hammer for framing' },
  { sku: 'A2', cat: 'tools', qty: 0, price: 25, tags: ['metal', 'power'], body: 'a cordless power drill' },
  { sku: 'A3', cat: 'tools', qty: 12, price: 7, tags: ['wood'], body: 'a wooden mallet' },
  { sku: 'B1', cat: 'paint', qty: 3, price: 40, tags: ['liquid'], body: 'white emulsion paint' },
  { sku: 'B2', cat: 'paint', qty: 8, price: 15, tags: ['liquid', 'spray'], body: 'red spray paint' },
  { sku: 'C1', cat: 'seeds', qty: 100, price: 2, tags: ['garden'], body: 'tomato seeds for planting' },
  { sku: 'D1', cat: 'misc', qty: 1, tags: [], body: 'an item with no price at all' },
];

export async function run(page, r) {
  await page.evaluate(async (seed) => {
    window.db = await window.taladb.openDB('documents.db');
    window.SEED = seed;
    const c = window.db.collection('catalogue');
    await c.insertMany(seed.map((d) => ({ ...d })));
    await c.createFtsIndex('body');
    window.cat = c;
  }, SEED);

  // --- filter DSL -------------------------------------------------------------

  await r.test('the filter operators the happy path leaves out', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.cat;
      const skus = async (f) => (await c.find(f)).map((d) => d.sku).sort();
      return {
        nin: await skus({ cat: { $nin: ['tools', 'paint'] } }),
        regex: await skus({ sku: { $regex: '^A' } }),
        regexAnchoredEnd: await skus({ sku: { $regex: '1$' } }),
        not: await skus({ $not: { cat: 'tools' } }),
        lte: await skus({ price: { $lte: 10 } }),
        existsFalse: await skus({ price: { $exists: false } }),
        // `$contains` is a boolean gate over an FTS index: every token must be
        // present, unlike searchText's OR-with-ranking.
        containsOne: await skus({ body: { $contains: 'paint' } }),
        containsAll: await skus({ body: { $contains: 'spray paint' } }),
        containsMiss: await skus({ body: { $contains: 'paint hammer' } }),
        // Combining an operator with a plain equality on another field.
        compound: await skus({ cat: 'tools', qty: { $gt: 4 } }),
        notNested: await skus({ $not: { tags: 'liquid' } }),
      };
    });
    r.eq(out.nin, ['C1', 'D1'], '$nin excludes every listed value');
    r.eq(out.regex, ['A1', 'A2', 'A3'], '$regex anchors at the start');
    r.eq(out.regexAnchoredEnd, ['A1', 'B1', 'C1', 'D1'], '$regex anchors at the end');
    r.eq(out.not, ['B1', 'B2', 'C1', 'D1'], '$not inverts a filter');
    r.eq(out.lte, ['A1', 'A3', 'C1'], '$lte is inclusive');
    r.eq(out.existsFalse, ['D1'], '$exists:false finds the missing field');
    r.eq(out.containsOne, ['B1', 'B2'], '$contains matches a single token');
    r.eq(out.containsAll, ['B2'], '$contains requires every token (AND)');
    r.eq(out.containsMiss, [], '$contains with an unsatisfiable token pair matches nothing');
    r.eq(out.compound, ['A1', 'A3'], 'two field conditions intersect');
    r.eq(out.notNested, ['A1', 'A2', 'A3', 'C1', 'D1'], '$not applies to array containment too');
  });

  await r.test('$push and $pull maintain array fields', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.cat;
      const tags = async (sku) => (await c.findOne({ sku })).tags;

      const pushed = await c.updateMany({ cat: 'tools' }, { $push: { tags: 'sale' } });
      const afterPush = { a1: await tags('A1'), a3: await tags('A3'), b1: await tags('B1') };

      // Pushing onto a field that is an empty array must still append.
      await c.updateOne({ sku: 'D1' }, { $push: { tags: 'clearance' } });
      const d1 = await tags('D1');

      // A push is queryable through the same array-containment path as a seeded
      // element — the index has to have been updated, not just the document.
      const onSale = (await c.find({ tags: 'sale' })).map((d) => d.sku).sort();

      const pulled = await c.updateMany({ cat: 'tools' }, { $pull: { tags: 'sale' } });
      const afterPull = { a1: await tags('A1'), onSale: (await c.find({ tags: 'sale' })).length };

      // Pulling a value that is not there is a no-op, not an error.
      const pullMissing = await c.updateOne({ sku: 'A1' }, { $pull: { tags: 'nope' } });
      return { pushed, afterPush, d1, onSale, pulled, afterPull, pullMissing, a1: await tags('A1') };
    });
    r.eq(out.pushed, 3, '$push reports every document it touched');
    r.eq(out.afterPush.a1, ['metal', 'hand', 'sale'], '$push appends to the end');
    r.eq(out.afterPush.a3, ['wood', 'sale'], '$push appends on a shorter array');
    r.eq(out.afterPush.b1, ['liquid'], '$push left non-matching documents alone');
    r.eq(out.d1, ['clearance'], '$push onto an empty array appends');
    r.eq(out.onSale, ['A1', 'A2', 'A3'], 'a pushed element is queryable by containment');
    r.eq(out.pulled, 3, '$pull reports every document it touched');
    r.eq(out.afterPull.a1, ['metal', 'hand'], '$pull removes the element');
    r.eq(out.afterPull.onSale, 0, 'the pulled element is gone from the index too');
    r.eq(out.a1, ['metal', 'hand'], 'pulling an absent element changes nothing');
  });

  // --- aggregation ------------------------------------------------------------

  await r.test('every $group accumulator', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.cat;
      const grouped = await c.aggregate([
        {
          $group: {
            _id: '$cat',
            n: { $count: {} },
            total: { $sum: '$qty' },
            avg: { $avg: '$qty' },
            min: { $min: '$qty' },
            max: { $max: '$qty' },
            all: { $push: '$sku' },
            first: { $first: '$sku' },
            last: { $last: '$sku' },
          },
        },
        { $sort: { _id: 1 } },
      ]);
      const whole = await c.aggregate([
        { $group: { _id: null, n: { $count: {} }, max: { $max: '$price' }, min: { $min: '$price' } } },
      ]);
      return { grouped, whole: whole[0] };
    });
    const tools = out.grouped.find((g) => g._id === 'tools');
    r.eq(out.grouped.map((g) => g._id), ['misc', 'paint', 'seeds', 'tools'], '$sort orders the groups');
    r.eq(tools?.n, 3, '$count counts the bucket');
    r.eq(tools?.total, 17, '$sum over a field (5+0+12)');
    r.ok(Math.abs(tools?.avg - 17 / 3) < 1e-9, '$avg over a field');
    r.eq(tools?.min, 0, '$min keeps a zero rather than treating it as absent');
    r.eq(tools?.max, 12, '$max over a field');
    r.eq(tools?.all, ['A1', 'A2', 'A3'], '$push collects the bucket in insertion order');
    r.eq(tools?.first, 'A1', '$first takes the leading document');
    r.eq(tools?.last, 'A3', '$last takes the trailing document');
    r.eq(out.whole?._id, null, 'a null _id groups the whole collection');
    r.eq(out.whole?.n, 7, 'the whole-collection bucket counts every document');
    r.eq(out.whole?.max, 40, '$max ignores the document missing the field');
    r.eq(out.whole?.min, 2, '$min ignores the document missing the field');
  });

  await r.test('subscribeAggregate delivers a live grouped result', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.db.collection('live_agg');
      await c.insertMany([
        { team: 'red', pts: 1 },
        { team: 'blue', pts: 2 },
      ]);
      const snapshots = [];
      const unsub = c.subscribeAggregate(
        [{ $group: { _id: '$team', pts: { $sum: '$pts' } } }, { $sort: { _id: 1 } }],
        (docs) => snapshots.push(docs.map((d) => `${d._id}:${d.pts}`).join(',')),
      );
      const settle = () => new Promise((res) => setTimeout(res, 600));
      await settle();
      await c.insert({ team: 'red', pts: 4 });
      await settle();
      await c.updateMany({ team: 'blue' }, { $inc: { pts: 10 } });
      await settle();
      unsub();
      const afterUnsub = snapshots.length;
      await c.insert({ team: 'red', pts: 100 });
      await settle();
      return { snapshots, leaked: snapshots.length > afterUnsub };
    });
    r.ok(out.snapshots.length >= 3, 'the subscription fired for the seed and both writes');
    r.eq(out.snapshots[0], 'blue:2,red:1', 'the first snapshot is the current grouping');
    r.eq(out.snapshots.at(-1), 'blue:12,red:5', 'the last snapshot reflects both writes');
    r.eq(out.leaked, false, 'unsubscribe stops delivery');
    r.note(`snapshots=${JSON.stringify(out.snapshots)}`);
  });

  // --- indexes ----------------------------------------------------------------

  await r.test('an index changes the plan, never the answer', async (r) => {
    const out = await page.evaluate(async () => {
      // The same rows in two collections: one fully indexed, one with no index
      // at all. Every filter must return the same skus from both. This is the
      // invariant a wrong index breaks silently — the query still succeeds.
      const rows = Array.from({ length: 120 }, (_, i) => {
        const row = {
          sku: `S${String(i).padStart(3, '0')}`,
          cat: ['tools', 'paint', 'seeds'][i % 3],
          qty: i % 17,
          price: (i * 7) % 50,
          tags: [`t${i % 5}`, i % 2 ? 'odd' : 'even'],
          nested: { city: ['Manila', 'Cebu'][i % 2], score: i / 3 },
        };
        // Three states for `rank`, because null and absent are different things
        // and a filter battery that only ever sees one of them cannot tell an
        // index that conflates them from one that does not.
        if (i % 2 === 0) row.rank = i;
        else if (i % 4 === 1) row.rank = null;
        return row;
      });
      const bare = window.db.collection('inv_bare');
      const indexed = window.db.collection('inv_indexed');
      await bare.insertMany(rows.map((d) => ({ ...d })));
      await indexed.insertMany(rows.map((d) => ({ ...d })));
      await indexed.createIndex('cat');
      await indexed.createIndex('qty');
      await indexed.createIndex('price');
      await indexed.createIndex('tags');
      await indexed.createIndex('rank');
      await indexed.createCompoundIndex(['cat', 'qty']);

      const filters = [
        {},
        { cat: 'tools' },
        { cat: { $ne: 'tools' } },
        { cat: { $in: ['tools', 'seeds'] } },
        { cat: { $nin: ['tools'] } },
        { qty: 0 },
        { qty: { $gt: 10 } },
        { qty: { $gte: 10, $lt: 14 } },
        { qty: { $lte: 2 } },
        { price: { $gt: 20, $lt: 30 } },
        { rank: null },
        { rank: { $exists: false } },
        { rank: { $ne: null } },
        { rank: { $gt: 60 } },
        { tags: 'odd' },
        { tags: 't3' },
        { tags: { $in: ['t1', 't2'] } },
        { cat: 'paint', qty: { $lt: 5 } },
        { $and: [{ cat: 'tools' }, { price: { $gte: 25 } }] },
        { $or: [{ qty: 16 }, { cat: 'seeds' }] },
        { $not: { cat: 'paint' } },
        { 'nested.city': 'Cebu' },
        { 'nested.score': { $gt: 30 } },
        { sku: { $regex: '^S0[12]' } },
      ];

      const disagreements = [];
      const vacuous = [];
      for (const f of filters) {
        const a = (await bare.find(f)).map((d) => d.sku).sort();
        const b = (await indexed.find(f)).map((d) => d.sku).sort();
        // A filter that matches nothing agrees trivially and proves nothing, so
        // it is a defect in this battery rather than a pass.
        if (a.length === 0) vacuous.push(JSON.stringify(f));
        if (JSON.stringify(a) !== JSON.stringify(b)) {
          disagreements.push({ filter: JSON.stringify(f), bare: a.length, indexed: b.length });
        }
        const ca = await bare.count(f);
        const cb = await indexed.count(f);
        if (ca !== cb || ca !== a.length) {
          disagreements.push({ filter: JSON.stringify(f), countBare: ca, countIndexed: cb, find: a.length });
        }
      }

      // Sorted paging must agree too — that is the path the index actually serves.
      const paged = async (c) =>
        (await c.aggregate([{ $sort: { qty: 1, sku: 1 } }, { $skip: 40 }, { $limit: 10 }])).map(
          (d) => d.sku,
        );
      const pagedBare = await paged(bare);
      const pagedIndexed = await paged(indexed);

      return {
        disagreements,
        checked: filters.length,
        vacuous,
        pagedAgree: JSON.stringify(pagedBare) === JSON.stringify(pagedIndexed),
        pagedSample: pagedIndexed.slice(0, 3),
      };
    });
    r.eq(out.disagreements, [], 'every filter agrees between the indexed and bare collections');
    r.eq(out.vacuous, [], 'no filter in the battery was vacuously empty');
    r.eq(out.pagedAgree, true, 'indexed and unindexed paging return the same page');
    r.note(`${out.checked} filters compared`);
  });

  await r.test('index lifecycle: create, use, drop, recreate', async (r) => {
    const out = await page.evaluate(
      async (attemptSrc) => {
        const attempt = eval(attemptSrc);
        const c = window.db.collection('idx_life');
        // Only a quarter of the rows carry the search token: a fixture where
        // every row matches cannot tell a working filter from an ignored one.
        await c.insertMany(
          Array.from({ length: 40 }, (_, i) => ({
            k: i,
            grp: i % 4,
            txt: i % 4 === 0 ? `row ${i} mentions tomato` : `row ${i} mentions cabbage`,
          })),
        );

        await c.createIndex('k');
        await c.createCompoundIndex(['grp', 'k']);
        await c.createFtsIndex('txt');
        const created = await c.listIndexes();
        const answersWithIndex = {
          point: (await c.find({ k: 7 })).length,
          compound: (await c.find({ grp: 1, k: { $lt: 20 } })).length,
          fts: (await c.find({ txt: { $contains: 'tomato' } })).length,
        };

        await c.dropIndex('k');
        await c.dropCompoundIndex(['grp', 'k']);
        await c.dropFtsIndex('txt');
        const dropped = await c.listIndexes();

        // Scalar queries must keep working after the index is gone — they fall
        // back to a scan. The FTS-only operator is the one that cannot.
        const answersWithout = {
          point: (await c.find({ k: 7 })).length,
          compound: (await c.find({ grp: 1, k: { $lt: 20 } })).length,
          fts: await attempt(() => c.find({ txt: { $contains: 'tomato' } })),
        };

        await c.createIndex('k');
        await c.createFtsIndex('txt');
        const recreated = {
          indexes: await c.listIndexes(),
          point: (await c.find({ k: 7 })).length,
          fts: (await c.find({ txt: { $contains: 'tomato' } })).length,
        };

        // Dropping an index that does not exist should not corrupt anything.
        const dropMissing = await attempt(() => c.dropIndex('nosuchfield'));
        return { created, answersWithIndex, dropped, answersWithout, recreated, dropMissing, rows: await c.count() };
      },
      ATTEMPT,
    );
    r.ok(out.created.btree?.includes('k'), 'the scalar index is listed');
    r.ok(out.created.fts?.includes('txt'), 'the text index is listed');
    r.eq(out.answersWithIndex.point, 1, 'point query with the index');
    r.eq(out.answersWithIndex.compound, 5, 'compound query with the index');
    r.eq(out.answersWithIndex.fts, 10, 'text query with the index matches only the tagged rows');
    r.ok(!out.dropped.btree?.includes('k'), 'the scalar index is delisted after the drop');
    r.eq(out.dropped.fts, [], 'the text index is delisted after the drop');
    r.eq(out.answersWithout.point, 1, 'a dropped index falls back to a scan, same answer');
    r.eq(out.answersWithout.compound, 5, 'a dropped compound index falls back to a scan');
    r.eq(out.recreated.point, 1, 'the recreated index answers correctly');
    r.eq(out.recreated.fts, 10, 'the recreated text index is backfilled from the documents');
    r.eq(out.rows, 40, 'no document was lost across the whole lifecycle');
    // Without the index `$contains` may refuse or fall back to a scan. What it
    // must never do is quietly drop the condition and return the collection.
    r.ok(
      out.answersWithout.fts.err || out.answersWithout.fts.ok?.length === 10,
      '$contains without an index either errors or still filters correctly',
    );
    r.note(
      `without fts index: ${out.answersWithout.fts.err ? 'errors' : `scans, returns ${out.answersWithout.fts.ok?.length}`}`,
    );
  });

  // --- schema, migrations, maintenance ---------------------------------------

  await r.test('a schema validates on insert, and optionally on read', async (r) => {
    const out = await page.evaluate(
      async (attemptSrc) => {
        const attempt = eval(attemptSrc);
        // Any object with `parse` — the shape Zod and Valibot both satisfy.
        const schema = {
          parse: (d) => {
            if (typeof d.name !== 'string') throw new Error('name must be a string');
            if (typeof d.age !== 'number') throw new Error('age must be a number');
            return d;
          },
        };
        const validated = window.db.collection('schema_users', { schema });
        const good = await attempt(() => validated.insert({ name: 'Ada', age: 36 }));
        const bad = await attempt(() => validated.insert({ name: 'Ada', age: 'thirty' }));
        const badMany = await attempt(() =>
          validated.insertMany([{ name: 'ok', age: 1 }, { name: 2, age: 2 }]),
        );
        const stored = await validated.count();

        // Drift: write around the schema through a plain handle, then read back
        // through one that validates on read.
        const raw = window.db.collection('schema_users');
        await raw.insert({ name: 'Drifted', age: 'not a number' });
        const strict = window.db.collection('schema_users', { schema, validateOnRead: true });
        const readAll = await attempt(() => strict.find({}));
        const readClean = await attempt(() => strict.find({ name: 'Ada' }));
        const lenientRead = await attempt(() => raw.find({}));
        return { good, bad, badMany, stored, readAll, readClean, lenientRead };
      },
      ATTEMPT,
    );
    r.ok(out.good.ok, 'a valid document inserts');
    r.ok(out.bad.err, 'an invalid document is rejected');
    r.ok(/age/.test(out.bad.err ?? ''), 'the rejection names the offending field');
    r.ok(out.badMany.err, 'insertMany rejects when any document fails');
    r.eq(out.stored, 1, 'a rejected insertMany stored nothing');
    r.ok(out.readAll.err, 'validateOnRead surfaces drifted data');
    r.ok(out.readClean.ok?.length === 1, 'validateOnRead still returns conforming documents');
    r.eq(out.lenientRead.ok?.length, 2, 'the same rows read fine without validateOnRead');
  });

  await r.test('openDB migrations run once each, in order, and resume', async (r) => {
    const first = await page.evaluate(async () => {
      window.__ran = [];
      const mk = (version) => ({
        version,
        up: async (db) => {
          window.__ran.push(version);
          await db.collection('mig_log').insert({ v: version });
        },
      });
      const db = await window.taladb.openDB('migrations.db', { migrations: [mk(1), mk(2)] });
      const rows = (await db.collection('mig_log').find({})).map((d) => d.v).sort();
      await db.close();
      return { ran: window.__ran, rows };
    });

    const second = await page.evaluate(async () => {
      window.__ran = [];
      const mk = (version) => ({
        version,
        up: async (db) => {
          window.__ran.push(version);
          await db.collection('mig_log').insert({ v: version });
        },
      });
      // Reopening with the same two plus a new one: only the new one may run.
      const db = await window.taladb.openDB('migrations.db', {
        migrations: [mk(1), mk(2), mk(3)],
      });
      const rows = (await db.collection('mig_log').find({})).map((d) => d.v).sort();
      await db.close();
      return { ran: window.__ran, rows };
    });

    r.eq(first.ran, [1, 2], 'both migrations ran on a fresh database, in ascending order');
    r.eq(first.rows, [1, 2], 'their writes landed');
    r.eq(second.ran, [3], 'a reopen runs only the migrations newer than the stored version');
    r.eq(second.rows, [1, 2, 3], 'the earlier writes were not repeated');
  });

  await r.test('compact() reclaims space and keeps every document', async (r) => {
    const out = await page.evaluate(async () => {
      const c = window.db.collection('compactable');
      await c.insertMany(Array.from({ length: 400 }, (_, i) => ({ i, pad: 'x'.repeat(200) })));
      await c.createIndex('i');
      await c.deleteMany({ i: { $lt: 300 } });
      const before = { count: await c.count(), sample: (await c.find({ i: 350 }))[0]?.i };
      await window.db.compact();
      return {
        before,
        after: {
          count: await c.count(),
          sample: (await c.find({ i: 350 }))[0]?.i,
          indexes: await c.listIndexes(),
          range: (await c.find({ i: { $gte: 390 } })).length,
        },
      };
    });
    r.eq(out.after.count, out.before.count, 'compaction preserves the document count');
    r.eq(out.after.sample, 350, 'an indexed point read still works after compaction');
    r.eq(out.after.range, 10, 'an indexed range read still works after compaction');
    r.ok(out.after.indexes.btree?.includes('i'), 'the index survived compaction');
  });

  await r.test('the catalogue survives a reload with its indexes', async (r) => {
    const before = await page.evaluate(async () => {
      const out = {
        count: await window.cat.count(),
        byCat: (await window.cat.find({ cat: 'tools' })).map((d) => d.sku).sort(),
        fts: (await window.cat.find({ body: { $contains: 'paint' } })).map((d) => d.sku).sort(),
        indexes: await window.cat.listIndexes(),
      };
      await window.db.close();
      return out;
    });
    await page.reload({ waitUntil: 'load' });
    await page.waitForFunction('window.__ready === true');
    const after = await page.evaluate(async () => {
      const db = await window.taladb.openDB('documents.db');
      window.db = db;
      window.cat = db.collection('catalogue');
      return {
        count: await window.cat.count(),
        byCat: (await window.cat.find({ cat: 'tools' })).map((d) => d.sku).sort(),
        fts: (await window.cat.find({ body: { $contains: 'paint' } })).map((d) => d.sku).sort(),
        indexes: await window.cat.listIndexes(),
        // The reloaded collection must still accept writes into its indexes.
        appended: await (async () => {
          await window.cat.insert({ sku: 'E1', cat: 'tools', qty: 2, price: 5, tags: [], body: 'a fresh paint brush' });
          return (await window.cat.find({ body: { $contains: 'paint' } })).map((d) => d.sku).sort();
        })(),
      };
    });
    r.eq(after.count, before.count, 'every document survived the reload');
    r.eq(after.byCat, before.byCat, 'scalar queries answer identically after the reload');
    r.eq(after.fts, before.fts, 'text queries answer identically after the reload');
    r.eq(after.indexes.fts, before.indexes.fts, 'the text index survived');
    r.eq(after.appended, [...before.fts, 'E1'].sort(), 'the reloaded text index accepts new rows');
  });
}
