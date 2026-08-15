---
title: Local-First Data (@taladb/react/query)
description: React Query-shaped hooks whose cache is a real TalaDB collection. Reads hydrate from your server and revalidate in the background; writes commit locally and drain to the network without blocking the UI.
---

# Local-First Data (`@taladb/react/query`)

`useQuery` and `useMutation` in the shape React developers already know — but the
cache is a real TalaDB collection rather than an in-memory store.

Three things follow from that, and they are the whole reason this exists:

- **It survives reload** with no rehydration step, because it was never a
  serialised snapshot.
- **It stays queryable offline.** `find`, `$sort`, `aggregate`, and full-text
  search all run over the data you fetched.
- **Writes never block the UI.** A mutation commits locally and drains to your
  server in the background — no spinner, and the write survives a reload, a
  crash, or a flight.

```bash
pnpm add taladb @taladb/web @taladb/react
```

```ts
import { useQuery, useMutation } from '@taladb/react/query'
```

It ships as a subpath, so an app that only uses the [local hooks](/guide/react)
never pulls any of it into the bundle.

::: warning This is not a cache
The documents land in **your** collections — the same ones `useFind` reads. They
are durable data, not a disposable projection. Nothing here is ever deleted by
age: a TTL marks a query stale so it refetches, and that is all it does.
:::

---

## Setup

