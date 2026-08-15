import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
  createWebhookDispatcher,
  validateWebhookConfig,
  wrapCollectionWithWebhook,
  type WebhookConfig,
} from '../src/webhook';
import type { Document } from '../src/types';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

interface Req {
  url: string;
  method: string;
  body: Record<string, unknown>;
  headers: Record<string, string>;
}

/** A `fetch` double that records every request and replies per `respond`. */
function recordingFetch(respond: (n: number) => { status: number } | Error = () => ({ status: 200 })) {
  const calls: Req[] = [];
  const fn = vi.fn(async (url: string, init: RequestInit) => {
    calls.push({
      url,
      method: init.method!,
      body: JSON.parse(init.body as string),
      headers: init.headers as Record<string, string>,
    });
    const r = respond(calls.length);
    if (r instanceof Error) throw r;
    return { ok: r.status >= 200 && r.status < 300, status: r.status, statusText: 'x' } as Response;
  });
  return { fn: fn as unknown as typeof fetch, calls };
}

function config(over: Partial<WebhookConfig> = {}): WebhookConfig {
  return { enabled: true, endpoint: 'https://api.test/hook', ...over };
}

/** Minimal in-memory collection double exposing only the webhook-wrapped slice. */
function fakeCollection(seed: Document[] = []) {
  let docs = [...seed];
  let n = 0;
  const spy = {
    find: vi.fn(),
    findOne: vi.fn(),
    updateOne: vi.fn(),
    deleteMany: vi.fn(),
  };
  const matches = (d: Document, f: unknown): boolean => {
    if (f === undefined || f === null) return true;
    return Object.entries(f as Record<string, unknown>).every(([k, v]) => d[k] === v);
  };
  const col = {
    async insert(doc: Omit<Document, '_id'>) {
      const _id = `id${++n}`;
      docs.push({ ...doc, _id } as Document);
      return _id;
    },
    async insertMany(input: Omit<Document, '_id'>[]) {
      return Promise.all(input.map((d) => col.insert(d)));
    },
    async find(f?: unknown) {
      spy.find(f);
      return docs.filter((d) => matches(d, f));
    },
    async findOne(f: unknown) {
      spy.findOne(f);
      return docs.find((d) => matches(d, f)) ?? null;
    },
    async updateOne(f: unknown, u: unknown) {
      spy.updateOne(f, u);
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
      spy.deleteMany(f);
      const before = docs.length;
      docs = docs.filter((d) => !matches(d, f));
      return before - docs.length;
    },
  };
  return { col, spy, all: () => docs };
}

beforeEach(() => {
  vi.spyOn(console, 'warn').mockImplementation(() => {});
});
afterEach(() => {
  vi.restoreAllMocks();
});

// ---------------------------------------------------------------------------
// Config validation
// ---------------------------------------------------------------------------

describe('validateWebhookConfig', () => {
  it('rejects a non-http endpoint', () => {
    expect(() => validateWebhookConfig({ endpoint: 'ftp://x.test' })).toThrow(/must start with http/);
  });

  it('rejects enabled without any endpoint', () => {
    expect(() => validateWebhookConfig({ enabled: true })).toThrow(/requires `endpoint`/);
  });

  it('accepts enabled when all three per-op endpoints are given', () => {
    expect(() =>
      validateWebhookConfig({
        enabled: true,
        insert_endpoint: 'https://a.test',
        update_endpoint: 'https://b.test',
        delete_endpoint: 'https://c.test',
      }),
    ).not.toThrow();
  });

  it('warns but does not throw on plaintext http to a remote host', () => {
    expect(() => validateWebhookConfig({ endpoint: 'http://remote.test/h' })).not.toThrow();
    expect(console.warn).toHaveBeenCalledWith(expect.stringContaining('plaintext HTTP'));
  });

  it('stays silent for http on localhost', () => {
    expect(() => validateWebhookConfig({ endpoint: 'http://localhost:3000/h' })).not.toThrow();
    expect(console.warn).not.toHaveBeenCalled();
  });
});

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

