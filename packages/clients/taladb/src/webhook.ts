// Change webhook — outbound HTTP notification for local writes.
//
// TalaDB is an embedded database and does not replicate. When an application
// wants a backend to learn about local writes, it enables this: every committed
// mutation fires one HTTP request, keyed by verb.
//
//   insert → POST    {endpoint}   { collection, id, document, timestamp }
//   update → PUT     {endpoint}   { collection, id, document, timestamp }
//   delete → DELETE  {endpoint}   { collection, id, timestamp }
//
// Delivery lives here, in TypeScript, rather than in the Rust core, for one
// reason: the core's HTTP path was `reqwest::blocking` on an OS thread pool,
// which cannot compile to `wasm32`. The webhook was therefore Node- and
// React-Native-only, on a database whose primary runtime is the browser.
// `fetch` exists on all three, so this module does.
//
// # Delivery guarantee: at most once
//
// Events are queued in memory and posted after the write commits. A crash, a
// closed tab, or a process exit drops whatever is still in flight. This is a
// *notification* channel, not a replication log — if the receiver must not miss
// a write, it needs its own reconciliation pass. `flush()` drains the queue and
// is the correct thing to await before closing the database.

import type { Document } from './types';

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/** The three mutation kinds a webhook reports, and the verb each one uses. */
export type WebhookOp = 'insert' | 'update' | 'delete';

const METHOD: Record<WebhookOp, string> = {
  insert: 'POST',
  update: 'PUT',
  delete: 'DELETE',
};

export interface WebhookConfig {
  /**
   * Enable outbound webhooks. Defaults to `false`, so a config block without
   * `enabled: true` is inert — safe to commit and safe to ship.
   */
  enabled?: boolean;
  /** Endpoint receiving every event. Required when `enabled` is `true`. */
  endpoint?: string;
  /** Headers sent with every request — typically `Authorization`. */
  headers?: Record<string, string>;
  /** Per-op endpoint overrides. Fall back to {@link WebhookConfig.endpoint}. */
  insert_endpoint?: string;
  update_endpoint?: string;
  delete_endpoint?: string;
  /**
   * Collections to report. Omit to report all of them. A `_`-prefixed
   * (reserved) collection is never reported either way.
   */
  collections?: string[];
  /**
   * Document fields stripped from every payload.
   *
   * The reason this exists on a vector database: a document carrying a 768-float
   * embedding is ~9KB of JSON that a webhook receiver almost never wants, on
   * every single write. Naming it here keeps it out of the request body.
   *
   * @example exclude_fields: ['embedding', 'clip_vector']
   */
  exclude_fields?: string[];
  /**
   * Max events held in memory before new ones are dropped. Default 512.
   * Back-pressure is a drop, never a block — a slow endpoint must not be able
   * to stall the write path.
   */
  max_queue?: number;
  /** Retry attempts per event after the first try. Default 3. */
  retries?: number;
  /** `fetch` implementation. Defaults to the runtime global. */
  fetch?: typeof fetch;
}

