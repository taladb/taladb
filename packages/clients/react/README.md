# @taladb/react

React and React Native hooks for TalaDB — live queries that re-render your components when the local database changes.

[![npm](https://img.shields.io/npm/v/@taladb/react)](https://www.npmjs.com/package/@taladb/react)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-orange.svg)](https://github.com/taladb/taladb/blob/main/LICENSE)

## What this gives you

TalaDB collections are already reactive — `col.subscribe(filter, cb)` pushes a fresh snapshot on every matching change. These hooks wire that into React's rendering model for you: subscription lifecycle, snapshot identity, and cleanup, so a write in one component re-renders every other component reading the same data.

Reads are backed by `useSyncExternalStore`, so snapshots never tear under concurrent rendering.

```tsx
import { TalaDBProvider, useCollection, useFind, useWrite } from '@taladb/react'

function App() {
  return (
    <TalaDBProvider name="myapp.db" fallback={<Spinner />}>
      <Articles />
    </TalaDBProvider>
  )
}

function Articles() {
  const articles = useCollection<Article>('articles')
  const { data, loading } = useFind(articles, { published: true })
  const { write, pending } = useWrite<Article>({ collection: 'articles' })

  if (loading) return <Spinner />

  return (
    <>
      {data.map((a) => (
        <h2 key={a._id}>{a.title}</h2>
      ))}
      <button
        disabled={pending}
        onClick={() => write({ type: 'insert', doc: { title: 'New', published: false } })}
      >
        Add
      </button>
    </>
  )
}
```

There is no cache to invalidate and no refetch to trigger. The write commits locally and the list above re-renders.

## Installation

```bash
pnpm add @taladb/react taladb
```

Plus the platform backend TalaDB needs — `@taladb/web` in the browser, `@taladb/react-native` on device, `@taladb/node` on the server. See the [taladb](https://www.npmjs.com/package/taladb) package.

**Peer dependencies:** `react >= 18`, `taladb ^0.11.0`.

## Provider

`TalaDBProvider` takes one of two shapes.

**Provider-owned** — pass a `name` and it owns the `openDB` lifecycle: opens lazily on the client, never during SSR, and closes on unmount. This is the right form for Next.js.

```tsx
<TalaDBProvider name="myapp.db" options={{ /* OpenDBOptions */ }} fallback={<Spinner />}>
```

Children render only once the database is ready, so `useTalaDB()` never observes a missing instance. `fallback` renders until then (and during SSR); it defaults to `null`.

**Caller-owned** — pass a `db` you opened yourself, and you keep the lifecycle.

```tsx
const db = await openDB('myapp.db')
<TalaDBProvider db={db}>
```

### Registering collection options

Per-collection options (`schema`, `syncSchema`, `migrateDocument`, …) belong on the provider, keyed by collection name:

```tsx
<TalaDBProvider
  name="myapp.db"
  collections={{
    articles: { schema: ArticleSchema, syncSchema: { version: 1 } },
  }}
>
```

This matters: without the registry, `useCollection` resolves a bare `db.collection(name)` handle, and writes through `useWrite` silently skip `schema` validation and the `_v` shape stamp. The registry is read when a handle is first created and treated as static, so an inline object here won't thrash live queries.

## Hooks

### `useCollection<T>(name, options?)`

Resolves a collection from the provider's database, applying the registered options for `name`. The handle is memoized — same reference every render unless the db or name changes — so pass it straight to `useFind` without wrapping in `useMemo`. A per-call `options` argument overrides the registry entry.

```ts
const articles = useCollection<Article>('articles')
```

### `useFind<T>(collection, filter?)`

Subscribes to a live query.

```ts
const { data, loading, error } = useFind(articles, { locale: 'en' })
```

| Field | Type | |
|---|---|---|
| `data` | `T[]` | Matching documents. Empty while loading. |
| `loading` | `boolean` | True until the first snapshot arrives. |
| `error` | `unknown \| null` | Last subscription error, cleared by the next good snapshot. |

Inline filter objects are safe — the filter is serialized for subscription identity, so `{ active: true }` on every render does not re-subscribe.

### `useFindOne<T>(collection, filter)`

Same, for a single document. `data` is `T | null` — `null` both while loading and when nothing matched.

```ts
const { data: article, loading } = useFindOne(articles, { slug })
```

### `useAggregate<T, R>(collection, pipeline)`

A live aggregation, and **the paging primitive** — `find()` has no sort, skip, or limit, so reach for this when you need them.

```ts
const { data, loading } = useAggregate<Article>(articles, [
  { $match: { published: true } },
  { $sort: { createdAt: -1 } },
  { $skip: page * 20 },
  { $limit: 20 },
])
```

Inline pipeline arrays are serialized for identity, the same as filters.

### `useWrite<T>({ collection })`

Writes to a collection. The write is local, immediate, and durable — every `useFind` / `useFindOne` / `useAggregate` on that collection re-renders once it commits.

```ts
const { write, writeAsync, pending, error } = useWrite<Article>({ collection: 'articles' })

write({ type: 'insert', doc: { title: 'Hello', published: false } })
write({ type: 'update', where: { _id: id }, set: { published: true } })
write({ type: 'delete', where: { _id: id } })

await writeAsync({ type: 'insert', doc })  // rejects on error
```

`write` is fire-and-forget — failures land on `error` rather than being thrown into render. `writeAsync` resolves once the write commits and rejects on failure. `pending` is true while a write is in flight.

It's named `useWrite`, not `useMutation`, because that's all it is: a local write with no network step and no rollback to reason about. The database is on the device — the write either committed or threw.

### `useTalaDB()`

The `TalaDB` instance from the nearest provider, for anything the hooks above don't cover.

## Server rendering

`openDB` cannot run during server rendering. Use the provider-owned form (`name=`), which opens only on the client and renders `fallback` until the handle exists. Hooks below it never see a null database.

## `@taladb/react/query`

A subpath export for a local-first data layer over TalaDB — cache-as-a-real-collection, background revalidation, writes that drain to the network without blocking the UI.

> **Status: in progress.** The type surface, `defineBackend`, and the envelope and query-record layer are in place. The hooks are not implemented yet — don't build on it.

## Documentation

**[https://taladb.dev](https://taladb.dev)**

- [React Guide](https://taladb.dev/guide/react)
- [React Native Guide](https://taladb.dev/guide/react-native)
- [Live Queries](https://taladb.dev/api/live-queries)
- [Schema Validation](https://taladb.dev/api/schema)
- [Filters](https://taladb.dev/api/filters) · [Aggregation](https://taladb.dev/api/aggregation)

## License

Apache 2.0 © [TalaDB](https://github.com/taladb)
