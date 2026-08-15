---
title: Local-First Data (@taladb/react/query)
description: React Query-shaped hooks whose cache is a real TalaDB collection. Step-by-step setup, then a full reference for every option and result field.
---

# Local-First Data (`@taladb/react/query`)

`useQuery` and `useMutation` with TanStack Query v5's surface — but the cache is
a real TalaDB collection rather than an in-memory store.

Three things follow from that, and they are the whole reason this exists:

- **It survives reload** with no rehydration step, because it was never a
  serialised snapshot.
- **It stays queryable offline.** `find`, `$sort`, `aggregate`, and full-text
  search all run over the data you fetched.
- **Writes never block the UI.** A mutation commits locally and drains to your
  server in the background — no spinner, and the write survives a reload, a
  crash, or a flight.

Both hooks speak [TanStack Query v5](./react-query-migration.md). If you already
use React Query, read the migration page instead — the differences are short and
this page will be mostly familiar.

::: warning This is not a cache
The documents land in **your** collections — the same ones `useFind` reads. They
are durable data, not a disposable projection. Nothing here is ever deleted by
age: freshness marks a query stale so it refetches, and that is all it does.
:::

---

## Step 1 — Install

```bash
pnpm add taladb @taladb/react
# plus the binding for your platform:
pnpm add @taladb/web            # browser (WASM)
pnpm add @taladb/node           # Node.js
pnpm add @taladb/react-native   # React Native
```

Everything on this page is imported from the `/query` subpath, so an app that
only uses the [local hooks](/guide/react) never pulls any of it into the bundle:

```ts
import { useQuery, useMutation, useSyncStatus } from '@taladb/react/query'
```

## Step 2 — Describe your collections

Register each collection once, on `TalaDBProvider`. This is optional for the
local hooks and **strongly recommended here**, because it is what lets
`useQuery` verify the collection it inferred from your query key instead of
creating one nobody declared.

```tsx
import { z } from 'zod'

const Todo = z.object({
  _id: z.string(),
  title: z.string(),
  done: z.boolean(),
})

const collections = { todos: { schema: Todo } }
```

