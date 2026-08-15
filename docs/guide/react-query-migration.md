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
reduce to documents with a string `_id`.

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
  as-is; there is no `extends Document` requirement on the public API.

## Not implemented yet

`useMutation` exists but has a different shape from TanStack's — see the
[local-first data guide](./query.md). `useInfiniteQuery`, `useQueries`,
`useSuspenseQuery`, `queryOptions()`, `invalidateQueries` and `setQueryData` are
not here yet.