/** Counters for observability. Read via {@link WebhookDispatcher.stats}. */
export interface WebhookStats {
  /** Queued or in flight. */
  pending: number;
  /** Delivered with a 2xx. */
  delivered: number;
  /** Gave up after exhausting retries, or hit a permanent 4xx. */
  failed: number;
  /** Never attempted — the queue was full when the write committed. */
  dropped: number;
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

export type WebhookEvent =
  | { op: 'insert'; collection: string; id: string; document: Document }
  | { op: 'update'; collection: string; id: string; document: Document }
  | { op: 'delete'; collection: string; id: string };

export interface WebhookDispatcher {
  /** True when this collection should report changes. Lets the write wrapper
   * skip pre-image resolution entirely for collections nobody is watching. */
  reports(collection: string): boolean;
  /** Queue an event. Never throws, never blocks the caller. */
  emit(event: WebhookEvent): void;
  /** Resolve once the queue is empty or `timeoutMs` elapses. `true` if drained. */
  flush(timeoutMs?: number): Promise<boolean>;
  stats(): WebhookStats;
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

const ENDPOINT_KEYS = [
  'endpoint',
  'insert_endpoint',
  'update_endpoint',
  'delete_endpoint',
] as const;

const LOCALHOST = new Set(['localhost', '127.0.0.1', '[::1]', '::1']);

function isLocalhost(url: string): boolean {
  try {
    return LOCALHOST.has(new URL(url).hostname);
  } catch {
    return false;
  }
}

/**
 * Validate a webhook config. Throws on the first invalid endpoint.
 *
 * Warns — rather than throws — on plaintext HTTP to a non-localhost host: the
 * payload carries document bodies, so shipping them unencrypted is a real
 * disclosure, but `http://` against a local dev server is routine.
 */
export function validateWebhookConfig(config: WebhookConfig): void {
  for (const key of ENDPOINT_KEYS) {
    const url = config[key];
    if (url === undefined) continue;
    if (!url.startsWith('http://') && !url.startsWith('https://')) {
      throw new Error(
        `TalaDB webhook: invalid ${key} "${url}" — must start with http:// or https://`,
      );
    }
    if (url.startsWith('http://') && !isLocalhost(url)) {
      console.warn(
        `[TalaDB] webhook ${key} "${url}" uses plaintext HTTP — document bodies ` +
          `will cross the network unencrypted. Use HTTPS in production.`,
      );
    }
  }
  if (config.enabled && !config.endpoint) {
    const perOp = ENDPOINT_KEYS.slice(1).every((k) => config[k] !== undefined);
    if (!perOp) {
      throw new Error(
        'TalaDB webhook: `enabled: true` requires `endpoint` (or all three per-op endpoints)',
      );
    }
  }
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

const DEFAULT_MAX_QUEUE = 512;
const DEFAULT_RETRIES = 3;
/** 200ms, 400ms, 800ms — matches the backoff the Rust hook used. */
const BACKOFF_BASE_MS = 200;

const sleep = (ms: number): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, ms));

function stripFields(doc: Document, exclude: Set<string>): Document {
  if (exclude.size === 0) return doc;
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(doc)) {
    if (!exclude.has(k)) out[k] = v;
  }
  return out as Document;
}

/**
 * Build a dispatcher, or return `null` when webhooks are disabled — so the
 * write path can check one nullable field instead of calling into a no-op.
 */