`QueryProvider` nests inside [`TalaDBProvider`](/guide/react#setup), so it
inherits the client-only `openDB` lifecycle — which is also why this works in
Next.js without touching server rendering.

```tsx
import { TalaDBProvider } from '@taladb/react'
import { QueryProvider } from '@taladb/react/query'

root.render(
  <TalaDBProvider name="myapp.db" fallback={<Splash />}>
    <QueryProvider headers={() => ({ Authorization: `Bearer ${getToken()}` })}>
      <App />
    </QueryProvider>
  </TalaDBProvider>,
)
```

That is the whole setup. Endpoints are not configured here — they live on the
hooks that use them.

`headers` is resolved **at send time**, not when the write was made, so a write
queued while offline and sent an hour later carries a current token rather than
the expired one it was made with. That timing is why it belongs on the provider.

### `classify` (optional)

By default `2xx` succeeds, `409` means already-applied, `5xx`/`408`/`429` retry,
and any other `4xx` is terminal. Override it only if your API disagrees:

```tsx
<QueryProvider classify={(res) => (res.status === 418 ? 'terminal' : undefined)}>
```

---

## The backend contract

This library owns a local queue and a `fetch`. It owns no transport, no cursors,
and no conflict resolution — **your server is yours**. In exchange it asks for
five things, and breaks in quiet, delayed ways if it does not get them.

| # | Rule | What breaks without it |
|---|---|---|
| 1 | **Accept `_id` from the body and never reassign it** | Every write duplicates on the server |
| 2 | **Writes are idempotent** | A retry after a timeout silently creates a second row |
| 3 | **Classify errors explicitly** (see below) | The queue stalls on an error that can never succeed |
| 4 | **Return the stored document** | The local copy drifts, with no symptom until a refetch |
| 5 | **No cross-document ordering requirements** | Per-document order is all that is guaranteed |

In development every one of these is checked at runtime, and a violation logs a
warning naming the rule and its consequence. Those checks are stripped from
production builds.

### The four outcomes

A response maps onto one of four things the queue can do. The second is the one
people forget:

| Outcome | Meaning |
|---|---|
| `ok` | Applied. The body is the canonical document. |
| `applied` | **Already applied** — typically a `409` answering a retry whose first attempt succeeded but never replied. Without this the queue stalls on it forever. |
| `retry` | May recover (`5xx`, network). Backs off and tries again. |
| `terminal` | Will never succeed (`4xx`). Parked for a human decision. |

---

## Reading

```tsx
const { data, loading, stale, error, refetch } = useQuery<Todo>({
  collection: 'todos',
  key: ['todos', { status }],
  fetch: ({ signal }) => api.get(`/todos?status=${status}`, { signal }),
  ttl: 5 * 60_000,
})
```

- **`collection`** is where documents live. **`key`** is what goes stale. They
  are separate on purpose: one document belongs to many queries, each with its
  own freshness.
- **`loading`** is true only when nothing is cached at all. A warm key renders
  instantly and revalidates behind it, reporting `stale` while it does — a
  reload never shows a spinner over data the device already holds.
- **`error`** never clears `data`. A failed refresh leaves what you had.
- **`data`** is resolved through the local database, not from the fetch
  response, so a local write to any document in the set re-renders the query
  immediately. There is nothing to invalidate — the collection *is* the state.

Documents arrive in the order your server returned them.

::: tip Deletes you cannot see
If a refetch returns fewer documents, they leave that query's result set but are
not deleted. From the client, "no longer matches this filter" and "deleted
upstream" are indistinguishable — and one of them is not yours to act on.
:::

---

## Writing

```tsx
const { mutate, mutateAsync, pending, error } = useMutation<Todo>({
  collection: 'todos',
  url: '/api/v2/todos/:id',
  method: { update: 'PATCH' },   // defaults to POST / PUT / DELETE
})

mutate({ type: 'insert', doc: { title: 'Buy milk' } })   // POST   /api/v2/todos
mutate({ type: 'update', where: { _id }, set: { done: true } })  // PATCH  /api/v2/todos/:id
mutate({ type: 'delete', where: { _id } })               // DELETE /api/v2/todos/:id
```

`:id` and `:collection` interpolate, and an insert drops the trailing `/:id`, so
one template covers all three verbs.

::: tip Why a template and not a function?
Because under `optimistic` and `queued` the request is sent by the drain loop —
after a route change, a reload, possibly a week later. By then the component
that would have held a closure is long gone. A template is *data*, so it is
stored on the queued document and travels with it.
:::

Every `useQuery` and `useFind` watching those documents re-renders on the local
commit, long before the network hears about it.

### The three modes

| `mode` | What happens |
|---|---|
| `optimistic` *(default)* | Commit locally, return, send in the background as soon as possible. `pending` is always `false` — the UI never waits. |
| `immediate` | An ordinary `fetch` first; write locally only once the server agrees. `pending` reflects the request. |
| `queued` | Commit locally and wait for the provider's schedule. For high-volume writes worth batching. |

Use **`immediate`** where showing success before the server confirms would
mislead: checkout, payment, anything irreversible. It is a plain request — no
queue, no retries, nothing stored until it succeeds, so there is nothing to roll
back if it fails. It works with just a `url` and no provider at all:

```tsx
const { mutateAsync, pending } = useMutation<Order>({
  collection: 'orders',
  url: '/api/orders/:id',
  mode: 'immediate',
})

await mutateAsync({ type: 'insert', doc: cart })  // throws if the server says no
```

::: warning `where` must be `{ _id }`
On a secondary browser tab a filter-based write is forwarded to the primary and
re-evaluated there against authoritative data — so `{ done: false }` can affect a
different set of documents than the one your UI just showed.
:::

### When writes are sent

Timing lives on the provider, not the call site:

```tsx
<QueryProvider drain={{ policy: 'interval', intervalMs: 30_000 }}>
```

| Policy | Behaviour |
|---|---|
| `auto` | Default. Sends an `optimistic` write as soon as it commits. |
| `interval` | Batches on a timer (`intervalMs`, default 30s). |
| `online-only` | Waits for connectivity. |
| `manual` | Only when you ask. |

A `queued` mutation ignores `auto` and waits for a trigger either way — that is
the difference between it and `optimistic`.

Reconnecting drains under every policy but `manual` — a backlog built offline is
exactly what is waiting.

### Retries

`retry` outcomes back off at 1s, doubling to a five-minute ceiling, with ±25%
jitter so a fleet of clients does not reconnect in lockstep. `terminal` outcomes
are never retried automatically; they need a decision.

---

## Showing what the queue is doing

Optimistic writes are this library's biggest hazard: a request that fails after
the user has navigated away has no UI left to fail into. **Render this
somewhere**, or a write can be lost with no symptom at all.

```tsx
import { useSyncStatus } from '@taladb/react/query'

function SyncBadge() {
  const { pending, failed, online, retry, discard } = useSyncStatus()

  if (failed.length > 0) {
    return (
      <Alert>
        {failed.length} change(s) couldn’t be saved.
        <button onClick={() => failed.forEach((f) => retry(f.id))}>Try again</button>
        <button onClick={() => failed.forEach((f) => discard(f.id))}>Discard</button>
      </Alert>
    )
  }
  if (!online) return <Badge>Offline — {pending} change(s) queued</Badge>
  return pending > 0 ? <Badge>Saving…</Badge> : null
}
```

Each entry in `failed` carries its `collection`, `id`, `op`, `error`, and
`attempts`.

::: warning `discard` is not undo
No previous version is stored. Discarding a failed **insert** deletes the
document, since the server never knew it existed. Discarding a failed **update**
or **delete** keeps the document and stops retrying — the local values stand
until the next fetch of a query containing it replaces them.
:::

---

## Multi-tab

The queue drains on the **primary tab** only — the one that owns the storage.
This is a correctness requirement, not an optimisation: a secondary tab reads a
pending set that lags by up to half a second, and its own bookkeeping is itself
a forwarded write applied later, so it would re-send what it just sent.

Nothing is required of you. See [`isPrimary()`](/guide/web#isprimary) if you
have your own background work with the same constraint.

---

## What this layer adds to your documents

Eight reserved fields, on every document it manages:

| Field | Purpose |
|---|---|
| `_sync` | `'synced'`, `'pending'`, or `'failed'` — this *is* the queue |
| `_op` | What the document still owes the server |
| `_attempt` | Retry count |
| `_error` | Last terminal error |
| `_fetched_at` | When the server last spoke about it |
| `_retry_at` | When it becomes eligible again |
| `_endpoint` | The URL template this write is going to |
| `_method` | The verb for the current `_op` |

They live on the document rather than in a separate outbox so every queued write
is a single atomic commit. **Do not use these names in your own schemas** — a
server response carrying `_sync` would overwrite the envelope and drop the
document out of the queue. Development builds warn if one appears.

One consequence worth knowing: **at most one operation is pending per document**.
A second edit folds into the first rather than queueing behind it, so the server
sees the final state, not the sequence that produced it. That is what keeps this
small — no dependency graph, no id remapping, no replay log. It also means
`$inc`-style operations cannot be expressed while offline.

Deleting is a **tombstone**: the document stays until the server confirms,
because the record is what carries the queued delete. Queries filter it out.

---

## Next

- [React Hooks](/guide/react) — the local-only hooks this builds on
- [Full-Text & Hybrid Search](/api/search) — run it over what you fetched
- [Multi-tab behaviour](/guide/web#multi-tab-behaviour) — how the primary is chosen
