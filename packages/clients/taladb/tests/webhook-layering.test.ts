// The webhook wrapper and the schema wrapper are separately unit-tested; these
// cover the seam *between* them, which is where they previously disagreed.
//
// `decorateCollection` is the single place that orders the two, so every case
// below goes through it rather than composing the wrappers by hand — a test
// that assembled its own stack could pass while every adapter shipped the
// opposite order.

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { decorateCollection } from '../src/index';
import { createWebhookDispatcher } from '../src/webhook';
import type { Collection, Document } from '../src/types';

function recordingFetch() {
  const calls: { method: string; body: Record<string, unknown> }[] = [];
  const fn = vi.fn(async (_url: string, init: RequestInit) => {
    calls.push({ method: init.method!, body: JSON.parse(init.body as string) });
    return { ok: true, status: 200, statusText: 'ok' } as Response;
  });
  return { fn: fn as unknown as typeof fetch, calls };
}

/**
 * Enough of the engine's filter language to run the `_v` guard `applySchema`
 * builds: `$and` / `$or` / `$exists` / `$lte` / `$in`. Without these the guarded
 * filter would match nothing and the divergence under test would be invisible.
 */
function matches(doc: Document, filter: unknown): boolean {
  if (filter === undefined || filter === null) return true;
  return Object.entries(filter as Record<string, unknown>).every(([k, v]) => {
    if (k === '$and') return (v as unknown[]).every((f) => matches(doc, f));
    if (k === '$or') return (v as unknown[]).some((f) => matches(doc, f));
    if (v !== null && typeof v === 'object') {
      const ops = v as Record<string, unknown>;
      if ('$exists' in ops) return (doc[k] !== undefined) === ops.$exists;
      if ('$lte' in ops) return (doc[k] as number) <= (ops.$lte as number);
      if ('$in' in ops) return (ops.$in as unknown[]).includes(doc[k]);
    }
    return doc[k] === v;
  });
}

/** A collection double that behaves like the engine below the wrappers. */
function fakeCollection(seed: Document[] = []) {
  let docs = seed.map((d) => ({ ...d }));
  let n = 0;
  const reads = { find: 0, findOne: 0, aggregate: 0 };
  const col = {
    async insert(doc: Omit<Document, '_id'>) {
      const _id = `id${++n}`;
      // The engine stamps `_changed_at`; the caller never supplies it.
      docs.push({ ...doc, _id, _changed_at: 1_700_000_000_000 } as Document);
      return _id;
    },
    async insertMany(input: Omit<Document, '_id'>[]) {
      const out: string[] = [];
      for (const d of input) out.push(await col.insert(d));
      return out;
    },
    async find(f?: unknown) {
      reads.find++;
      return docs.filter((d) => matches(d, f));
    },
    async findOne(f: unknown) {
      reads.findOne++;
      return docs.find((d) => matches(d, f)) ?? null;
    },
    async updateOne(f: unknown, u: unknown) {
      const i = docs.findIndex((d) => matches(d, f));
      if (i === -1) return false;
      docs[i] = { ...docs[i], ...((u as { $set: object }).$set ?? {}) };
      return true;
    },
    async updateMany(f: unknown, u: unknown) {
      let c = 0;
      docs = docs.map((d) => {
        if (!matches(d, f)) return d;
        c++;
        return { ...d, ...((u as { $set: object }).$set ?? {}) };
      });
      return c;
    },
    async deleteOne(f: unknown) {
      const i = docs.findIndex((d) => matches(d, f));
      if (i === -1) return false;
      docs.splice(i, 1);
      return true;
    },
    async deleteMany(f: unknown) {
      const before = docs.length;
      docs = docs.filter((d) => !matches(d, f));
      return before - docs.length;
    },
    async aggregate(pipeline: unknown) {
      reads.aggregate++;
      const [{ $match: f }] = pipeline as [{ $match: unknown }];
      return docs.filter((d) => matches(d, f)).map((d) => ({ _id: d._id }) as Document);
    },
    async count(f?: unknown) {
      return docs.filter((d) => matches(d, f)).length;
    },
    // `applySchema` wraps or binds these; they are never driven in these tests.
    subscribe: () => () => {},
    subscribeAggregate: () => () => {},
  } as unknown as Collection<Document>;
  return { col, reads, all: () => docs };
}

const cfg = { enabled: true, endpoint: 'https://api.test/hook' };
const v = (version: number) => ({ syncSchema: { version, fields: {} } }) as never;

beforeEach(() => {
  vi.spyOn(console, 'warn').mockImplementation(() => {});
});
afterEach(() => {
  vi.restoreAllMocks();
});