export function createWebhookDispatcher(
  config: WebhookConfig | undefined,
): WebhookDispatcher | null {
  if (!config?.enabled) return null;
  validateWebhookConfig(config);

  const fetchFn = config.fetch ?? globalThis.fetch?.bind(globalThis);
  if (!fetchFn) {
    throw new Error(
      'TalaDB webhook: no global fetch available. Pass `fetch` in the webhook config.',
    );
  }

  const headers = { 'content-type': 'application/json', ...(config.headers ?? {}) };
  const exclude = new Set(config.exclude_fields ?? []);
  const only = config.collections ? new Set(config.collections) : null;
  const maxQueue = config.max_queue ?? DEFAULT_MAX_QUEUE;
  const retries = config.retries ?? DEFAULT_RETRIES;

  const stats: WebhookStats = { pending: 0, delivered: 0, failed: 0, dropped: 0 };

  // Per-document delivery chain. Events for the same (collection, id) must
  // arrive in the order they happened — an insert overtaken by its own update
  // leaves the receiver holding the stale body. Keying the chain by document
  // rather than globally keeps unrelated documents fully concurrent.
  const chains = new Map<string, Promise<void>>();

  function endpointFor(op: WebhookOp): string {
    return (
      config![`${op}_endpoint` as const] ??
      config!.endpoint!
    );
  }

  function bodyFor(event: WebhookEvent): string {
    const base = {
      collection: event.collection,
      id: event.id,
      timestamp: Date.now(),
    };
    return JSON.stringify(
      event.op === 'delete'
        ? base
        : { ...base, document: stripFields(event.document, exclude) },
    );
  }

  /** One delivery attempt chain. Resolves when delivered or permanently failed. */
  async function deliver(event: WebhookEvent): Promise<void> {
    const url = endpointFor(event.op);
    const init: RequestInit = {
      method: METHOD[event.op],
      headers,
      body: bodyFor(event),
    };

    for (let attempt = 0; attempt <= retries; attempt++) {
      try {
        const res = await fetchFn!(url, init);
        if (res.ok) {
          stats.delivered++;
          return;
        }
        // 4xx is the receiver telling us the request is wrong. Retrying an
        // unauthorized or malformed request just repeats it more expensively.
        if (res.status >= 400 && res.status < 500) {
          stats.failed++;
          console.warn(
            `[TalaDB] webhook ${event.op} ${event.collection}/${event.id} → ` +
              `${res.status} ${res.statusText} (not retried)`,
          );
          return;
        }
      } catch {
        // Network error — indistinguishable from 5xx for our purposes, retry.
      }
      if (attempt < retries) await sleep(BACKOFF_BASE_MS * 2 ** attempt);
    }
    stats.failed++;
    console.warn(
      `[TalaDB] webhook ${event.op} ${event.collection}/${event.id} failed after ` +
        `${retries + 1} attempts`,
    );
  }

  return {
    reports(collection: string): boolean {
      if (collection.startsWith('_')) return false;
      return only === null || only.has(collection);
    },

    emit(event: WebhookEvent): void {
      if (!this.reports(event.collection)) return;
      if (stats.pending >= maxQueue) {
        stats.dropped++;
        console.warn(
          `[TalaDB] webhook queue full (${maxQueue}) — dropped ${event.op} ` +
            `${event.collection}/${event.id}. The endpoint is not keeping up.`,
        );
        return;
      }

      stats.pending++;
      const key = `${event.collection} ${event.id}`;
      const prior = chains.get(key) ?? Promise.resolve();
      const next = prior
        .then(() => deliver(event))
        .finally(() => {
          stats.pending--;
          // Drop the chain entry once this was the last link, so a long-lived
          // database doesn't accumulate one Promise per document ever written.
          if (chains.get(key) === next) chains.delete(key);
        });
      chains.set(key, next);
    },

    async flush(timeoutMs = 5000): Promise<boolean> {
      const deadline = Date.now() + timeoutMs;
      // Chains grow while draining (a retry re-arms one), so re-read the map
      // each pass rather than awaiting one snapshot of it.
      while (stats.pending > 0 && Date.now() < deadline) {
        await Promise.race([
          Promise.allSettled([...chains.values()]),
          sleep(Math.max(0, deadline - Date.now())),
        ]);
      }
      return stats.pending === 0;
    },

    stats(): WebhookStats {
      return { ...stats };
    },
  };
}

// ---------------------------------------------------------------------------
// Write-path integration
// ---------------------------------------------------------------------------

/**
 * The slice of `Collection<T>` the webhook wrapper needs. Declared structurally
 * rather than importing `Collection<T>` so this module stays independent of the
 * full collection surface (vector search, FTS, index DDL, …).
 */
interface WebhookWritable<T extends Document> {
  insert(doc: Omit<T, '_id'>): Promise<string>;
  insertMany(docs: Omit<T, '_id'>[]): Promise<string[]>;
  find(filter?: unknown): Promise<T[]>;
  findOne(filter: unknown): Promise<T | null>;
  updateOne(filter: unknown, update: unknown): Promise<boolean>;
  updateMany(filter: unknown, update: unknown): Promise<number>;
  deleteOne(filter: unknown): Promise<boolean>;
  deleteMany(filter: unknown): Promise<number>;
}

/**
 * A filter of exactly `{ _id: "<string>" }` — the shape that lets us skip the
 * pre-image query entirely.
 *
 * This is not a micro-optimisation for an unusual case: it is the dominant one.
 * `useMutation` generates `{ _id }` filters, and so does virtually every
 * "edit this record" handler. Recognising it means the common write costs one
 * engine call with the webhook enabled, exactly as it does without.
 */
