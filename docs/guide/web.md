---
title: Web Guide — Browser / WASM
description: Use TalaDB in the browser with WebAssembly and OPFS persistent storage. Works with Vite, Next.js, and any modern bundler.
---

# Web (Browser / WASM)

Run a full vector database in the browser tab. Pair TalaDB with an on-device
embedding model (transformers.js, ONNX Runtime Web) and you have
[semantic search](/api/vector-search), [BM25 full-text](/api/search), and
[hybrid RAG retrieval](/api/search#hybrid-search) with no server, no API key,
and no data leaving the user's machine.

TalaDB runs in the browser as a WebAssembly module compiled from the same Rust
core used on every other platform. Data is persisted to the
[Origin Private File System](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API/Origin_private_file_system)
(OPFS) — a fast, private storage area built into modern browsers.

> Building with **Next.js**? There's a dedicated [Next.js guide](/guide/nextjs) covering providers, the RSC boundary, and using your API routes as the sync backend.

## Browser support

| Feature | Chrome | Firefox | Safari |
|---|---|---|---|
| WASM (in-memory) | 79+ | 78+ | 14+ |
| OPFS (fastest persistence) | 86+ | 111+ | 15.2+ |
| IndexedDB fallback (persistence without OPFS) | 79+ | 78+ | 14+ |

On browsers without OPFS, TalaDB automatically falls back to an
IndexedDB-backed in-memory database. Data still persists across page reloads.
Snapshots are written to IndexedDB with a short debounce so bulk inserts stay
fast.

## Installation

```bash
pnpm add taladb @taladb/web
```

## Vite setup

Add two things to `vite.config.ts`:

1. Exclude `taladb` and `@taladb/web` from dependency pre-bundling
2. Set the COOP/COEP headers required for the OPFS Worker context

```ts
// vite.config.ts
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  optimizeDeps: {
    exclude: ['taladb', '@taladb/web'],
  },
  server: {
    headers: {
      'Cross-Origin-Opener-Policy': 'same-origin',
      'Cross-Origin-Embedder-Policy': 'credentialless',
    },
  },
})
```

## Quick start

```ts
import { openDB } from 'taladb'

const db = await openDB('myapp.db')
const users = db.collection('users')

await users.insert({ name: 'Alice', age: 30 })
const all = await users.find()
```

`openDB` detects the browser automatically, opens a persistent OPFS database
named `myapp.db`, and returns a collection API identical to Node.js and React
Native.

## Defining your schema

TalaDB is schemaless, but TypeScript generics let you describe the shape of each
collection:

```ts
interface User {
  _id?: string
  name: string
  email: string
  age: number
  role: 'user' | 'admin'
  createdAt: number
}
```

## Basic CRUD

```ts
const users = db.collection<User>('users')

// Create indexes at startup — idempotent, safe to call on every open
await users.createIndex('email')
await users.createIndex('age')

// Insert — returns the generated ULID string
const id = await users.insert({
  name: 'Alice',
  email: 'alice@example.com',
  age: 30,
  role: 'user',
  createdAt: Date.now(),
})

// Insert many
await users.insertMany([
  { name: 'Bob',   email: 'bob@example.com',   age: 25, role: 'user',  createdAt: Date.now() },
  { name: 'Carol', email: 'carol@example.com', age: 35, role: 'admin', createdAt: Date.now() },
])

// Find all
const everyone = await users.find()

// Find with filter — uses index automatically
const adults = await users.find({ age: { $gte: 18 } })

// Find one
const alice = await users.findOne({ email: 'alice@example.com' })

// Count
const adminCount = await users.count({ role: 'admin' })

// Update
await users.updateOne({ email: 'alice@example.com' }, { $set: { age: 31 } })
await users.updateMany({ role: 'user' }, { $set: { verified: true } })

// Delete
await users.deleteOne({ email: 'alice@example.com' })
await users.deleteMany({ role: 'banned' })
```

## Queries

```ts
// Range
const thirties = await users.find({ age: { $gte: 30, $lte: 39 } })

// OR — uses IndexOr plan when both fields are indexed
const staff = await users.find({
  $or: [{ role: 'admin' }, { role: 'moderator' }],
})

// Membership test
const team = await users.find({ role: { $in: ['admin', 'moderator', 'editor'] } })

// Compound AND
const activeAdults = await users.find({
  $and: [{ age: { $gte: 18 } }, { role: { $ne: 'banned' } }],
})
```

## Full-text & hybrid search

Create an FTS index on a string field, then rank by relevance with `searchText`
(BM25), gate booleanly with `$contains`, or combine keyword + vector ranking
with `hybridSearch`:

```ts
const posts = db.collection<Post>('posts')

// Create once at startup — idempotent
await posts.createFtsIndex('body')

// Ranked keyword search (best matches first)
const hits = await posts.searchText('body', 'embedded rust database', 5)

// Boolean filter (every token must appear)
const all = await posts.find({ body: { $contains: 'taladb' } })
```

Pair an FTS index with a vector index and use
[`hybridSearch`](/api/search#hybrid-search) for RAG retrieval that runs entirely
in the browser. See the [Search reference](/api/search).

## Inspecting indexes

```ts
const { btree, fts, vector } = await users.listIndexes()
// btree: ['email', 'age']
// fts:   []
// vector: []
```

## Live queries in React

`subscribe` fires the callback immediately with the current results, then again
after any write that could affect the result set. Call the returned function to
unsubscribe.

```tsx
import { useEffect, useState } from 'react'
import { openDB, type Collection, type Document } from 'taladb'

function useLiveQuery<T extends Document>(col: Collection<T>, filter = {}) {
  const [docs, setDocs] = useState<T[]>([])

  useEffect(() => {
    const unsub = col.subscribe(filter, setDocs)
    return unsub
  }, [])

  return docs
}

// Usage
const admins = useLiveQuery(db.collection<User>('users'), { role: 'admin' })
```

## Vector search

Store and search embeddings from an on-device model — no cloud API, no data
leaving the browser.

```ts
import { pipeline } from '@huggingface/transformers'

// Model is downloaded and cached on first use (~25 MB for MiniLM)
const embedder = await pipeline('feature-extraction', 'Xenova/all-MiniLM-L6-v2')

async function embed(text: string): Promise<number[]> {
  const out = await embedder(text, { pooling: 'mean', normalize: true })
  return Array.from(out.data) as number[]
}
```

```ts
interface Article {
  _id?: string
  title: string
  body: string
  category: string
  embedding: number[]
}

const articles = db.collection<Article>('articles')

// Create once at startup — idempotent
await articles.createVectorIndex('embedding', { dimensions: 384 })

// Insert with embedding
await articles.insert({
  title: 'Getting started',
  body: '...',
  category: 'guide',
  embedding: await embed('Getting started'),
})

// Semantic search
const queryVec = await embed('how do I begin')
const results = await articles.findNearest('embedding', queryVec, 5)

results.forEach(({ document, score }) => {
  console.log(`${score.toFixed(3)}  ${document.title}`)
})

// Filtered vector search: filter first, then rank — one call, no extra round-trips
const filtered = await articles.findNearest('embedding', queryVec, 5, {
  category: 'guide',
})
```

### Similarity metrics

| Metric | Best for | Score range |
|---|---|---|
| `cosine` (default) | Text embeddings, normalised vectors | [-1, 1] |
| `dot` | Embeddings where magnitude matters | Unbounded |
| `euclidean` | Spatial / coordinate data | (0, 1] |

## Multi-tab behaviour

TalaDB uses the [Web Locks API](https://developer.mozilla.org/en-US/docs/Web/API/Web_Locks_API)
to coordinate database access across tabs sharing the same origin.

**Primary tab** — the first tab to open a given database acquires an exclusive
lock on the OPFS file. All writes go directly to the persistent file.

**Secondary tabs** — additional tabs open an in-memory copy seeded from an
IndexedDB snapshot. They stay read-consistent within ~500 ms of any primary-tab
write via BroadcastChannel. Writes made on a secondary tab are automatically
merged into the primary tab's OPFS database using Last-Write-Wins — no extra
code required.

```
Tab A (primary, OPFS)          Tab B (secondary, in-memory)
      │                                    │
      │←── write from Tab B ───────────────┤  BroadcastChannel changeset
      │    importChangeset()               │
      │    write to OPFS                   │
      │─── taladb:changed ────────────────→│  Tab B reloads snapshot
```

For bulk write workloads across many tabs, prefer routing mutations through the
primary tab. The merge path adds one BroadcastChannel round-trip per write batch.

## HTTP push sync

Pass a `config` option to `openDB` to push mutation events to a remote endpoint
after every write:

```ts
import { openDB } from 'taladb'

const db = await openDB('myapp.db', {
  config: {
    sync: {
      enabled: true,
      endpoint: 'https://api.example.com/taladb-events',
      headers: { Authorization: `Bearer ${myToken}` },
      exclude_fields: ['embedding'],  // omit large vector fields from payloads
    },
  },
})

// Every write now fires an HTTP POST in the background
const users = db.collection('users')
await users.insert({ name: 'Alice', role: 'admin' })
```

After every committed write, TalaDB fires a background `fetch` on the JS
microtask queue and POSTs the event payload to the configured endpoint with up
to **3 retries** and exponential backoff (200 ms / 400 ms / 800 ms). Writes are
never blocked.

::: warning Tab lifetime
In-flight sync requests are subject to normal browser fetch constraints. If the
user closes the tab during a retry sequence, any remaining attempts are lost.
HTTP push sync is best-effort by design.

The worker bounds its HTTP queue and times each attempt out after 10 seconds.
You can inspect delivery health and drain accepted events before closing:

```ts
const health = await db.syncStatus?.() // { pending, dropped, failed }
const drained = await db.flushSync?.(5_000)
```
:::

Per-event endpoint overrides are supported:

```ts
const db = await openDB('myapp.db', {
  config: {
    sync: {
      enabled: true,
      endpoint: 'https://api.example.com/events',
      insert_endpoint: 'https://api.example.com/events/insert',
      update_endpoint: 'https://api.example.com/events/update',
      delete_endpoint: 'https://api.example.com/events/delete',
      headers: { Authorization: 'Bearer YOUR_TOKEN' },
    },
  },
})
```

See the [HTTP Push Sync guide](/guide/http-sync) for the full config reference,
payload shapes, and retry behaviour.

## Bidirectional sync

HTTP push sync sends local writes to your server. To pull remote changes back
into the browser — for multi-device or offline-first scenarios — use the
changeset API:

```ts
let lastSyncMs = 0

async function sync() {
  // Push local changes since last sync
  const outgoing = await db.exportChangeset(['users', 'posts'], lastSyncMs)
  await fetch('/sync/push', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: outgoing,
  })

  // Pull remote changes since last sync
  const resp = await fetch(`/sync/pull?since=${lastSyncMs}`)
  const applied = await db.importChangeset(await resp.text())
  if (applied > 0) console.log(`Merged ${applied} remote change(s)`)

  lastSyncMs = Date.now()
}

// Sync on load, then every 30 s
sync()
setInterval(sync, 30_000)
```

`exportChangeset` and `importChangeset` use Last-Write-Wins conflict resolution
with ULID tie-breaking for deterministic merge across any number of replicas.
Deletes are tombstoned so they propagate correctly through every sync cycle.
You supply the transport — fetch polling, WebSocket, SSE, or WebRTC data
channel.

### Conflict resolution

Every document carries an internal `_changed_at` timestamp (set automatically
on every insert and update — no manual stamping required). When two replicas
both modified the same document, the one with the higher `_changed_at` wins. If
timestamps are equal, the higher ULID wins, giving a deterministic total order
without coordination.

### Tombstone management

Deleted document IDs are kept as tombstones so deletions propagate correctly via
`exportChangeset`. Tombstones accumulate over time and should be pruned
periodically once you are confident all replicas have received the deletion:

```ts
// On app startup — prune tombstones older than your retention window
const THIRTY_DAYS_MS = 30 * 24 * 60 * 60 * 1000
const cutoff = Date.now() - THIRTY_DAYS_MS

for (const name of ['users', 'posts', 'comments']) {
  const pruned = await db.compactTombstones(name, cutoff)
  if (pruned > 0) console.log(`Pruned ${pruned} tombstone(s) from '${name}'`)
}
```

## Migrations

Run schema changes at open time — each migration runs once, in order,
atomically:

```ts
const db = await openDB('myapp.db', {
  migrations: [
    {
      version: 1,
      description: 'Add email index',
      up: async (db) => {
        await db.collection('users').createIndex('email')
      },
    },
    {
      version: 2,
      description: 'Backfill role field',
      up: async (db) => {
        const users = db.collection('users')
        for (const user of await users.find({})) {
          if (!user.role) {
            await users.updateOne({ _id: user._id }, { $set: { role: 'user' } })
          }
        }
      },
    },
  ],
})
```

## Exporting a snapshot

```ts
// Export the whole database to a Uint8Array
const bytes = await db.exportSnapshot()

// Save as a file download
const blob = new Blob([bytes], { type: 'application/octet-stream' })
const url  = URL.createObjectURL(blob)
const a    = Object.assign(document.createElement('a'), { href: url, download: 'myapp.taladb' })
a.click()
```

## Closing the database

```ts
await db.close()
```

Calling `close()` flushes any pending IDB snapshot, releases the Web Lock, and
allows another tab to acquire the OPFS file as the new primary.

## Current limitations

- **HNSW vector index** — not available in the browser. The HNSW algorithm uses
  `rayon` for parallelism which requires native threads. Calling
  `createVectorIndex({ indexType: 'hnsw' })` or `upgradeVectorIndex()` in the
  browser throws a clear error. Flat (brute-force) vector search works correctly
  and scales to ~100k vectors without an index upgrade.

- **Snapshot size** — `exportSnapshot` and the IndexedDB fallback path
  serialise the entire database to a `Uint8Array` in Wasm memory. This works
  well for databases under ~50 MB. Beyond that, the serialisation overhead
  becomes noticeable. OPFS-backed storage (the default when OPFS is available)
  is not affected — it writes directly to the file with no in-memory copy.