describe('webhook + schema layering', () => {
  it('reports the stored document on insert, not the caller\'s input', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher({ ...cfg, fetch: fn })!;
    const { col: raw, all } = fakeCollection();
    const col = decorateCollection(raw, 'notes', v(2), d);

    const id = await col.insert({ title: 'a' } as never);
    await d.flush();

    // `_v` comes from applySchema, `_changed_at` from the engine. Reporting the
    // submitted document would have carried neither, so a receiver persisting
    // the POST body held a different document than the one on disk.
    expect(all().find((doc) => doc._id === id)).toMatchObject({ _v: 2, _changed_at: expect.any(Number) });
    expect(calls[0].method).toBe('POST');
    expect(calls[0].body.document).toMatchObject({ _id: id, title: 'a', _v: 2, _changed_at: expect.any(Number) });
  });

  it('resolves ids through the same _v guard the mutation applies', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher({ ...cfg, fetch: fn })!;
    // id2 was written by a newer client; this build must not touch or report it.
    const { col: raw, all } = fakeCollection([
      { _id: 'id1', tag: 'x', _v: 1 },
      { _id: 'id2', tag: 'x', _v: 9 },
    ]);
    const col = decorateCollection(raw, 'notes', v(1), d);

    expect(await col.deleteMany({ tag: 'x' } as never)).toBe(1);
    await d.flush();

    expect(all().map((doc) => doc._id)).toEqual(['id2']);
    expect(calls).toHaveLength(1);
    expect(calls[0].body.id).toBe('id1');
  });

  it('reports the document updateOne actually picked under the guard', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher({ ...cfg, fetch: fn })!;
    // The unguarded filter's first match is id1, the guarded filter's is id2.
    const { col: raw } = fakeCollection([
      { _id: 'id1', tag: 'x', _v: 9 },
      { _id: 'id2', tag: 'x', _v: 1 },
    ]);
    const col = decorateCollection(raw, 'notes', v(1), d);

    expect(await col.updateOne({ tag: 'x' } as never, { $set: { done: true } } as never)).toBe(true);
    await d.flush();

    expect(calls).toHaveLength(1);
    expect(calls[0].body.id).toBe('id2');
    expect(calls[0].body.document).toMatchObject({ _id: 'id2', done: true });
  });

  it('does not let validateOnRead turn a delete into a throw', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher({ ...cfg, fetch: fn })!;
    const opts = {
      schema: { parse: (doc: Document) => { if (doc.bad) throw new Error('nope'); return doc; } },
      validateOnRead: true,
    } as never;
    const { col: raw, all } = fakeCollection([{ _id: 'id1', tag: 'x', bad: true }]);
    const col = decorateCollection(raw, 'notes', opts, d);

    // Id resolution reads below the schema layer, so an unreadable document is
    // still a deletable one — enabling a notification channel must not be able
    // to fail the write it is reporting on.
    expect(await col.deleteMany({ tag: 'x' } as never)).toBe(1);
    await d.flush();
    expect(all()).toEqual([]);
    expect(calls.map((c) => c.method)).toEqual(['DELETE']);
  });

  it('reads post-images in one batched query, not one per document', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher({ ...cfg, fetch: fn })!;
    const seed = Array.from({ length: 25 }, (_, i) => ({ _id: `id${i}`, tag: 'x' }));
    const { col: raw, reads } = fakeCollection(seed);
    const col = decorateCollection(raw, 'notes', undefined, d);

    expect(await col.updateMany({ tag: 'x' } as never, { $set: { done: true } } as never)).toBe(25);
    await d.flush();

    expect(calls).toHaveLength(25);
    // One projection to resolve ids, one batched read for the post-images.
    expect(reads.aggregate).toBe(1);
    expect(reads.find).toBe(1);
    expect(reads.findOne).toBe(0);
  });

  it('emits per-document events in mutation order', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher({ ...cfg, fetch: fn })!;
    const { col: raw } = fakeCollection();
    const col = decorateCollection(raw, 'notes', undefined, d);

    const id = await col.insert({ title: 'a' } as never);
    await col.updateOne({ _id: id } as never, { $set: { title: 'b' } } as never);
    await col.deleteOne({ _id: id } as never);
    await d.flush();

    expect(calls.map((c) => c.method)).toEqual(['POST', 'PUT', 'DELETE']);
  });

  it('leaves the collection undecorated when no webhook is configured', () => {
    const { col: raw } = fakeCollection();
    expect(decorateCollection(raw, 'notes', undefined, null)).toBe(raw);
  });
});

describe('flush', () => {
  it('clears its timeout so a drained queue does not hold the event loop open', async () => {
    vi.useFakeTimers();
    try {
      const fetchFn = (async () => ({ ok: true, status: 200, statusText: 'ok' })) as unknown as typeof fetch;
      const d = createWebhookDispatcher({ ...cfg, fetch: fetchFn })!;
      d.emit({ op: 'delete', collection: 'c', id: '1' });
      const drained = d.flush(30_000);
      await vi.advanceTimersByTimeAsync(0);
      expect(await drained).toBe(true);
      // Left armed, this timer keeps a Node process alive for the full timeout
      // after `await db.close()` has already returned.
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it('still resolves false when the queue cannot drain in time', async () => {
    const stalled = (() => new Promise<Response>(() => {})) as unknown as typeof fetch;
    const d = createWebhookDispatcher({ ...cfg, fetch: stalled })!;
    d.emit({ op: 'delete', collection: 'c', id: '1' });
    expect(await d.flush(20)).toBe(false);
  });
});

describe('webhook timestamps', () => {
  it('stamps commit time, not delivery time', async () => {
    let now = 1_000_000;
    vi.spyOn(Date, 'now').mockImplementation(() => now);
    const bodies: Record<string, unknown>[] = [];
    // A slow endpoint: delivery happens well after the events were queued.
    const fn = (async (_u: string, init: RequestInit) => {
      bodies.push(JSON.parse(init.body as string));
      return { ok: true, status: 200, statusText: 'ok' } as Response;
    }) as unknown as typeof fetch;
    const d = createWebhookDispatcher({ ...cfg, fetch: fn })!;

    d.emit({ op: 'delete', collection: 'c', id: 'a' });
    now = 1_000_500;
    d.emit({ op: 'delete', collection: 'c', id: 'b' });
    now = 9_999_999; // time passes while the queue drains
    await d.flush();

    expect(bodies.map((b) => b.timestamp)).toEqual([1_000_000, 1_000_500]);
  });
});