function idFromFilter(filter: unknown): string | null {
  if (filter === null || typeof filter !== 'object') return null;
  const keys = Object.keys(filter as Record<string, unknown>);
  if (keys.length !== 1 || keys[0] !== '_id') return null;
  const id = (filter as { _id: unknown })._id;
  return typeof id === 'string' ? id : null;
}

/**
 * Wrap a collection so every committed mutation queues a webhook event.
 *
 * ## Why the ids are resolved here and not in the engine
 *
 * `updateOne`/`deleteMany` take a *filter* and return a count — the affected
 * ids never cross the binding boundary. A webhook keyed on `PUT /:id` needs
 * them, so either the engine grows a new "return the ids you touched" method on
 * all four binding surfaces (napi, two wasm surfaces, and the React Native C
 * FFI plus its C++ JSI layer), or the client resolves the filter itself. This
 * takes the second route.
 *
 * The cost of that choice, stated plainly: for a filter that is *not* a bare
 * `_id`, the resolving query and the mutation are separate transactions, so a
 * concurrent write landing between them can make the reported id set drift from
 * the set actually mutated. Under the at-most-once delivery contract this is a
 * notification that may be missed or extra, never a corrupted database — the
 * mutation itself is unaffected. Bare-`_id` filters (see {@link idFromFilter})
 * have no such window because no resolving query happens at all.
 *
 * Post-images for `update` are read after the commit, so a `PUT` carries the
 * document as it now stands — which is what `PUT` means.
 */
export function wrapCollectionWithWebhook<T extends Document>(
  col: WebhookWritable<T>,
  collection: string,
  webhook: WebhookDispatcher,
): WebhookWritable<T> {
  if (!webhook.reports(collection)) return col;

  /** Ids matching `filter`, using the bare-`_id` fast path when possible. */
  async function idsFor(filter: unknown, limitOne: boolean): Promise<string[]> {
    const fast = idFromFilter(filter);
    if (fast !== null) return [fast];
    if (limitOne) {
      const doc = await col.findOne(filter);
      return doc && typeof doc._id === 'string' ? [doc._id] : [];
    }
    const docs = await col.find(filter);
    return docs.map((d) => d._id).filter((id): id is string => typeof id === 'string');
  }

  /** Emit `update` events carrying each id's post-commit document. */
  async function emitUpdates(ids: string[]): Promise<void> {
    for (const id of ids) {
      const doc = await col.findOne({ _id: id });
      // Absent means something else deleted it between the commit and this
      // read. There is no post-image to report, so report the deletion — the
      // receiver's view of "this id is gone" is correct either way.
      if (doc === null) webhook.emit({ op: 'delete', collection, id });
      else webhook.emit({ op: 'update', collection, id, document: doc });
    }
  }

  return {
    ...col,

    async insert(doc) {
      const id = await col.insert(doc);
      webhook.emit({
        op: 'insert',
        collection,
        id,
        document: { ...doc, _id: id } as unknown as Document,
      });
      return id;
    },

    async insertMany(docs) {
      const ids = await col.insertMany(docs);
      ids.forEach((id, i) => {
        webhook.emit({
          op: 'insert',
          collection,
          id,
          document: { ...docs[i], _id: id } as unknown as Document,
        });
      });
      return ids;
    },

    async updateOne(filter, update) {
      const ids = await idsFor(filter, true);
      const changed = await col.updateOne(filter, update);
      if (changed) await emitUpdates(ids);
      return changed;
    },

    async updateMany(filter, update) {
      const ids = await idsFor(filter, false);
      const n = await col.updateMany(filter, update);
      if (n > 0) await emitUpdates(ids);
      return n;
    },

    async deleteOne(filter) {
      const ids = await idsFor(filter, true);
      const deleted = await col.deleteOne(filter);
      if (deleted) {
        for (const id of ids) webhook.emit({ op: 'delete', collection, id });
      }
      return deleted;
    },

    async deleteMany(filter) {
      const ids = await idsFor(filter, false);
      const n = await col.deleteMany(filter);
      if (n > 0) {
        for (const id of ids) webhook.emit({ op: 'delete', collection, id });
      }
      return n;
    },
  };
}
