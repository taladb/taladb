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

  /**
   * `at` is the moment the event was *queued* — i.e. just after the write
   * committed — not the moment it is sent.
   *
   * The distinction matters because delivery is neither immediate nor
   * order-free: events for one document are chained, and a failing endpoint
   * adds up to 1.4s of retry backoff per event ahead of this one in the chain.
   * Stamping at send time would make `timestamp` drift arbitrarily from the
   * mutation it describes, which is exactly what a receiver reaching for that
   * field is trying to order by.
   */
  function bodyFor(event: WebhookEvent, at: number): string {
    const base = {
      collection: event.collection,
      id: event.id,
      timestamp: at,
    };
    return JSON.stringify(
      event.op === 'delete'
        ? base
        : { ...base, document: stripFields(event.document, exclude) },
    );
  }

  /** One delivery attempt chain. Resolves when delivered or permanently failed. */
  async function deliver(event: WebhookEvent, at: number): Promise<void> {
    const url = endpointFor(event.op);
    const init: RequestInit = {
      method: METHOD[event.op],
      headers,
      body: bodyFor(event, at),
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
      // Stamped here, at commit time — see `bodyFor`.
      const at = Date.now();
      const key = `${event.collection} ${event.id}`;
      const prior = chains.get(key) ?? Promise.resolve();
      const next = prior
        .then(() => deliver(event, at))
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
      while (stats.pending > 0) {
        const remaining = deadline - Date.now();
        if (remaining <= 0) break;
        // The timer is cleared whichever side of the race wins. Left armed it
        // holds Node's event loop open for the rest of the timeout *after* the
        // queue has drained — `await db.close()` would resolve promptly and the
        // process would still sit there for five seconds before exiting.
        let timer: ReturnType<typeof setTimeout> | undefined;
        const expired = new Promise<void>((resolve) => {
          timer = setTimeout(resolve, remaining);
        });
        try {
          await Promise.race([Promise.allSettled([...chains.values()]), expired]);
        } finally {
          clearTimeout(timer);
        }
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
  /**
   * Used only to resolve ids, and only when present — every platform adapter
   * has it, but the structural type stays honest about the wrapper needing
   * nothing more than a fallback to `find` if one ever does not.
   */
  aggregate?<R extends Document = Document>(pipeline: unknown): Promise<R[]>;
}

/** Ids as an engine filter. `_id` `$in` is planned as a direct key lookup. */
function byIds(ids: string[]): unknown {
  return ids.length === 1 ? { _id: ids[0] } : { _id: { $in: ids } };
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
 * ## Where this sits in the stack
 *
 * **Directly on the platform adapter, underneath `applySchema`.** That order is
 * load-bearing in three ways, and getting it backwards is silently wrong:
 *
 * 1. `applySchema` narrows write filters through `writableFilter` (the `_v`
 *    guard that stops this build from clobbering a newer peer's documents). The
 *    filter that reaches this wrapper is therefore the *same* one the mutation
 *    runs, so the ids reported are exactly the ids touched. Wrapped the other
 *    way round, resolution sees the unguarded filter and reports deletions of
 *    documents the guard had spared.
 * 2. Resolution reads go straight to the engine, so they cannot be failed by
 *    `validateOnRead` or trigger a `persistMigrations` write-back. Enabling a
 *    notification channel must not be able to make a delete throw.
 * 3. Inserts arrive already carrying `_v`, so the reported document matches
 *    what was stored rather than what the caller passed in.
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

  /**
   * Ids matching `filter`, using the bare-`_id` fast path when possible.
   *
   * The multi-document path projects to `_id` rather than calling `find`. On a
   * vector database the difference is not marginal: `find` decodes every
   * matching document in full, embeddings included, and then everything but the
   * id is thrown away. `exclude_fields` does not help here — it trims the
   * request body, not the read.
   */
  async function idsFor(filter: unknown, limitOne: boolean): Promise<string[]> {
    const fast = idFromFilter(filter);
    if (fast !== null) return [fast];
    if (limitOne) {
      const doc = await col.findOne(filter);
      return doc && typeof doc._id === 'string' ? [doc._id] : [];
    }
    const docs = col.aggregate
      ? await col.aggregate([{ $match: filter }, { $project: { _id: 1 } }])
      : await col.find(filter);
    return docs.map((d) => d._id).filter((id): id is string => typeof id === 'string');
  }

  /**
   * Emit `op` events carrying each id's post-commit document.
   *
   * ## One query for the batch, not one per id
   *
   * `updateMany` over a large match set was issuing N sequential round trips
   * *inside the caller's await*, so the write call scaled with the size of the
   * match rather than with the cost of the write. `_id`-`$in` is planned as a
   * direct key lookup, so the batched read is also cheaper per document than
   * the point reads it replaces.
   *
   * ## Why inserts are read back too
   *
   * The obvious shortcut on the insert path is to report the document the
   * caller passed plus the minted `_id` — no read at all. It is wrong: the
   * engine stamps `_changed_at` and `applySchema` stamps `_v`, so the shortcut
   * ships a body that is missing fields the stored document has, and only on
   * `POST`. A receiver that persists what it is handed then holds a different
   * document depending on which verb delivered it. One batched read per insert
   * *call* (not per document) buys one payload shape for all three verbs.
   */
  async function emitPostImages(ids: string[], op: 'insert' | 'update'): Promise<void> {
    if (ids.length === 0) return;
    const docs = await col.find(byIds(ids));
    const byId = new Map<string, T>();
    for (const doc of docs) {
      if (typeof doc._id === 'string') byId.set(doc._id, doc);
    }
    // Emission follows `ids`, not result order, so a document's events stay in
    // the order the mutations happened.
    for (const id of ids) {
      const doc = byId.get(id);
      // Absent means something else deleted it between the commit and this
      // read. There is no post-image to report, so report the deletion — the
      // receiver's view of "this id is gone" is correct either way.
      if (doc === undefined) webhook.emit({ op: 'delete', collection, id });
      else webhook.emit({ op, collection, id, document: doc });
    }
  }

  return {
    ...col,

    async insert(doc) {
      const id = await col.insert(doc);
      await emitPostImages([id], 'insert');
      return id;
    },

    async insertMany(docs) {
      const ids = await col.insertMany(docs);
      await emitPostImages(ids, 'insert');
      return ids;
    },

    async updateOne(filter, update) {
      const ids = await idsFor(filter, true);
      const changed = await col.updateOne(filter, update);
      if (changed) await emitPostImages(ids, 'update');
      return changed;
    },

    async updateMany(filter, update) {
      const ids = await idsFor(filter, false);
      const n = await col.updateMany(filter, update);
      if (n > 0) await emitPostImages(ids, 'update');
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