describe('createWebhookDispatcher', () => {
  it('returns null when disabled, so the write path pays nothing', () => {
    expect(createWebhookDispatcher(undefined)).toBeNull();
    expect(createWebhookDispatcher({ enabled: false, endpoint: 'https://a.test' })).toBeNull();
  });

  it('maps each op to its HTTP verb', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher(config({ fetch: fn }))!;
    d.emit({ op: 'insert', collection: 'c', id: '1', document: { _id: '1' } });
    d.emit({ op: 'update', collection: 'c', id: '2', document: { _id: '2' } });
    d.emit({ op: 'delete', collection: 'c', id: '3' });
    await d.flush();
    expect(calls.map((c) => c.method)).toEqual(['POST', 'PUT', 'DELETE']);
  });

  it('omits the document on delete and includes it otherwise', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher(config({ fetch: fn }))!;
    d.emit({ op: 'insert', collection: 'notes', id: '1', document: { _id: '1', title: 'a' } });
    d.emit({ op: 'delete', collection: 'notes', id: '1' });
    await d.flush();
    expect(calls[0].body).toMatchObject({ collection: 'notes', id: '1', document: { title: 'a' } });
    expect(calls[1].body).toMatchObject({ collection: 'notes', id: '1' });
    expect(calls[1].body.document).toBeUndefined();
    expect(calls[0].body.timestamp).toEqual(expect.any(Number));
  });

  it('sends configured headers alongside content-type', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher(
      config({ fetch: fn, headers: { Authorization: 'Bearer t' } }),
    )!;
    d.emit({ op: 'delete', collection: 'c', id: '1' });
    await d.flush();
    expect(calls[0].headers).toMatchObject({
      'content-type': 'application/json',
      Authorization: 'Bearer t',
    });
  });

  it('honours per-op endpoint overrides', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher(
      config({ fetch: fn, delete_endpoint: 'https://api.test/gone' }),
    )!;
    d.emit({ op: 'insert', collection: 'c', id: '1', document: { _id: '1' } });
    d.emit({ op: 'delete', collection: 'c', id: '1' });
    await d.flush();
    expect(calls[0].url).toBe('https://api.test/hook');
    expect(calls[1].url).toBe('https://api.test/gone');
  });

  it('strips exclude_fields from the payload', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher(config({ fetch: fn, exclude_fields: ['embedding'] }))!;
    d.emit({
      op: 'insert',
      collection: 'docs',
      id: '1',
      document: { _id: '1', title: 'a', embedding: [0.1, 0.2, 0.3] },
    });
    await d.flush();
    const doc = calls[0].body.document as Record<string, unknown>;
    expect(doc.title).toBe('a');
    expect(doc).not.toHaveProperty('embedding');
  });

  it('skips collections outside the configured list, and reserved ones always', () => {
    const { fn } = recordingFetch();
    const d = createWebhookDispatcher(config({ fetch: fn, collections: ['notes'] }))!;
    expect(d.reports('notes')).toBe(true);
    expect(d.reports('secrets')).toBe(false);
    expect(d.reports('_internal')).toBe(false);
  });

  it('retries 5xx then succeeds, counting one delivery', async () => {
    const { fn, calls } = recordingFetch((n) => ({ status: n < 3 ? 503 : 200 }));
    const d = createWebhookDispatcher(config({ fetch: fn }))!;
    d.emit({ op: 'delete', collection: 'c', id: '1' });
    await d.flush(10_000);
    expect(calls).toHaveLength(3);
    expect(d.stats()).toMatchObject({ delivered: 1, failed: 0, pending: 0 });
  });

  it('retries network errors', async () => {
    const { fn, calls } = recordingFetch((n) => (n < 2 ? new Error('ECONNREFUSED') : { status: 200 }));
    const d = createWebhookDispatcher(config({ fetch: fn }))!;
    d.emit({ op: 'delete', collection: 'c', id: '1' });
    await d.flush(10_000);
    expect(calls).toHaveLength(2);
    expect(d.stats().delivered).toBe(1);
  });

  it('does not retry 4xx — a rejected request stays rejected', async () => {
    const { fn, calls } = recordingFetch(() => ({ status: 401 }));
    const d = createWebhookDispatcher(config({ fetch: fn }))!;
    d.emit({ op: 'delete', collection: 'c', id: '1' });
    await d.flush(10_000);
    expect(calls).toHaveLength(1);
    expect(d.stats()).toMatchObject({ delivered: 0, failed: 1 });
  });

  it('gives up after the configured retries', async () => {
    const { fn, calls } = recordingFetch(() => ({ status: 500 }));
    const d = createWebhookDispatcher(config({ fetch: fn, retries: 2 }))!;
    d.emit({ op: 'delete', collection: 'c', id: '1' });
    await d.flush(10_000);
    expect(calls).toHaveLength(3); // initial + 2 retries
    expect(d.stats().failed).toBe(1);
  });

  it('preserves per-document order, so an update cannot overtake its insert', async () => {
    const { fn, calls } = recordingFetch(() => ({ status: 200 }));
    const d = createWebhookDispatcher(config({ fetch: fn }))!;
    d.emit({ op: 'insert', collection: 'c', id: 'same', document: { _id: 'same', v: 1 } });
    d.emit({ op: 'update', collection: 'c', id: 'same', document: { _id: 'same', v: 2 } });
    d.emit({ op: 'delete', collection: 'c', id: 'same' });
    await d.flush(10_000);
    expect(calls.map((c) => c.method)).toEqual(['POST', 'PUT', 'DELETE']);
  });

  it('drops rather than blocking when the queue is saturated', async () => {
    // Never resolves until released — every event stays pending.
    let release!: () => void;
    const gate = new Promise<void>((r) => { release = r; });
    const fn = vi.fn(async () => {
      await gate;
      return { ok: true, status: 200, statusText: '' } as Response;
    }) as unknown as typeof fetch;

    const d = createWebhookDispatcher(config({ fetch: fn, max_queue: 2 }))!;
    for (let i = 0; i < 5; i++) {
      d.emit({ op: 'delete', collection: 'c', id: `id${i}` });
    }
    expect(d.stats().pending).toBe(2);
    expect(d.stats().dropped).toBe(3);
    expect(console.warn).toHaveBeenCalledWith(expect.stringContaining('queue full'));
    release();
    await d.flush(10_000);
  });

  it('flush resolves false when the queue does not drain in time', async () => {
    const fn = vi.fn(() => new Promise<Response>(() => {})) as unknown as typeof fetch;
    const d = createWebhookDispatcher(config({ fetch: fn }))!;
    d.emit({ op: 'delete', collection: 'c', id: '1' });
    expect(await d.flush(50)).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// Write-path wrapper
// ---------------------------------------------------------------------------

describe('wrapCollectionWithWebhook', () => {
  function setup(seed: Document[] = [], over: Partial<WebhookConfig> = {}) {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher(config({ fetch: fn, ...over }))!;
    const { col, spy, all } = fakeCollection(seed);
    return { col: wrapCollectionWithWebhook(col, 'notes', d), raw: col, d, calls, spy, all };
  }

  it('emits an insert carrying the minted id', async () => {
    const { col, d, calls } = setup();
    const id = await col.insert({ title: 'a' });
    await d.flush();
    expect(calls).toHaveLength(1);
    expect(calls[0].method).toBe('POST');
    expect(calls[0].body).toMatchObject({ collection: 'notes', id, document: { _id: id, title: 'a' } });
  });

  it('emits one insert per document for insertMany, paired to the right id', async () => {
    const { col, d, calls } = setup();
    const ids = await col.insertMany([{ title: 'a' }, { title: 'b' }]);
    await d.flush();
    expect(calls).toHaveLength(2);
    expect(calls.map((c) => c.body.id)).toEqual(ids);
    expect((calls[0].body.document as Document).title).toBe('a');
    expect((calls[1].body.document as Document).title).toBe('b');
  });

  it('takes the fast path for a bare _id filter, issuing no resolving query', async () => {
    const { col, d, calls, spy } = setup([{ _id: 'id1', title: 'a' }]);
    await col.updateOne({ _id: 'id1' }, { $set: { title: 'b' } });
    await d.flush();
    // findOne is called once — for the post-image — never to resolve the filter.
    expect(spy.findOne).toHaveBeenCalledTimes(1);
    expect(spy.find).not.toHaveBeenCalled();
    expect(calls[0].method).toBe('PUT');
    expect((calls[0].body.document as Document).title).toBe('b');
  });

  it('resolves a non-_id filter to ids before mutating', async () => {
    const { col, d, calls, spy } = setup([
      { _id: 'id1', tag: 'x', title: 'a' },
      { _id: 'id2', tag: 'x', title: 'b' },
      { _id: 'id3', tag: 'y', title: 'c' },
    ]);
    await col.deleteMany({ tag: 'x' });
    await d.flush();
    expect(spy.find).toHaveBeenCalledWith({ tag: 'x' });
    expect(calls).toHaveLength(2);
    expect(calls.every((c) => c.method === 'DELETE')).toBe(true);
    expect(calls.map((c) => c.body.id).sort()).toEqual(['id1', 'id2']);
  });

  it('sends the post-commit document on update, not the pre-image', async () => {
    const { col, d, calls } = setup([{ _id: 'id1', title: 'before', keep: 1 }]);
    await col.updateOne({ _id: 'id1' }, { $set: { title: 'after' } });
    await d.flush();
    expect(calls[0].body.document).toMatchObject({ _id: 'id1', title: 'after', keep: 1 });
  });

  it('emits nothing when a mutation matches no documents', async () => {
    const { col, d, calls } = setup([{ _id: 'id1', title: 'a' }]);
    expect(await col.updateOne({ _id: 'nope' }, { $set: { title: 'x' } })).toBe(false);
    expect(await col.deleteOne({ _id: 'nope' })).toBe(false);
    expect(await col.deleteMany({ tag: 'absent' })).toBe(0);
    await d.flush();
    expect(calls).toHaveLength(0);
  });

  it('reports a delete when the post-image is gone by read time', async () => {
    // Models a concurrent delete landing between the update commit and the
    // post-image read: there is no document to PUT, so the receiver is told the
    // id is gone rather than being sent a stale body.
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher(config({ fetch: fn }))!;
    const { col: raw } = fakeCollection([{ _id: 'id1', title: 'a' }]);
    raw.findOne = (async () => null) as typeof raw.findOne;
    const col = wrapCollectionWithWebhook(raw, 'notes', d);
    await col.updateOne({ _id: 'id1' }, { $set: { title: 'b' } });
    await d.flush();
    expect(calls).toHaveLength(1);
    expect(calls[0].method).toBe('DELETE');
  });

  it('returns the collection untouched for an unreported collection', async () => {
    const { fn, calls } = recordingFetch();
    const d = createWebhookDispatcher(config({ fetch: fn, collections: ['other'] }))!;
    const { col: raw } = fakeCollection();
    const col = wrapCollectionWithWebhook(raw, 'notes', d);
    expect(col).toBe(raw);
    await col.insert({ title: 'a' });
    await d.flush();
    expect(calls).toHaveLength(0);
  });

  it('passes the mutation result through unchanged', async () => {
    const { col } = setup([
      { _id: 'id1', tag: 'x' },
      { _id: 'id2', tag: 'x' },
    ]);
    expect(await col.updateMany({ tag: 'x' }, { $set: { done: true } })).toBe(2);
    expect(await col.deleteMany({ tag: 'x' })).toBe(2);
  });
});
