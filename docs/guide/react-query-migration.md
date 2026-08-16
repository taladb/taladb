---
title: Migrating from TanStack Query
description: What changes when you move a React Query codebase onto @taladb/react/query, and what does not.
---

# Migrating from TanStack Query

`@taladb/react/query` speaks TanStack Query v5's `useQuery` surface on purpose,
so moving a component across is usually one line:

```diff
- import { useQuery } from '@tanstack/react-query'
+ import { useQuery } from '@taladb/react/query'
```

`queryKey`, `queryFn`, `staleTime`, `select`, `placeholderData`, `retry`,
`status`, `isPending`, `isFetching`, `error.message` — all of it works as you
already know it. What follows is only the parts that differ.

Three real components are kept in the repository as an executable version of
this claim: `packages/clients/react/tests/parity/` renders TanStack-idiom
components against this library with nothing changed but their import.

## Two things are not one line

### 1. The provider

There is no `QueryClient` and no `QueryClientProvider`.

```diff
- <QueryClientProvider client={queryClient}>
-   <App />
- </QueryClientProvider>
+ <TalaDBProvider name="app.db" collections={{ todos: { schema: Todo } }}>
+   <QueryProvider>
+     <App />
+   </QueryProvider>
+ </TalaDBProvider>
```

This is deliberate rather than unfinished. TanStack's client *owns the cache*;
here the database owns it, and an object whose `client.clear()` either lied or
deleted your documents would be worse than no object at all.

Whatever you passed to `new QueryClient({ defaultOptions })` moves across
unchanged:

```tsx
<QueryProvider defaultOptions={{ queries: { staleTime: 0, retry: false } }}>
```

### 2. `queryFn` must return documents

This is the one that cannot be papered over. TanStack's `data` *is* whatever
`queryFn` returned. Here `data` is resolved from your local collection — that is
what makes it live, offline-readable, and searchable — so the response has to
reduce to documents with an `_id`.

**`_id` must be a ULID**, and your server's id almost certainly is not one. Ids
are 16 raw bytes on disk and every index key ends with one, so `"265"`, a slug,
or a UUID is rejected outright. Fold your natural key into a ULID in `queryFn`:

```ts
import { deriveDocId } from 'taladb'

useQuery({
  queryKey: ['books', shelf],
  queryFn: async () => {
    const rows = await api.get(`/books?shelf=${shelf}`)
    return rows.map((row) => ({ ...row, _id: deriveDocId('books', String(row.id)) }))
  },
})
```

`deriveDocId` is a hash, so the same `(collection, key)` always maps to the same
id. That is what makes hydration idempotent: refetching a page updates the rows
it already stored instead of inserting a second copy. Keep passing your original
`id` through as an ordinary field — nothing stops you reading or filtering on it.

Use `isDocId(value)` if you need to check one yourself. Note the ordering
caveat: a derived id's ULID timestamp prefix is a hash, not a clock, so reads
over a hydrated collection need an explicit `$sort`.

Three shapes are recognised without configuration:

| `queryFn` returns | `data` is |
|---|---|
| `Todo[]` | the live documents, in server order |
| `Todo` | that live document (`undefined` if deleted locally) |
| anything else | an error telling you to add `documents` / `assemble` |

For an envelope, name the two halves:

```ts
useQuery({
  queryKey: ['todos', page],
  queryFn: () => api.get(`/todos?page=${page}`),   // { items, total }
  documents: (raw) => raw.items,
  assemble: (items, raw) => ({ ...raw, items }),
})
```

`assemble` is what keeps liveness: `data.items` re-renders when a document
changes locally, while `data.total` stays whatever the server said.

## Defaults that differ

### `staleTime` is one hour, not zero

TanStack defaults to `0` — revalidate on every mount. That is right for a cache
that dies on reload: the data is seconds old and the request is nearly free. It
is wrong for a cache that survives reload and is expected to work offline, where
the same setting turns every mount into a network round trip against data the
device already holds.

**This is the only difference that is invisible at compile time.** Everything
else either compiles or throws; this one quietly changes how often your app
talks to your server — usually for the better, but not if you were relying on
refetch-on-mount for freshness.

The one-line undo:

```tsx
<QueryProvider defaultOptions={{ queries: { staleTime: 0 } }}>
```

`refetchOnMount`, `refetchOnWindowFocus` and `refetchOnReconnect` all still
default to `true` and still mean "refetch *if stale*" — the one-hour window
simply means they fire far less often. `'always'` bypasses it as usual.

### `collection` is inferred, and checked

`useQuery` needs to know which collection a response is stored in — TanStack has
no equivalent, because it has nowhere to store anything. It is taken from
`queryKey[0]`, so `queryKey: ['todos', …]` needs no edit.

It is then **checked against the collections registered on `TalaDBProvider`**,
and a name that is not registered is an error rather than a new collection. This
option decides where documents physically land; a wrong guess would pour server
data into a collection your app reads for something else.

If your keys do not lead with the collection, fix it once:

```tsx
<QueryProvider resolveCollection={(key) => key[2] as string}>
```

## Options accepted but not acted on

A drop-in that throws on an unknown option is not a drop-in, so these compile
and are ignored. Each one says so once, in development.

