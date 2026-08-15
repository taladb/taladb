# Change Webhook

TalaDB is an embedded database. It stores and queries data on the device and does
not replicate. When a backend needs to know about local writes, enable the change
webhook: every committed mutation fires one HTTP request.

```ts
const db = await openDB('app.db', {
  webhook: {
    enabled: true,
    endpoint: 'https://api.example.com/taladb',
    headers: { Authorization: `Bearer ${token}` },
  },
});
```

That is the whole setup. Writes continue to work exactly as before; the requests
happen after each commit.

## Requests

The verb carries the operation, so a REST backend can route on method alone.

| Mutation | Method | Body |
|---|---|---|
| `insert`, `insertMany` | `POST` | `{ collection, id, document, timestamp }` |
| `updateOne`, `updateMany` | `PUT` | `{ collection, id, document, timestamp }` |
| `deleteOne`, `deleteMany` | `DELETE` | `{ collection, id, timestamp }` |

```json
{
  "collection": "notes",
  "id": "01J4XQ8ZK3N7YB2W5V9MTC6RDA",
  "document": { "_id": "01J4XQ8ZK3N7YB2W5V9MTC6RDA", "title": "Draft", "_changed_at": 1755212400000 },
  "timestamp": 1755212400123
}
```

A batch mutation fires **one request per affected document**, not one per call:
`deleteMany({ archived: true })` matching 12 documents sends 12 `DELETE`s. Events
for the same document always arrive in the order they happened — an update can
never overtake its own insert — while different documents are delivered
concurrently.

`PUT` carries the document as it stands *after* the commit, which is what `PUT`
means. It is not a patch: there is no changed-fields diff.

## Delivery guarantee: at most once

Events are queued in memory and sent after the write commits. **A crash, a closed
tab, or a process exit drops whatever is still in flight.**

This is a notification channel, not a replication log. If your backend must never
miss a write, it needs its own reconciliation pass — a periodic full read, a
nightly job, a version counter it can compare. Do not build a system whose
correctness depends on every webhook arriving.

What you get in exchange is that a slow or failing endpoint can never stall a
write. The queue is bounded (512 events by default); when it saturates, new
events are dropped with a console warning rather than applying back-pressure to
the database.

### Retries

Failed deliveries retry three times with 200 / 400 / 800 ms backoff.

- **5xx and network errors** are retried — the endpoint may recover.
- **4xx is not retried.** A 401 or a malformed body will fail identically on the
  next attempt; retrying only makes it more expensive.

### Draining before close

`db.close()` drains the queue before tearing down the handle. To drain at another
moment — before a tab unloads, say — call `flushWebhook`:

```ts
window.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden') void db.flushWebhook(2000);
});
```

It resolves `true` if the queue emptied and `false` on timeout.

## Configuration

```ts
interface WebhookConfig {
  enabled?: boolean;              // default false — the block is inert without it
  endpoint?: string;              // required when enabled
  headers?: Record<string, string>;
  insert_endpoint?: string;       // per-op overrides
  update_endpoint?: string;
  delete_endpoint?: string;
  collections?: string[];         // omit to report every collection
  exclude_fields?: string[];      // fields stripped from every payload
  max_queue?: number;             // default 512
  retries?: number;               // default 3
  fetch?: typeof fetch;           // defaults to the runtime global
}
```

### `exclude_fields` — the one you will want

TalaDB documents routinely carry embeddings. A 768-dimension vector is roughly
9 KB of JSON, on every write, that a webhook receiver almost never wants:

```ts
webhook: {
  enabled: true,
  endpoint: 'https://api.example.com/taladb',
  exclude_fields: ['embedding'],
}
```

### `collections` — report only what matters

```ts
webhook: { enabled: true, endpoint: '…', collections: ['orders', 'invoices'] }
```

Collections outside the list skip the webhook path entirely, including the id
resolution described below — so a chatty local cache costs nothing. Reserved
(`_`-prefixed) collections are never reported.

### Config file

On Node.js the same settings can live in `taladb.config.yml`, alongside the
engine's own `durability` block:

```yaml
durability:
  flush_every_write: true

webhook:
  enabled: true
  endpoint: "https://api.example.com/taladb"
  exclude_fields:
    - embedding
```

The engine reads `durability` and ignores `webhook`; the client reads `webhook`.
An inline `openDB({ webhook })` option takes precedence over the file, and is the
only way to configure the webhook in the browser and on React Native, where there
is no config file to discover.

## Security

Payloads carry document bodies. Use `https://` in production — TalaDB warns on a
plaintext `http://` endpoint to any non-localhost host.

The webhook sends data outward and never accepts anything inward: there is no
endpoint on the database, and a response body is ignored beyond its status code.
A compromised receiver cannot write to the device.

## Runtimes

Identical on all three. Delivery is implemented once in the `taladb` TypeScript
client on top of `fetch`, above the platform bindings, so Node.js, the browser
(both the SharedWorker and the in-memory fallback), and React Native run the same
code and behave the same way.

## Cost on the write path

With the webhook disabled there is no wrapper and no cost.

With it enabled, the cost depends on the filter shape:

- **`{ _id: '…' }`** — the common case, and what `useMutation` generates — needs no
  resolving query. The mutation runs as normal, plus one read for the `PUT`
  post-image.
- **Any other filter** — `deleteMany({ archived: true })` — needs one query first,
  to learn which ids the mutation will affect. Updates and deletes take filters
  and return only counts, so the ids are not otherwise available.

There is one consequence worth stating plainly. For a non-`_id` filter, the
resolving query and the mutation are separate transactions, so a concurrent write
landing between them can make the reported id set differ slightly from the set
actually mutated. Under at-most-once delivery that means a notification may be
missed or extra — **the mutation itself is never affected**, and the database is
never inconsistent. Filters on `_id` have no such window at all.

## Observability

```ts
db.webhookStats();
// { pending: 3, delivered: 128, failed: 1, dropped: 0 }
```

`dropped` above zero means the queue saturated and events were discarded — the
endpoint is not keeping up. Raise `max_queue`, narrow `collections`, or make the
receiver faster.