The schema is any [Standard Schema](https://standardschema.dev) validator — Zod,
Valibot, ArkType. It gives you validation on local writes and a `_v` shape
version on every document.

## Step 3 — Wrap your app

`QueryProvider` nests inside [`TalaDBProvider`](/guide/react#setup), so it
inherits the client-only `openDB` lifecycle — which is also why this works in
Next.js without touching server rendering.

```tsx
import { TalaDBProvider } from '@taladb/react'
import { QueryProvider } from '@taladb/react/query'

root.render(
  <TalaDBProvider name="myapp.db" collections={collections} fallback={<Splash />}>
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

::: tip There is no `QueryClient`
TanStack's client owns its cache; here the database owns it. Anything you passed
to `new QueryClient({ defaultOptions })` goes on `<QueryProvider defaultOptions>`
instead, in the same shape.
:::

## Step 4 — Read

```tsx
function TodoList({ status }: { status: string }) {
  const { data, isPending, isError, error, isFetching } = useQuery({
    queryKey: ['todos', { status }],
    queryFn: ({ signal }) => api.get(`/todos?status=${status}`, { signal }),
    staleTime: 5 * 60_000,
  })

  if (isPending) return <Spinner />
  if (isError) return <p role="alert">{error.message}</p>

  return (
    <ul aria-busy={isFetching}>
      {data.map((todo) => <li key={todo._id}>{todo.title}</li>)}
    </ul>
  )
}
```

- **`data`** is resolved through the local database, not from the fetch
  response, so a local write to any document in the set re-renders the query
  immediately. There is nothing to invalidate — the collection *is* the state.
- **`isPending`** is true only when nothing is cached at all. A warm key renders
  instantly and revalidates behind it, reporting `isFetching` while it does — a
  reload never shows a spinner over data the device already holds.
- **`error`** never clears `data`. A failed refresh leaves what you had, and
  reports `isRefetchError` rather than `isLoadingError`.

Documents arrive in the order your server returned them.

### One request per key

Components sharing a `queryKey` share the request. A list, a count badge and a
header that all read `['todos']` produce **one** fetch, not three — the second
and third join the first rather than starting their own.

The whole fetch is shared, not just the network call: hydrating the same
documents once per component would multiply writes, and on a secondary browser
tab every one of those is forwarded to the primary against a bounded buffer.

A request is abandoned only when *every* component waiting on it has gone, so
one unmounting never cancels a fetch the others still need.

### Where the documents go

`queryKey[0]` names the collection — `['todos', …]` stores into `todos`. That
name is checked against the collections you registered in step 2, so a typo is
an error rather than a new collection nobody declared.

If your keys are shaped differently, fix it once on the provider rather than at
every call site:

```tsx
<QueryProvider resolveCollection={(key) => key[2] as string}>
  {/* queryKey: ['api', 'v2', 'todos'] → the `todos` collection */}
```

Or per query, with `collection: 'todos'`.

A collection is where documents live; a key is what goes stale. They are
separate on purpose: one document belongs to many queries, each with its own
freshness.

### What `queryFn` may return

`data` is rebuilt from the collection, so the response has to reduce to
documents with a string `_id`. Three shapes are recognised:

| `queryFn` returns | `data` is |
|---|---|
| `Todo[]` | the live documents, in server order |
| `Todo` | that live document — `undefined` if it is deleted locally |
| anything else | an error naming `documents` and `assemble` |

An envelope names its own halves:

```tsx
useQuery({
  queryKey: ['todos', page],
  queryFn: () => api.get(`/todos?page=${page}`),   // { items, total }
  documents: (raw) => raw.items,
  assemble: (items, raw) => ({ ...raw, items }),
})
```

`data.items` stays live — a local edit re-renders it — while `data.total` is
whatever the server said.

### Narrowing locally with `where`

`where` filters the query's result set **in the engine**, without another
request:

```tsx
const key = ['todos']
const all    = useQuery({ queryKey: key, queryFn })
const active = useQuery({ queryKey: key, queryFn, where: { done: false } })
const done   = useQuery({ queryKey: key, queryFn, where: { done: true } })
```

One fetch, three views. `where` is deliberately **not part of the query key** —
that is the whole point. It takes the full [filter
DSL](/api/find#filter-operators): `$in`, `$gte`, `$contains`, `$and`, and the
rest.

Two things it does *not* do, both on purpose:

- **It never changes what is stored.** Every document the server sent is still
  in the collection, and still visible to other queries and to `useFind`.
- **It never changes the query record.** Membership stays exactly what the
  server returned, so a refetch cannot fight the filter — and a
  locally-excluded document is never confused with one the server deleted.

It composes with everything else: server order is preserved within the narrowed
set, an empty narrowing is `success` with `data: []` rather than `pending`, and
under `assemble` it applies to the documents before they are reassembled, so a
server-side `total` stays the server's.

The reason to reach for it over `select` is that it runs against an index rather
than a JavaScript `.filter`, and that it reacts to local writes: a document
edited out of the predicate leaves the view immediately.

::: tip Deletes you cannot see
If a refetch returns fewer documents, they leave that query's result set but are
not deleted. From the client, "no longer matches this filter" and "deleted
upstream" are indistinguishable — and one of them is not yours to act on.
:::

## Step 5 — Write

The write surface is TanStack Query v5's too: `mutate(variables)` takes the
document, and the operation is declared once on the hook — one hook per
operation, which is how React Query code is already written.

```tsx
function AddTodo() {
  const { mutate, isPending, isError, error } = useMutation<Todo>({
    collection: 'todos',
    url: '/api/v2/todos/:id',
    onSuccess: (todo) => router.push(`/todos/${todo._id}`),
  })

  return (
    <form onSubmit={(e) => { e.preventDefault(); mutate({ title, done: false }) }}>
      <button disabled={isPending}>Add</button>
      {isError && <p role="alert">{error.message}</p>}
    </form>
  )
}
```

Every `useQuery` and `useFind` watching those documents re-renders on the local
commit, long before the network hears about it. There is nothing to invalidate.

### The three operations

```tsx
useMutation<Todo>({ collection: 'todos', url: '/api/v2/todos/:id' })
  .mutate({ title: 'Buy milk' })                    // POST   /api/v2/todos

useMutation<Todo>({ collection: 'todos', url: '/api/v2/todos/:id', operation: 'update' })
  .mutate({ _id, done: true })                      // PUT    /api/v2/todos/:id

useMutation<Todo>({ collection: 'todos', url: '/api/v2/todos/:id', operation: 'delete' })
  .mutate({ _id })                                  // DELETE /api/v2/todos/:id
```

An insert mints its own `_id`; update and delete require one on the variables.
`operation` is **not** inferred from the presence of `_id` — a caller who passes
an id on an insert would silently get an update instead.

`:id` and `:collection` interpolate, and an insert drops the trailing `/:id`, so
one template covers all three verbs. `method: { update: 'PATCH' }` overrides the
default POST / PUT / DELETE.

::: tip Why a template and not a function?
Under `optimistic` and `queued` the request is sent by the drain loop — after a
route change, a reload, possibly a week later. By then the component that would
have held a closure is long gone. A template is *data*, so it is stored on the
queued document and travels with it.

This is also why [`mutationFn`](#the-three-modes) only works under
`immediate`, where the request is made from your component while it is alive.
:::

### Callbacks

`onMutate`, `onSuccess`, `onError` and `onSettled` work as they do in TanStack,
with `context` flowing from `onMutate`. Per-call callbacks compose with the
hook-level ones rather than replacing them, which is how a list row passes a
callback that closes over which row was clicked:

```tsx
mutate({ _id, done: true }, { onSuccess: () => toast('Saved') })
```

**`onSuccess` fires on the local commit** under `optimistic` and `queued`: the
write is durable and the screen has updated, but nothing has reached the server
yet. `onSynced` fires when the server confirms — and is best-effort, because the
drain may land after this component is gone, in another tab, or on a later run
of the app. Anything that must not be missed belongs in
[`useSyncStatus`](#step-6-show-the-queue).

::: warning Migrating from TanStack? Delete your rollback code
The canonical `onMutate` + `queryClient.setQueryData` + `onError` rollback block
has no analogue here and is not needed — the optimism *is* the database. It will
not fail to compile, so grep for `queryClient` before calling a migration done.
See [Migrating from TanStack Query](./react-query-migration.md).
:::

## Step 6 — Show the queue

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

::: warning `discard` is not undo
No previous version is stored. Discarding a failed **insert** deletes the
document, since the server never knew it existed. Discarding a failed **update**
or **delete** keeps the document and stops retrying — the local values stand
until the next fetch of a query containing it replaces them.
:::

## Step 7 — Declare your request parameters

Optional, and the part that pays for itself fastest. An app already has
`?category=book&page=1`; without this you write `category` twice — once for the
request, once as a local predicate — and the two drift.

`defineParams` declares each parameter once, so the request and the collection
read the same thing:

```tsx
import { defineParams, eq, gte, contains, shape } from '@taladb/react/query'

const bookParams = defineParams({
  category: eq('category'),     // ?category=book  → { category: 'book' }
  minPrice: gte('price'),       // ?minPrice=10    → { price: { $gte: 10 } }
  search:   contains('title'),  // ?search=dune    → { title: { $contains: 'dune' } }
  page:     shape(),            // ?page=1         → nothing local
  sort:     shape(),
})
```

Then at the call site:

```tsx
function Books({ category, page }: { category: string; page: number }) {
  const { data, isPending } = useQuery({
    queryKey: ['books'],
    params: bookParams({ category, page }),
    queryFn: ({ params, signal }) =>
      api.get(`/books?${params.toSearchParams()}`, { signal }),
  })

  if (isPending) return <Spinner />
  return <ul>{data.map((b) => <li key={b._id}>{b.title}</li>)}</ul>
}
```

One declaration, two consumers: **you** call `toSearchParams()`, the **library**
calls `toFilter()`. It never builds a URL — it only reads the half of the
declaration that concerns the collection.

`params` is appended to `queryKey`, so different parameters are a different
query with its own result set and its own freshness. The collection is still
taken from `queryKey[0]`.

### Start with a plain object

`params` also accepts a plain object, and that is a fine place to start:

| You write | You get |
|---|---|
| `params: { category, page }` | Query key + `toSearchParams()` in `queryFn`. Zero setup. |
| `params: bookParams({ category, page })` | The above, plus an indexed local filter. |

Same option, same call shape, one import to upgrade — so nobody has to predict
at the start whether they will need the filter.

### Nothing is inferred, on purpose

An undeclared parameter reaches the key and the request and contributes **no**
local predicate. That asymmetry is deliberate.

There is no reliable way to know a collection's field names — schemas are
Standard Schema values with no portable key enumeration, and sampling documents
fails exactly when it matters most, on a cold empty collection. So "a parameter
whose name matches a field is a predicate" would guess, and guessing wrong is
silent: `page: 1` treated as a predicate matches nothing and renders an empty
list with `status: 'success'`.

Erring the other way is safe. A local filter that is broader than the server's
shows one document too many — recoverable, and in practice invisible, because
the query's stored id list still bounds the result. A filter that is narrower
shows an empty screen over a full database.

`shape()` is how `page` and `sort` say "deliberately not a predicate" rather
than being silently dropped.

### Why translate at all?

The id list already records what the server returned, so the parameter filter is
usually redundant. Two moments where it is not, and they are the reason this
exists:

**A local write moves a document out of view.** Edit a book's category to
`stationery` while the `category=book` query is on screen and it leaves
immediately. Against the raw id list it would linger until a refetch, because
membership was decided by the server before your edit existed.

**A cold key, offline.** With no record for `{ category: 'book', page: 2 }` there
is nothing to render and no network to ask — a spinner over a database that may
well hold matching books. The parameters describe what was wanted, so there is a
real query to run locally:

```tsx
const { data, fromCollection } = useQuery({
  queryKey: ['books'],
  params: bookParams({ category, page }),
  queryFn: ({ params, signal }) => api.get(`/books?${params.toSearchParams()}`, { signal }),
  offline: 'collection',
})

return (
  <>
    {fromCollection && <Banner>Showing results saved on this device</Banner>}
    <BookList books={data} />
  </>
)
```

This changes what membership *means*, so it is opt-in and reported.
`offline: 'strict'` — the default — keeps the guarantee that `data` is exactly
what the server returned: no record, no answer.

::: warning A local query has no page 2
`shape()` parameters cannot be honoured offline. Under `'collection'` with
`page` set you will get more results than the request would have returned, and a
development warning saying so.
:::

---

## Reference — `<QueryProvider>`

| Prop | Type | Default | Description |
|---|---|---|---|
| `headers` | `() => HeadersInit \| Promise<HeadersInit>` | — | Headers for every outbound write, resolved **at send time** so a queued write carries a current token. |
| `classify` | `(response, op) => WriteOutcome` | see below | Map a response onto what the queue should do. |
| `fetch` | `typeof fetch` | global | Override the fetch implementation, for tests or instrumentation. |
| `drain` | `DrainOptions` | `{ policy: 'auto' }` | When queued writes are sent. See [drain policies](#when-writes-are-sent). |
| `resolveCollection` | `(queryKey) => string \| undefined` | — | Work out the collection for a key that does not lead with it. Returning `undefined` falls back to `queryKey[0]`. |
| `defaultOptions` | `{ queries: {…} }` | — | Defaults for every `useQuery` below this provider. |

### `defaultOptions.queries`

Accepts `staleTime`, `retry`, `retryDelay`, `refetchOnMount`,
`refetchOnWindowFocus`, `refetchOnReconnect`, `refetchInterval`,
`refetchIntervalInBackground`, `networkMode`, `forgetAfter` and `throwOnError` —
each described in the `useQuery` table below. A hook option always wins.

```tsx
<QueryProvider defaultOptions={{ queries: { staleTime: 0, retry: false } }}>
```

`staleTime: 0` here is the one-line way back to TanStack's
revalidate-on-every-mount behaviour. `retry: false` is what most test harnesses
want.

---

## Reference — `useQuery` options

### Required

| Option | Type | Description |
|---|---|---|
| `queryKey` | `readonly unknown[]` | Identifies this query. Object properties are order-insensitive, so `{ a, b }` and `{ b, a }` are one query. Must be JSON-shaped. |
| `queryFn` | `(ctx) => Promise<T[] \| T \| unknown>` | Fetches current server state. `ctx` is `{ queryKey, signal, meta }`; honour `signal`. |

### Storage and shape

| Option | Type | Default | Description |
|---|---|---|---|
| `collection` | `string` | `queryKey[0]` | Which collection fetched documents are stored in. Validated against the provider's registry. |
| `documents` | `(raw) => T[]` | — | Selects the documents out of an envelope response. |
| `assemble` | `(docs, raw) => TData` | documents alone | Rebuilds `data` from the live documents plus the stored response. |
| `select` | `(data) => TData` | — | Transforms what this component sees, without changing what is stored. Safe to write inline. |
| `where` | `Filter<T>` | — | Narrows the result set in the engine, composed with the stored id list as `$and`. Not part of the query key — one fetch can serve many narrowings. |
| `params` | `BoundParams \| Record<string, ParamValue>` | — | Request parameters. Appended to the query key, handed to `queryFn`, and translated into a local filter when declared with `defineParams`. |
| `offline` | `'strict' \| 'collection'` | `'strict'` | What to answer with when the key is cold and the fetch fails. `'collection'` queries everything cached through the `params` filter and sets `fromCollection`. |

### Freshness and fetching

| Option | Type | Default | Description |
|---|---|---|---|
| `staleTime` | `number` | `3_600_000` (1h) | How long data is considered fresh. **Not TanStack's `0`** — a cache that survives reload should not treat every mount as a reason to hit the network. |
| `enabled` | `boolean \| (() => boolean)` | `true` | `false` suppresses the *fetch*, not the cache read. A cold *and* disabled query stays `pending`. A function is re-evaluated per render — the dependent-query shape. |
| `refetchOnMount` | `boolean \| 'always'` | `true` | Revalidate on mount when stale. `'always'` ignores `staleTime`. |
| `refetchOnWindowFocus` | `boolean \| 'always'` | `true` | Revalidate when the window regains focus, if stale. |
| `refetchOnReconnect` | `boolean \| 'always'` | `true` | Revalidate when the network returns, if stale. |
| `refetchInterval` | `number \| false \| (() => number \| false)` | `false` | Poll every N ms, regardless of freshness. A function is re-evaluated per render, so polling can stop itself. |
| `refetchIntervalInBackground` | `boolean` | `false` | Keep polling while the tab is hidden. |
| `networkMode` | `'online' \| 'always' \| 'offlineFirst'` | `'online'` | `'online'` reports `fetchStatus: 'paused'` instead of failing while offline. Use `'always'` when `queryFn` talks to something local. |

### Failure

| Option | Type | Default | Description |
|---|---|---|---|
| `retry` | `boolean \| number \| (failureCount, error) => boolean` | `3` | How many times to retry. A predicate is how you stop retrying a `404`. |
| `retryDelay` | `number \| (attemptIndex, error) => number` | `min(1000·2ⁿ, 30s)` | Delay between attempts. |
| `throwOnError` | `boolean \| (error) => boolean` | `false` | Rethrow during render, for an error boundary to catch. |

While a query is retrying, `failureCount` climbs but `error` stays `null` and
`status` does not become `'error'` — a query that is still retrying has not
failed yet.

### Initial and placeholder data

| Option | Type | Default | Description |
|---|---|---|---|
| `initialData` | `TFetched \| (() => TFetched)` | — | Data to start from on a cold key. **Persisted** — hydrated into the collection exactly as a fetch would be. For SSR payloads and route loaders. |
| `initialDataUpdatedAt` | `number` | now | When `initialData` was true, so it can be born stale and revalidate immediately. |
| `placeholderData` | `TData \| ((previous) => TData \| undefined)` | — | Shown *instead of* a pending state and **never stored**. Sets `isPlaceholderData`. |

`keepPreviousData` is exported for the common case:

```tsx
import { keepPreviousData } from '@taladb/react/query'

useQuery({ queryKey: ['todos', page], queryFn, placeholderData: keepPreviousData })
```

### Lifetime and identity

| Option | Type | Default | Description |
|---|---|---|---|
| `forgetAfter` | `number` | `Infinity` | How long to remember this query's **result set** after nothing is using it. Not garbage collection — see below. |
| `queryKeyHashFn` | `(queryKey) => string` | built-in | Serialises a key into the record's identity. Replace only if the default cannot express your keys. |
| `meta` | `Record<string, unknown>` | — | Opaque; passed through to `queryFn`. |

#### `forgetAfter` is not garbage collection

**Documents are never deleted by this layer.** They live in your collections,
which your app also reads directly — sweeping them would destroy real user data
rather than reclaim a cache. What expires is the *record of which documents a
query returned*: a few hundred bytes. When it goes, the next mount is cold and
refetches to re-establish membership.

The default is `Infinity`, because that record is exactly what makes a warm
start warm. Reach for `forgetAfter` when a result set is genuinely disposable —
a search whose terms will never be typed again.

TanStack's `gcTime` is **not** accepted — it is a compile error rather than a
silent no-op. Nothing here is collected by age, and revalidation is already
`staleTime`'s job.

### Accepted and ignored

`notifyOnChangeProps` and `structuralSharing` compile and do nothing —
structural sharing is always on here, and `notifyOnChangeProps` is a render
optimisation with no effect on correctness. Each logs once in development.

---

## Reference — `useQuery` result

The result is a discriminated union, so `if (isPending)` and `if (isError)`
narrow `data` for you.

| Field | Type | Description |
|---|---|---|
| `data` | `TData \| undefined` | Live documents, resolved from the collection. `undefined` until the first result — never `[]`. |
| `error` | `Error \| null` | Typed `Error`, not `unknown`. Non-`Error` throws are wrapped. |
| `status` | `'pending' \| 'success' \| 'error'` | Whether there is data. |
| `fetchStatus` | `'fetching' \| 'paused' \| 'idle'` | Whether a request is happening. |
| `isPending` | `boolean` | `status === 'pending'`. |
| `isSuccess` / `isError` | `boolean` | Mirrors `status`. |
| `isLoading` | `boolean` | `isPending && isFetching` — a first load with nothing to show. |
| `isFetching` | `boolean` | A request is in flight. |
| `isRefetching` | `boolean` | Fetching over data already on screen. |
| `isPaused` | `boolean` | Offline, so the fetch has not been attempted. |
| `isStale` | `boolean` | Past `staleTime`. **Not** "currently revalidating" — that is `isFetching`. |
| `isFetched` / `isFetchedAfterMount` | `boolean` | Whether a fetch has completed, ever / since mount. |
| `isPlaceholderData` | `boolean` | `data` is a stand-in. |
| `fromCollection` | `boolean` | `data` came from the whole collection rather than this query's result set — see `offline: 'collection'`. |
| `isLoadingError` | `boolean` | Failed with nothing cached — the blank-screen case. |
| `isRefetchError` | `boolean` | Failed over data that is still on screen. |
| `dataUpdatedAt` | `number` | Epoch ms of the last successful fetch. |
| `errorUpdatedAt` | `number` | Epoch ms of the last failure. |
| `failureCount` | `number` | Consecutive failed attempts, reset on success. |
| `failureReason` | `Error \| null` | The most recent failure, including during retries. |
| `errorUpdateCount` | `number` | Total failures over the query's life. |
| `refetch` | `(opts?) => Promise<QueryResult>` | Fetch now. Resolves once the fetched documents are on screen, so `const { data } = await refetch()` sees them. `opts.cancelRefetch` defaults to `true` — abandon any request already on its way; `false` joins it instead. |

---

## Reference — `useMutation`

| Option | Type | Default | Description |
|---|---|---|---|
| `collection` | `string` | *required* | Which collection the write applies to. |
| `url` | `string` | provider's backend | URL template — `/api/v2/todos/:id`. |
| `method` | `Partial<Record<SyncOp, string>>` | `POST`/`PUT`/`DELETE` | Verb overrides per operation. |
| `mode` | `'optimistic' \| 'immediate' \| 'queued'` | `'optimistic'` | When the network happens. |

| Option | Type | Default | Description |
|---|---|---|---|
| `retry` | `boolean \| number \| fn` | `0` | **Not** the queries' `3`. A read is idempotent; a write may not be. `immediate` only. |
| `retryDelay` | `number \| fn` | exponential | As `useQuery`. |
| `networkMode` | `'online' \| 'always' \| 'offlineFirst'` | `'online'` | Under `immediate`, `'online'` **pauses** while offline and goes on reconnect. |
| `throwOnError` | `boolean \| fn` | `false` | Rethrow during render, for an error boundary. |
| `scope` | `{ id: string }` | — | Run mutations sharing an id one at a time. Prevents two rapid submits racing. |
| `mutationFn` | `(variables) => Promise<T>` | — | **`immediate` only.** Owns the request; `url`/`method`/`classify` do not apply. |

| Result field | Type | Description |
|---|---|---|
| `mutate` | `(variables, callbacks?) => void` | Fire-and-forget. Errors surface on `error`. |
| `mutateAsync` | `(variables, callbacks?) => Promise<T>` | Awaitable. Under `optimistic`, resolves on the *local* commit. |
| `data` | `T \| undefined` | The stored document. |
| `status` | `'idle' \| 'pending' \| 'success' \| 'error'` | `idle` has no `useQuery` equivalent. |
| `isIdle` / `isPending` / `isSuccess` / `isError` | `boolean` | `isPending` stays `false` under `optimistic` — that is the point. |
| `isPaused` | `boolean` | Offline, waiting for the connection. |
| `variables` | `TVariables \| undefined` | What the last `mutate` was called with. |
| `reset` | `() => void` | Back to `idle`. |
| `submittedAt` | `number` | Epoch ms of the last `mutate`. |
| `failureCount` / `failureReason` | | Reset on success. |
| `error` | `Error \| null` | Typed `Error`, so `error.message` compiles. |

### Callbacks

`onMutate`, `onSuccess`, `onError` and `onSettled` behave as in TanStack, with
`context` flowing from `onMutate`. Per-call callbacks compose with the
hook-level ones rather than replacing them:

```tsx
mutate({ _id, done: true }, { onSuccess: () => toast('Saved') })
```

**`onSuccess` fires on the local commit** under `optimistic` and `queued` — the
write is durable and the UI has updated, but nothing has reached the server.
`onSynced` fires when the server confirms, and is best-effort: the drain may
land after this component is gone. Anything that must not be missed belongs in
[`useSyncStatus`](#reference-usesyncstatus).

### The three modes

| `mode` | What happens |
|---|---|
| Mode | UI updates | Network happens | `isPending` |
|---|---|---|---|
| `optimistic` *(default)* | instantly, on the local commit | at once, in the background | always `false` |
| `queued` | **instantly, on the local commit** | on the provider's schedule | always `false` |
| `immediate` | after the server agrees | first | reflects the request |

`queued` does **not** make the user wait. Like `optimistic` it commits locally
and re-renders every query over those documents; the only thing it changes is
when the request leaves. It is a request-count knob for high-volume writes —
reading progress on every page turn, cursor positions, telemetry — not a
latency one.

Use **`immediate`** where showing success before the server confirms would
mislead: checkout, payment, anything irreversible. Nothing is stored until it
succeeds, so there is nothing to roll back if it fails. This is also the one
mode that accepts `mutationFn`, because the request is made from your component
while it is still alive:

```tsx
const { mutateAsync, isPending } = useMutation<Order>({
  collection: 'orders',
  mode: 'immediate',
  mutationFn: async (cart) => {
    const response = await fetch('/api/orders', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(cart),
    })
    if (!response.ok) throw new Error(`Checkout failed (${response.status})`)
    return response.json()
  },
})

await mutateAsync(cart)  // throws if the server says no
```

Given a `mutationFn`, `url`, `method` and the provider's `classify` do not
apply — the function owns the request, and whatever it resolves to is stored.
Passing one in any other mode throws on the first render, because there would be
no closure left for the drain to call.

Or without one, routing by template as the other modes do:

```tsx
const { mutateAsync, isPending } = useMutation<Order>({
  collection: 'orders',
  url: '/api/orders/:id',
  mode: 'immediate',
})
```

::: warning Writes are addressed by id
`operation: 'update'` and `'delete'` need an `_id` on the variables, never a
filter. On a secondary browser tab a filter-based write is forwarded to the
primary and re-evaluated there against authoritative data — so `{ done: false }`
could affect a different set of documents than the one your UI just showed.
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
the difference between it and `optimistic`. **It does not make the UI wait:**
like `optimistic`, it commits locally first and every query over those documents
re-renders immediately. The only thing it changes is when the request leaves.

### The timer is the fallback, not the mechanism

The moment a queue most needs draining is the moment the interval is least
likely to be about to fire, so four other things trigger a cycle:

| Trigger | Option | Why |
|---|---|---|
| Tab hidden | `flushOnHide` *(default on)* | The user switched away or backgrounded the app. The page is still alive, so this is a normal drain. |
| Tab closing | `flushOnHide` | A last-ditch `pagehide` flush — see below. |
| Reconnect | always on except `manual` | A backlog built offline is exactly what is waiting. |
| Queue depth | `flushAt` *(default 100)* | Stop accumulating. Without it a burst sits unsent for the whole interval and the queue grows without bound between ticks. |

`batch` caps how many documents one cycle sends.

::: warning The `pagehide` flush is best-effort by construction
An ordinary `fetch` is cancelled the moment the document goes away, so the
closing flush is sent with `keepalive: true`. Browsers cap keepalive bodies at
**64 KB across all in-flight requests**, so a large queue will not fit. It is
genuinely a last chance, not a guarantee — which is fine, because the writes are
already durable and the next session drains them. The flush on *hidden* is the
one that does the real work.
:::

### Choosing an interval

A longer interval does not risk losing writes — they are committed locally and
survive reload, crash and flight. What it costs is **staleness on other
devices** and **delay before a terminal failure surfaces**. Five minutes is fine
for reading progress or telemetry, and clearly wrong for a checkbox someone else
is watching, which is why the default is 30 seconds.

That second cost is worth dwelling on: the longer the interval, the longer
between a user acting and a rejection appearing. [`useSyncStatus`](#reference-usesyncstatus)
is what catches it, and it matters *more* as the interval grows, not less.

Write retries back off at 1s, doubling to a five-minute ceiling, with ±25%
jitter so a fleet of clients does not reconnect in lockstep. `terminal` outcomes
are never retried automatically; they need a decision.

---

## Reference — `useSyncStatus`

| Field | Type | Description |
|---|---|---|
| `pending` | `number` | Writes queued and waiting to send. |
| `failed` | `FailedWrite[]` | Terminal failures needing a human decision. Each carries `collection`, `id`, `op`, `error`, `attempts`. |
| `draining` | `boolean` | A drain cycle is running. |
| `online` | `boolean` | Browser connectivity. |
| `retry` | `(id: string) => void` | Requeue a failed write. |
| `discard` | `(id: string) => void` | Drop the local change and stop trying. |

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

By default `2xx` succeeds, `409` is already-applied, `5xx`/`408`/`429` retry,
and any other `4xx` is terminal. Override with `classify` only if your API
disagrees:

```tsx
<QueryProvider classify={(res) => (res.status === 418 ? 'terminal' : undefined)}>
```

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

- [Migrating from TanStack Query](./react-query-migration.md) — the short list of differences
- [React Hooks](/guide/react) — the local-only hooks this builds on
- [Full-Text & Hybrid Search](/api/search) — run it over what you fetched
- [Multi-tab behaviour](/guide/web#multi-tab-behaviour) — how the primary is chosen
