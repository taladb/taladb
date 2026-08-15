---
title: React Guide — Hooks
description: First-party React hooks for TalaDB. useFind, useFindOne, and useCollection give you live-updating queries with zero-tearing snapshots in concurrent React. Works in React and React Native.
---

# React Hooks (`@taladb/react`)

`@taladb/react` is the official hooks package for TalaDB. It wraps TalaDB's live query API with `useSyncExternalStore` so your components automatically re-render whenever the underlying data changes — no manual subscriptions, no boilerplate.

Works in **React (browser + Node.js)** and **React Native** with the same API.

## Installation

::: code-group

```bash [Browser]
pnpm add taladb @taladb/web @taladb/react
```

```bash [React Native]
pnpm add taladb @taladb/react-native @taladb/react
```

:::

**Requirements:** React 18+ · `taladb` 0.4+

---

## Setup

Two ways to provide the database. **Name form** (since v0.9.0) — the provider owns the `openDB` lifecycle: it opens lazily on the client, renders `fallback` until ready (so hooks never see a missing db), and closes on unmount. The right default for browser apps and required for Next.js, where `openDB` cannot run during server rendering:

```tsx
import { TalaDBProvider } from '@taladb/react'

root.render(
  <TalaDBProvider name="myapp.db" fallback={<Splash />}>
    <App />
  </TalaDBProvider>
)
```

**Instance form** — you open the database yourself and hand it in (plain React with top-level await, React Native):

```tsx
import { openDB } from 'taladb'
import { TalaDBProvider } from '@taladb/react'

const db = await openDB('myapp.db')

root.render(
  <TalaDBProvider db={db}>
    <App />
  </TalaDBProvider>
)
```

> **React Native**: call `TalaDBModule.initialize('myapp.db')` before `openDB`. See the [React Native guide](/guide/react-native) for setup details.
>
> **Next.js**: the package ships the `'use client'` directive, so importing hooks never trips the RSC boundary.

### Registering collection options

Hooks resolve their collection by name, so they need to know how that collection is configured. Register each collection's options — `schema`, `syncSchema`, `migrateDocument` — once on the provider:

```tsx
<TalaDBProvider
  name="myapp.db"
  fallback={<Splash />}
  collections={{
    bookings: { schema: Booking, syncSchema: { version: 1, required: ['listingId'] } },
    reviews: { schema: Review, syncSchema: { version: 1 } },
  }}
>
  <App />
</TalaDBProvider>
```

Every hook below it — `useCollection`, `useFind`, `useFindOne`, `useAggregate`, `useMutation` — now opens a **configured** collection: a local write hard-fails validation, and the engine stamps the document's `_v` shape version. `useCollection(name, options)` overrides the registry for a single call.

::: warning Changed in 0.9.3
Before 0.9.3 there was no way to give the hooks these options: `useCollection(name)` called `db.collection(name)` bare, so a write through `useMutation` **silently skipped** the strict `schema` and the `_v` stamp that `db.collection(name, { … })` applies. An app that followed this guide lost local validation without any warning. Register `collections` and that gap is closed.
:::

---

## Quick example

```tsx
import { useCollection, useFind } from '@taladb/react'

interface Note {
  _id?: string
  text: string
  pinned: boolean
}

export function NoteList() {
  const notes = useCollection<Note>('notes')
  const { data, loading } = useFind(notes, { pinned: true })

  if (loading) return <p>Loading…</p>

  return (
    <ul>
      {data.map((note) => (
        <li key={note._id}>{note.text}</li>
      ))}
    </ul>
  )
}
```

The component re-renders automatically whenever a pinned note is inserted, updated, or deleted — no `useEffect`, no manual `subscribe` calls.

---

## Hooks

### `useCollection`

Returns a stable, memoised `Collection<T>` from the nearest `<TalaDBProvider>`. Use this as the collection argument to `useFind` and `useFindOne`.

```ts
const collection = useCollection<T>(name: string): Collection<T>
```

The same `Collection` object is returned on every render (memoised by `db` identity + name), so you can pass it directly to `useFind` without wrapping it in `useMemo`.

```tsx
const articles = useCollection<Article>('articles')
```

---

### `useFind`

Subscribes to a live query and returns all matching documents. Re-renders whenever the result set changes.

```ts
const { data, loading } = useFind<T>(
  collection: Collection<T>,
  filter?: Filter<T>,
): { data: T[]; loading: boolean }
```

