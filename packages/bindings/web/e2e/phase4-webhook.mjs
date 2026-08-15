// Phase 4 — the change webhook: one HTTP request per committed mutation.
import { newTab, hook, sleep, ORIGIN } from './lib.mjs';

// Read at call time: the server's port is assigned by the OS, and this module
// is imported after it has settled — but a top-level const would still freeze
// whatever `ORIGIN` held at import.
const hookUrl = () => `${ORIGIN}/hook`;

async function openWithHook(page, dbName, webhook = {}) {
  return page.evaluate(
    async (n, url, extra) => {
      window.db = await window.taladb.openDB(n, {
        webhook: { enabled: true, endpoint: url, ...extra },
      });
      window.c = window.db.collection('items');
      return true;
    },
    dbName,
    hookUrl(),
    webhook,
  );
}

export async function run(browser, r) {
  const page = await newTab(browser, { label: 'W' });

  await r.test('insert → POST, update → PUT, delete → DELETE', async (r) => {
    await hook.reset();
    await openWithHook(page, 'wh1.db');
    const id = await page.evaluate(async () => {
      const id = await window.c.insert({ name: 'first', n: 1 });
      await window.c.updateOne({ _id: id }, { $set: { n: 2 } });
      await window.c.deleteOne({ _id: id });
      await window.db.flushWebhook();
      return id;
    });
    const log = await hook.waitFor(3);
    r.eq(log.map((e) => e.method), ['POST', 'PUT', 'DELETE'], 'one request per mutation, right verb');
    r.eq(log[0].body.collection, 'items', 'collection reported');
    r.eq(log[0].body.id, id, 'id reported');
    r.eq(log[0].body.document?.name, 'first', 'insert carries the document');
    r.ok(typeof log[0].body.document?._changed_at === 'number', 'insert body carries engine fields');
    r.eq(log[1].body.document?.n, 2, 'update carries the post-image');
    // A delete has no post-image to read, so the payload carries the last
    // stored pre-image instead — the key is present for every verb.
    r.eq(log[2].body.document?.n, 2, 'delete carries the pre-image');
    r.eq(log[2].body.document?._id, id, 'the pre-image is the deleted document');
    r.ok(
      log.every((e) => typeof e.body.timestamp === 'number'),
      'every event carries a timestamp',
    );
    r.ok(
      log.every((e) => typeof e.body.event_id === 'string' && e.body.event_id),
      'every event carries an event_id',
    );
    r.eq(
      new Set(log.map((e) => e.body.event_id)).size,
      3,
      'each mutation gets its own event_id',
    );
  });

  await r.test('insertMany / updateMany / deleteMany fire one event per document', async (r) => {
    await hook.reset();
    const before = await page.evaluate(() => window.db.webhookStats());
    const stats = await page.evaluate(async () => {
      const bulk = window.db.collection('bulk');
      await bulk.insertMany(Array.from({ length: 5 }, (_, i) => ({ i })));
      await bulk.updateMany({ i: { $lt: 3 } }, { $set: { touched: true } });
      await bulk.deleteMany({ i: { $gte: 3 } });
      await window.db.flushWebhook();
      return window.db.webhookStats();
    });
    const log = await hook.waitFor(10);
    // Counted per verb, not compared as an object: different documents deliver
    // concurrently, so the key order of a tallied object is delivery order, and
    // the runner compares by JSON — which would make this assertion a coin flip.
    const count = (m) => log.filter((e) => e.method === m).length;
    r.eq(
      [count('POST'), count('PUT'), count('DELETE')],
      [5, 3, 2],
      'one event per affected document',
    );
    r.eq(stats.pending, 0, 'flushWebhook drained the queue');
    r.eq(stats.failed, 0, 'no failures');
    r.eq(stats.delivered - before.delivered, 10, 'delivered counter matches');
    const ids = new Set(log.filter((e) => e.method === 'POST').map((e) => e.body.id));
    r.eq(ids.size, 5, 'insertMany reports 5 distinct ids');
  });

  await r.test('per-document ordering: awaited writes carry every intermediate state', async (r) => {
    await hook.reset();
    await page.evaluate(async () => {
      const ord = window.db.collection('ordering');
      const id = await ord.insert({ v: 0 });
      for (let i = 1; i <= 10; i++) await ord.updateOne({ _id: id }, { $set: { v: i } });
      await window.db.flushWebhook();
    });
    const log = await hook.waitFor(11);
    const seq = log.map((e) => e.body.document?.v);
    r.eq(seq, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 'every state is reported, in order');
  });

  await r.test('per-document ordering: overlapping writes still converge', async (r) => {
    await hook.reset();
    await page.evaluate(async () => {
      const ord = window.db.collection('ordering2');
      const id = await ord.insert({ v: 0 });
      // Deliberately not awaited: the post-image read for each event races the
      // writes that follow it, so bodies may repeat the newest value.
      for (let i = 1; i <= 10; i++) ord.updateOne({ _id: id }, { $set: { v: i } });
      await window.db.flushWebhook();
    });
    const log = await hook.waitFor(11);
    const seq = log.map((e) => e.body.document?.v);
    r.eq(log.length, 11, 'one event per mutation');
    r.eq(seq[seq.length - 1], 10, 'the last event carries the final state');
    r.ok(
      seq.every((v, i) => i === 0 || v >= seq[i - 1]),
      'values never go backwards (a receiver converges on the truth)',
    );
    r.note(`bodies=${JSON.stringify(seq)}`);
  });

  await r.test('exclude_fields strips heavy fields; collections allowlist filters', async (r) => {
    await hook.reset();
    await page.evaluate(async () => {
      await window.db.close();
      window.db = await window.taladb.openDB('wh2.db', {
        webhook: {
          enabled: true,
          endpoint: `${location.origin}/hook`,
          exclude_fields: ['embedding'],
          collections: ['watched'],
        },
      });
      await window.db.collection('watched').insert({ name: 'w', embedding: [1, 2, 3] });
      await window.db.collection('ignored').insert({ name: 'i' });
      await window.db.flushWebhook();
    });
    const log = await hook.waitFor(1);
    await sleep(300);
    const all = await hook.log();
    r.eq(all.length, 1, 'only the allow-listed collection reports');
    r.eq(all[0]?.body.collection, 'watched', 'right collection');
    r.eq(all[0]?.body.document?.embedding, undefined, 'excluded field stripped');
    r.eq(all[0]?.body.document?.name, 'w', 'other fields kept');
  });

  await r.test('retries on 5xx, gives up on 4xx', async (r) => {
    await hook.reset();
    await hook.mode({ failFirst: 2 });
    await page.evaluate(async () => {
      await window.db.collection('watched').insert({ name: 'retry-me' });
      await window.db.flushWebhook(8000);
    });
    const log = await hook.waitFor(3, 8000);
    r.eq(log.length, 3, 'two 500s then a success (3 attempts)');
    r.ok(
      log.every((e) => e.body.id === log[0].body.id),
      'the same event was retried',
    );
    const statsAfter5xx = await page.evaluate(() => window.db.webhookStats());
    r.eq(statsAfter5xx.failed, 0, '5xx retry eventually succeeded');

    await hook.reset();
    await hook.mode({ status: 400 });
    const stats = await page.evaluate(async () => {
      await window.db.collection('watched').insert({ name: 'reject-me' });
      await window.db.flushWebhook(8000);
      return window.db.webhookStats();
    });
    await sleep(500);
    const log4xx = await hook.log();
    r.eq(log4xx.length, 1, '4xx is not retried');
    r.eq(stats.failed, 1, 'failed counter incremented');
  });

  await r.test('timestamp reflects commit time, not delivery time', async (r) => {
    await hook.reset();
    await hook.mode({ delayMs: 300 });
    const commitTimes = await page.evaluate(async () => {
      const c = window.db.collection('watched');
      const t = [];
      for (let i = 0; i < 3; i++) {
        t.push(Date.now());
        await c.insert({ seq: i });
      }
      await window.db.flushWebhook(10000);
      return t;
    });
    const log = await hook.waitFor(3, 10000);
    const stamps = log.map((e) => e.body.timestamp);
    const drift = stamps.map((s, i) => s - commitTimes[i]);
    r.ok(
      drift.every((d) => d >= 0 && d < 1000),
      `timestamps track commit time (drift ${JSON.stringify(drift)}ms)`,
    );
    await hook.mode({});
  });

  await r.test('a slow endpoint does not stall the write path', async (r) => {
    await hook.reset();
    await hook.mode({ delayMs: 400 });
    const ms = await page.evaluate(async () => {
      const c = window.db.collection('watched');
      const t0 = performance.now();
      for (let i = 0; i < 5; i++) await c.insert({ slow: i });
      return performance.now() - t0;
    });
    r.ok(ms < 500, `5 writes took ${Math.round(ms)}ms while the endpoint takes 400ms each`);
    await page.evaluate(() => window.db.flushWebhook(15000));
    await hook.mode({});
  });

  await r.test('queue overflow drops rather than blocking', async (r) => {
    await hook.reset();
    await hook.mode({ delayMs: 200 });
    const stats = await page.evaluate(async () => {
      await window.db.close();
      window.db = await window.taladb.openDB('wh3.db', {
        webhook: { enabled: true, endpoint: `${location.origin}/hook`, max_queue: 10 },
      });
      const c = window.db.collection('flood');
      await c.insertMany(Array.from({ length: 100 }, (_, i) => ({ i })));
      return window.db.webhookStats();
    });
    r.ok(stats.dropped > 0, `queue overflow dropped ${stats.dropped} events instead of blocking`);
    r.ok(stats.pending <= 10, `pending capped at max_queue (${stats.pending})`);
    await hook.mode({});
    await page.evaluate(() => window.db.flushWebhook(15000));
  });

  await r.test('close() drains the queue', async (r) => {
    await hook.reset();
    await page.evaluate(async () => {
      await window.db.close();
      window.db = await window.taladb.openDB('wh4.db', {
        webhook: { enabled: true, endpoint: `${location.origin}/hook` },
      });
      await window.db.collection('last').insert({ final: true });
      await window.db.close();
    });
    const log = await hook.waitFor(1, 3000);
    r.eq(log.length, 1, 'the last write was delivered before close resolved');
    r.eq(log[0]?.body.document?.final, true, 'payload intact');
  });

  await r.test('multi-tab: a forwarded write fires exactly one webhook', async (r) => {
    await hook.reset();
    const A = await newTab(browser, { label: 'wA' });
    const B = await newTab(browser, { label: 'wB' });
    await openWithHook(A, 'wh-mt.db');
    await openWithHook(B, 'wh-mt.db');
    await sleep(300);
    await B.evaluate(async () => {
      await window.c.insert({ from: 'B' });
      await window.db.flushWebhook(5000);
    });
    await sleep(1200);
    const log = await hook.log();
    r.eq(log.length, 1, 'exactly one webhook for a write forwarded between tabs');
    r.eq(log[0]?.body.document?.from, 'B', 'payload comes from the originating tab');
    await A.evaluate(() => window.db.close().catch(() => {}));
    await B.evaluate(() => window.db.close().catch(() => {}));
    await A.close();
    await B.close();
  });

  await page.close();
}
