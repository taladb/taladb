// Phase 3 — multi-tab. Tab A takes the OPFS lock; later tabs fall back to an
// IndexedDB snapshot and forward their writes to A.
import { newTab, sleep } from './lib.mjs';

const open = (page, dbName, opts = {}) =>
  page.evaluate(
    async (n, o) => {
      window.db = await window.taladb.openDB(n, o);
      window.c = window.db.collection('notes');
      window.mode = window.__logs;
      return true;
    },
    dbName,
    opts,
  );

const count = (page, filter = null) =>
  page.evaluate((f) => window.c.count(f ?? undefined), filter);

/** Poll a page-side expression until it matches, or time out. */
async function until(page, fn, arg, predicate, timeout = 4000) {
  const deadline = Date.now() + timeout;
  let last;
  for (;;) {
    last = await page.evaluate(fn, arg);
    if (predicate(last)) return last;
    if (Date.now() > deadline) return last;
    await sleep(100);
  }
}

export async function run(browser, r) {
  const A = await newTab(browser, { label: 'A' });
  const B = await newTab(browser, { label: 'B' });

  await r.test('tab A takes OPFS, tab B falls back to the IDB snapshot', async (r) => {
    await open(A, 'mt.db');
    await A.evaluate(async () => {
      await window.c.insertMany([
        { t: 'from-A-1' },
        { t: 'from-A-2' },
      ]);
      await window.db.flush?.();
    });
    await open(B, 'mt.db');
    const modes = {
      a: await A.evaluate(() => window.__logs.map((l) => l[1]).join(' | ')),
      b: await B.evaluate(() => window.__logs.map((l) => l[1]).join(' | ')),
    };
    const bCount = await until(B, () => window.c.count(), null, (n) => n >= 2);
    r.eq(await count(A), 2, 'A has both documents');
    r.eq(bCount, 2, 'B sees the documents A wrote before it opened');
    r.note(`B warnings: ${modes.b.slice(0, 100) || '(none)'}`);
  });

  await r.test('isPrimary: A owns the file, B does not', async (r) => {
    r.eq(await A.evaluate(() => window.db.isPrimary()), true, 'A holds the OPFS lock');
    r.eq(await B.evaluate(() => window.db.isPrimary()), false, 'B forwards its writes');
  });

  await r.test("A's later writes become visible in B", async (r) => {
    await A.evaluate(() => window.c.insert({ t: 'from-A-3' }));
    const n = await until(B, () => window.c.count(), null, (x) => x >= 3);
    r.eq(n, 3, 'B picks up a write made by A after both were open');
  });

  await r.test("B's writes reach A (cross-tab write forwarding)", async (r) => {
    const localCount = await B.evaluate(async () => {
      await window.c.insert({ t: 'from-B-1' });
      return window.c.count();
    });
    r.eq(localCount, 4, 'B reads its own write back immediately');
    const aCount = await until(A, () => window.c.count(), null, (x) => x >= 4);
    r.eq(aCount, 4, "A (the durable tab) received B's write");
    const stillInB = await until(
      B,
      () => window.c.count({ t: 'from-B-1' }),
      null,
      (x) => x === 1,
    );
    r.eq(stillInB, 1, "B's write survives its next snapshot reload");
  });

  await r.test("B's update/delete are re-evaluated by A", async (r) => {
    const res = await B.evaluate(async () => {
      const updated = await window.c.updateMany({ t: 'from-A-1' }, { $set: { seen: true } });
      const deleted = await window.c.deleteOne({ t: 'from-A-2' });
      return { updated, deleted };
    });
    r.eq(res.updated, 1, 'B reports the update it applied locally');
    r.eq(res.deleted, true, 'B reports the delete it applied locally');
    const a = await until(
      A,
      async () => ({
        seen: await window.c.count({ seen: true }),
        gone: await window.c.count({ t: 'from-A-2' }),
      }),
      null,
      (x) => x.seen === 1 && x.gone === 0,
    );
    r.eq(a.seen, 1, "A applied B's update");
    r.eq(a.gone, 0, "A applied B's delete");
  });

  await r.test('live query in the fallback tab sees the primary tab writes', async (r) => {
    await B.evaluate(() => {
      window.snaps = [];
      window.unsub = window.c.subscribe({ live: true }, (docs) => window.snaps.push(docs.length));
    });
    await sleep(600);
    await A.evaluate(() => window.c.insert({ live: true, n: 1 }));
    await sleep(1500);
    await A.evaluate(() => window.c.insert({ live: true, n: 2 }));
    await sleep(1500);
    const snaps = await B.evaluate(() => {
      window.unsub?.();
      return window.snaps;
    });
    r.eq(snaps, [0, 1, 2], 'the fallback tab live query saw both inserts');
    r.note(`snapshots=${JSON.stringify(snaps)}`);
  });

  await r.test('live query in the primary tab sees the fallback tab writes', async (r) => {
    await A.evaluate(() => {
      window.snaps = [];
      window.unsub = window.c.subscribe({ live2: true }, (docs) => window.snaps.push(docs.length));
    });
    await sleep(600);
    await B.evaluate(() => window.c.insert({ live2: true, n: 1 }));
    await sleep(1500);
    const snaps = await A.evaluate(() => {
      window.unsub?.();
      return window.snaps;
    });
    r.eq(snaps, [0, 1], "the primary tab live query saw the fallback tab's insert");
  });

  await r.test('a third tab also converges', async (r) => {
    const C = await newTab(browser, { label: 'C' });
    await open(C, 'mt.db');
    const aCount = await count(A);
    const cCount = await until(C, () => window.c.count(), null, (x) => x === aCount);
    r.eq(cCount, aCount, 'tab C converges on the same document count as A');
    await C.evaluate(() => window.c.insert({ t: 'from-C-1' }));
    const seenByA = await until(A, () => window.c.count({ t: 'from-C-1' }), null, (x) => x === 1);
    r.eq(seenByA, 1, "A received C's write");
    const seenByB = await until(B, () => window.c.count({ t: 'from-C-1' }), null, (x) => x === 1);
    r.eq(seenByB, 1, "B also converges on C's write");
    await C.close();
  });

  await r.test('when the primary tab closes, a fallback tab takes over persistence', async (r) => {
    const before = await count(A);
    await A.evaluate(() => window.db.close());
    // Give B a chance to notice the lock is free.
    await sleep(1500);
    await B.evaluate(() => window.c.insert({ t: 'after-A-closed' }));
    await sleep(1500);
    // A fresh tab reads whatever is durable now.
    const D = await newTab(browser, { label: 'D' });
    await open(D, 'mt.db');
    const durable = await until(
      D,
      () => window.c.count({ t: 'after-A-closed' }),
      null,
      (x) => x === 1,
    );
    const total = await count(D);
    r.eq(durable, 1, 'a write made after the primary closed is durable');
    r.note(`before=${before} totalAfter=${total}`);
    // Primary status is not fixed for a tab's lifetime — B was a secondary and
    // is now the owner. Anything gated on isPrimary must re-check, not cache.
    r.eq(await B.evaluate(() => window.db.isPrimary()), true, 'B reports primary after promotion');
    await D.close();
  });

  await r.test('writes racing the primary going away are not lost', async (r) => {
    const P = await newTab(browser, { label: 'rA' });
    const Q = await newTab(browser, { label: 'rB' });
    await open(P, 'race.db');
    await P.evaluate(() => window.c.insert({ t: 'seed' }));
    await open(Q, 'race.db');
    await until(Q, () => window.c.count(), null, (x) => x === 1);
    // Close the owner and write from the survivor in the same instant — the
    // window where no tab owns the file.
    await Promise.all([
      P.evaluate(() => window.db.close()),
      Q.evaluate(async () => {
        for (let i = 0; i < 5; i++) await window.c.insert({ t: `race-${i}` });
      }),
    ]);
    await sleep(2000);
    await Q.evaluate(() => window.db.flush?.());
    const R = await newTab(browser, { label: 'rC' });
    await open(R, 'race.db');
    const durable = await until(
      R,
      () => window.c.count({ t: { $ne: 'seed' } }),
      null,
      (x) => x === 5,
    );
    r.eq(durable, 5, 'every write made during the handover is durable');
    await P.close();
    await Q.close();
    await R.close();
  });

  await r.test('two tabs writing concurrently converge', async (r) => {
    const P = await newTab(browser, { label: 'cA' });
    const Q = await newTab(browser, { label: 'cB' });
    await open(P, 'conc.db');
    await open(Q, 'conc.db');
    await sleep(300);
    await Promise.all([
      P.evaluate(async () => {
        for (let i = 0; i < 20; i++) await window.c.insert({ from: 'P', i });
      }),
      Q.evaluate(async () => {
        for (let i = 0; i < 20; i++) await window.c.insert({ from: 'Q', i });
      }),
    ]);
    const pTotal = await until(P, () => window.c.count(), null, (x) => x === 40);
    const qTotal = await until(Q, () => window.c.count(), null, (x) => x === 40);
    r.eq(pTotal, 40, 'the owning tab holds all 40 documents');
    r.eq(qTotal, 40, 'the fallback tab converges on all 40 documents');
    const dupes = await P.evaluate(async () => {
      const docs = await window.c.find();
      const keys = docs.map((d) => `${d.from}-${d.i}`);
      return keys.length - new Set(keys).size;
    });
    r.eq(dupes, 0, 'no document was applied twice');
    await P.close();
    await Q.close();
  });

  await r.test('data survives every tab closing and a fresh open', async (r) => {
    const expected = await count(B);
    await B.evaluate(() => window.db.close().catch(() => {}));
    await B.close();
    await A.close();
    await sleep(500);
    const E = await newTab(browser, { label: 'E' });
    await open(E, 'mt.db');
    const got = await until(E, () => window.c.count(), null, (x) => x === expected);
    r.eq(got, expected, 'all documents survived every tab closing');
    await E.close();
  });

  await r.test('encrypted database refuses a second tab (no plaintext fallback)', async (r) => {
    const P = await newTab(browser, { label: 'P' });
    const Q = await newTab(browser, { label: 'Q' });
    const first = await P.evaluate(async () => {
      try {
        window.db = await window.taladb.openDB('enc.db', { passphrase: 'correct horse battery' });
        await window.db.collection('secret').insert({ s: 1 });
        return 'ok';
      } catch (e) {
        return String(e.message ?? e).slice(0, 120);
      }
    });
    const second = await Q.evaluate(async () => {
      try {
        const db = await window.taladb.openDB('enc.db', { passphrase: 'correct horse battery' });
        await db.collection('secret').insert({ s: 2 });
        return 'ok';
      } catch (e) {
        return String(e.message ?? e).slice(0, 120);
      }
    });
    r.eq(first, 'ok', 'first tab opens the encrypted database');
    r.ok(second !== 'ok', 'second tab is refused rather than downgraded');
    r.note(`second: ${second}`);
    const wrongPass = await P.evaluate(async () => {
      await window.db.close();
      try {
        await window.taladb.openDB('enc.db', { passphrase: 'wrong' });
        return 'OPENED-WITH-WRONG-PASSPHRASE';
      } catch (e) {
        return String(e.message ?? e).slice(0, 80);
      }
    });
    r.ok(wrongPass !== 'OPENED-WITH-WRONG-PASSPHRASE', 'a wrong passphrase is rejected');
    r.note(`wrong passphrase: ${wrongPass}`);
    await P.close();
    await Q.close();
  });
}