| Option | What happens |
|---|---|
| `notifyOnChangeProps` | Ignored. A render optimisation with no effect on correctness. |
| `structuralSharing` | Always on; cannot be disabled. A live query produces snapshots far more often than a polling cache, so a new `data` reference each time would be worse here, not merely different. |

## `gcTime` is not accepted

TanStack's `gcTime` drops cached data from memory once no component is using it.
There is no equivalent here and it is not accepted, so `gcTime:` is a compile
error rather than a silent no-op. Delete the line.

**Nothing here is collected by age.** Documents live in your own collections,
which your app also reads directly — sweeping them would destroy real user data
rather than reclaim a cache. And freshness is already `staleTime`'s job: a stale
query refetches, which is the behaviour `gcTime` was standing in for.

If you genuinely need to bound how long a *result set* is remembered — a search
whose terms will never be typed again — `forgetAfter` does that, and only that:

```ts
useQuery({ queryKey: ['search', term], queryFn, forgetAfter: 5 * 60_000 })
```

It drops the record of which documents the query returned, never the documents.
The default is `Infinity`, because that record is what makes a warm start warm.

## Smaller differences

- **`error` is typed `Error`**, not `unknown`, so `error.message` compiles.
  Non-`Error` throws are wrapped.
- **`refetch()` resolves with the result**, so `const { data } = await refetch()`
  works.
- **`placeholderData` is applied after `select`**, where TanStack applies it
  before. This is what lets `placeholderData: keepPreviousData` hand back the
  previous `data` untouched. The difference is only observable for a placeholder
  that is not the previous value.
- **A disabled query still reads the cache.** `enabled: false` suppresses the
  fetch, not the local read — that is the difference between a persistent cache
  and a memory one. A cold *and* disabled query stays `pending` with
  `data: undefined`, as TanStack does.
- **Your model types need no changes.** `interface Todo { _id: string }` works
  as-is; there is no `extends Document` requirement on the public API. The type
  is just `string` — that a stored `_id` must be a *ULID* is a runtime rule, not
  one the compiler can check for you, which is why it is called out above.

## Mutations

`useMutation` speaks the same surface: `mutate(variables)`, `mutateAsync`,
`onMutate`/`onSuccess`/`onError`/`onSettled` with `context`, per-call callbacks,
and `status`/`data`/`variables`/`reset`/`isPending`. Two things differ.

### `mutationFn` only works in `immediate` mode

Under the default `optimistic` mode — and under `queued` — the request is sent
by the drain loop, long after your component is gone: after a route change, a
reload, a week. **A closure cannot be stored**, so there would be nothing left
to call. Accepting one would mean silently dropping the write on reload.

So those modes route by template, which is data and survives:

```diff
- useMutation({ mutationFn: (todo) => api.post('/todos', todo) })
+ useMutation({ collection: 'todos', url: '/api/todos/:id' })
```

The operation is declared once rather than passed per call, which matches how
React Query code is already written — one `useCreateTodo`, one `useUpdateTodo`:

```ts
useMutation({ collection: 'todos', url: '/api/todos/:id' })                       // insert
useMutation({ collection: 'todos', url: '/api/todos/:id', operation: 'update' })  // update
useMutation({ collection: 'todos', url: '/api/todos/:id', operation: 'delete' })  // delete
```

`mutationFn` is still exactly right for a write you must not show as succeeded
before the server agrees — a checkout, a payment — which is `mode: 'immediate'`:
a plain request from your component, with no queue behind it and nothing stored
until it succeeds.

Passing `mutationFn` in any other mode throws on the first render, naming both
ways out.

### Delete your rollback code

This is the canonical TanStack optimistic mutation:

```ts
onMutate: async (next) => {
  await queryClient.cancelQueries({ queryKey: ['todos'] })
  const previous = queryClient.getQueryData(['todos'])
  queryClient.setQueryData(['todos'], (old) => [...old, next])
  return { previous }
},
onError: (err, next, ctx) => queryClient.setQueryData(['todos'], ctx.previous),
onSettled: () => queryClient.invalidateQueries({ queryKey: ['todos'] }),
```

**All of it goes.** The optimism *is* the database: the write is committed
locally before the request is made, every `useQuery` over those documents
re-renders from that commit, and there is nothing to invalidate. Migrating this
is a deletion, not a port — but it will not fail to compile, so it is worth
grepping for `queryClient` before you call the migration done.

`onMutate` itself is kept as a plain hook point for logging and deriving
`context`.

### When `onSuccess` fires

Under `optimistic` and `queued` it fires on the **local commit** — the write is
durable and the UI has updated, but nothing has reached the server. Under
`immediate` it fires on the server's response.

Waiting for the server in every mode would be more faithful and much worse: the
drain often lands after the component is gone, so the callback would silently
never fire. `onSynced` is there for server confirmation, and is best-effort by
nature. Anything that must not be missed — a write that failed and needs a human
decision — belongs in `useSyncStatus`, which survives navigation.

## Not implemented yet

`useInfiniteQuery`, `useQueries`, `useSuspenseQuery`, `queryOptions()`,
`invalidateQueries` and `setQueryData` are not here yet. `useMutationState` is
not planned — `useSyncStatus` answers the question it exists for.