| | |
|---|---|
| `data` | Array of matching documents. Empty array while `loading` is `true`. |
| `loading` | `true` until the first snapshot is delivered from the database. |

**Inline filter objects are safe** — the filter is serialised to a string internally, so `{ active: true }` written directly in JSX does not cause a re-subscription on every render.

```tsx
// All documents
const { data: all } = useFind(users)

// With filter
const { data: active } = useFind(users, { active: true })

// With comparison operators
const { data: recent } = useFind(articles, {
  publishedAt: { $gte: Date.now() - 86_400_000 },
})
```

See [Filters](/api/filters) for the full filter DSL.

---

### `useFindOne`

Subscribes to a single document. Returns the first match, or `null` if nothing matches.

```ts
const { data, loading } = useFindOne<T>(
  collection: Collection<T>,
  filter: Filter<T>,
): { data: T | null; loading: boolean }
```

```tsx
interface User {
  _id?: string
  name: string
  role: 'admin' | 'member'
}

const users = useCollection<User>('users')
const { data: user, loading } = useFindOne(users, { _id: userId })

if (loading) return <Spinner />
if (!user)   return <p>User not found.</p>

return <p>Hello, {user.name}</p>
```

---

### `useTalaDB`

Returns the raw `TalaDB` instance from context. Use this when you need direct access to `db.collection()` for write operations outside of `useCollection`.

```ts
const db = useTalaDB(): TalaDB
```

```tsx
function AddNoteButton() {
  const db = useTalaDB()

  async function handleClick() {
    await db.collection('notes').insert({ text: 'New note', pinned: false })
    // useFind subscribers update automatically — no setState needed
  }

  return <button onClick={handleClick}>Add Note</button>
}
```

---

## Full example

A complete notes app with live queries, inserts, and deletes:

```tsx
import { useCollection, useFind, useTalaDB } from '@taladb/react'

interface Note {
  _id?: string
  text: string
  createdAt: number
}

export function NotesApp() {
  const db = useTalaDB()
  const notes = useCollection<Note>('notes')
  const { data, loading } = useFind(notes)

  async function addNote() {
    await notes.insert({ text: `Note ${Date.now()}`, createdAt: Date.now() })
  }

  async function deleteNote(id: string) {
    await notes.deleteOne({ _id: id })
  }

  return (
    <div>
      <button onClick={addNote}>Add Note</button>
      {loading && <p>Loading…</p>}
      <ul>
        {data.map((note) => (
          <li key={note._id}>
            {note.text}
            <button onClick={() => deleteNote(note._id!)}>Delete</button>
          </li>
        ))}
      </ul>
    </div>
  )
}
```

---

## React Native

`@taladb/react` works in React Native without any changes. Install `@taladb/react-native` as the platform adapter and the same hooks work on iOS and Android:

```tsx
// App.tsx
import { TalaDBModule } from '@taladb/react-native'
import { openDB } from 'taladb'
import { TalaDBProvider } from '@taladb/react'

export default function App() {
  const [db, setDb] = useState<TalaDB | null>(null)

  useEffect(() => {
    async function init() {
      await TalaDBModule.initialize('myapp.db')
      setDb(await openDB('myapp.db'))
    }
    init()
  }, [])

  if (!db) return null

  return (
    <TalaDBProvider db={db}>
      <NoteList />
    </TalaDBProvider>
  )
}
```

Everything else — `useCollection`, `useFind`, `useFindOne` — is identical to the browser.

---

## How live queries work

Under the hood, `useFind` and `useFindOne` call `collection.subscribe(filter, callback)` and bridge it to React with `useSyncExternalStore`. This gives you:

- **Zero-tearing** — all components reading from the same collection see the same snapshot within a single render pass (concurrent React guarantee)
- **Automatic cleanup** — the subscription is cancelled when the component unmounts
- **Filter stability** — the filter is serialised to JSON for subscription identity, so inline objects like `{ active: true }` don't trigger re-subscription on every render

The subscription polls for changes every 300 ms on all platforms, with an additional `BroadcastChannel` nudge in the browser so cross-tab writes propagate immediately.

---

## TypeScript tips

Pass your document type as a generic to get fully typed `data`:

```ts
interface Article {
  _id?: string
  title: string
  locale: string
  publishedAt: number
}

const articles = useCollection<Article>('articles')
const { data } = useFind(articles, { locale: 'en' })
//      ^? Article[]
```

Filter and update types are inferred from the document type — typos on field names are caught at compile time.
