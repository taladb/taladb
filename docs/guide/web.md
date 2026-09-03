---
title: Web Guide — Browser / WASM
description: Use TalaDB in the browser with WebAssembly and OPFS persistent storage. Works with Vite, Next.js, and any modern bundler.
---

# Web (Browser / WASM)

Run a full vector and document database in the browser tab. Store and query
application data, then pair TalaDB with an on-device embedding model
(transformers.js, ONNX Runtime Web) to add
[semantic search](/api/vector-search), [BM25 full-text](/api/search), and
[hybrid RAG retrieval](/api/search#hybrid-search) with no server, no API key,
and no data leaving the user's machine.

TalaDB runs in the browser as a WebAssembly module compiled from the same Rust
core used on every other platform. Data is persisted to the
[Origin Private File System](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API/Origin_private_file_system)
(OPFS) — a fast, private storage area built into modern browsers.

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

## Next.js setup

Turbopack — the default for `next dev` and `next build` since Next 16 — needs no
configuration at all from **0.11.1** onward. Install, mount the provider on the
client, and you are done. The rest of this section is the things that are easy
to get wrong, in the order they bite.

### The provider is client-only, and so is everything under it

`TalaDBProvider` opens the database in an effect, and effects do not run on the
server. It renders its `fallback` during SSR, which means **nothing beneath it
appears in the server-rendered HTML**, and there is no `dehydrate` /
`HydrationBoundary` equivalent to prefetch through.

So do not wrap `app/layout.tsx`, and do not wrap a page whose content has to be
indexed. Mount it around the smallest subtree that reads from TalaDB:

```tsx
// app/providers/taladb.tsx
'use client'

import { TalaDBProvider } from '@taladb/react'

export function LocalData({ children }: { children: React.ReactNode }) {
  return (
    <TalaDBProvider name="myapp.db" collections={{ books: {} }} fallback={null}>
      {children}
    </TalaDBProvider>
  )
}
```

To keep a page server-rendered *and* leave a local copy behind, render the
server's data as HTML and hydrate it into TalaDB from a client component that
renders `null`. The page stays indexable; the collection ends up warm.

### Content-Security-Policy

The engine is WebAssembly running inside a Worker, so a CSP needs both of these
or the database never opens:

```
script-src 'self' 'wasm-unsafe-eval';
worker-src 'self' blob:;
```

### Webpack, if you opt out of Turbopack

`next dev --webpack` / `next build --webpack` needs help that Turbopack does not.
Webpack only follows the inline `new Worker(new URL(…, import.meta.url))` form,
and the browser build spawns its worker from a variable — so it copies the worker
out verbatim without ever bundling the worker's own
`import('../pkg/taladb_web.js')`. At runtime that relative URL resolves to
`/_next/static/pkg/taladb_web.js`, which nothing has emitted. The symptom is a
white screen on a cold start.

Emit both files there:

```ts
// next.config.ts
webpack(config, { isServer }) {
  if (isServer) return config

  config.experiments = { ...config.experiments, asyncWebAssembly: true }
  config.module.rules.push({
    test: /\.wasm$/,
    type: 'asset/resource',
    generator: { filename: 'static/wasm/[name][ext]' },
  })

  const path = require('path')
  const fs = require('fs')
  const pkgDir = path.dirname(require.resolve('@taladb/web/pkg/taladb_web.js'))

  config.plugins.push({
    apply(compiler) {
      const { Compilation, sources } = compiler.webpack
      compiler.hooks.thisCompilation.tap('EmitTaladbPkg', (compilation) => {
        compilation.hooks.processAssets.tap(
          { name: 'EmitTaladbPkg', stage: Compilation.PROCESS_ASSETS_STAGE_ADDITIONAL },
          () => {
            for (const file of ['taladb_web.js', 'taladb_web_bg.wasm']) {
              if (compilation.getAsset(`static/pkg/${file}`)) continue
              compilation.emitAsset(
                `static/pkg/${file}`,
                new sources.RawSource(fs.readFileSync(path.join(pkgDir, file))),
              )
            }
          },
        )
      })
    },
  })

  return config
}
```

### `transpilePackages`

Not required, but harmless and occasionally useful if another tool in your chain
chokes on the published ESM:

```ts
transpilePackages: ['taladb', '@taladb/web', '@taladb/react']
```

### Weight

The engine is roughly **570 kB gzipped** of WebAssembly, fetched when the
database first opens. That is a real cost on a landing page, and a JS-only
bundle budget will not see it. On a page where TalaDB is not needed for the
first paint, mount the provider after the page is interactive:

```tsx
const [ready, setReady] = useState(false)
useEffect(() => {
  const id = requestIdleCallback(() => setReady(true), { timeout: 5000 })
  return () => cancelIdleCallback(id)
}, [])
if (!ready) return null
```

### React StrictMode

Safe from **0.11.1**. Handles are shared and reference-counted per database
name, so StrictMode's double-mount opens one instance rather than two. On
0.11.0 and earlier it opened two, the second lost the OPFS lock, fell back to an
IndexedDB snapshot, and silently dropped its writes — if you are pinned there,
set `reactStrictMode: false`.

### Linking a local checkout

Two settings, and Next requires them to agree — it warns and then silently
prefers `outputFileTracingRoot`, so changing only one looks applied but is not:

```ts
outputFileTracingRoot: path.join(__dirname, '../..'),
turbopack: { root: path.join(__dirname, '../..') },
```

Both must be an ancestor of the linked checkout, or Turbopack reports
`Can't resolve 'taladb'`. Note also that `link:` brings the checkout's own
`node_modules` along — a second copy of `@types/react` there will clash with
yours. A `pnpm pack` tarball installed by `file:` avoids both problems.

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
write via BroadcastChannel.

```
Tab A (primary, OPFS)          Tab B (secondary, in-memory)
      │                                    │
      │─── taladb:changed ────────────────→│  Tab B reloads snapshot
```

Writes made on a secondary tab are forwarded to the primary tab over the same
BroadcastChannel and applied there, so the OPFS file stays authoritative — no
extra code required.

```
Tab A (primary, OPFS)          Tab B (secondary, in-memory)
      │                                    │
      │←── taladb:tab-write ───────────────┤  forwarded write
      │    apply + write to OPFS           │
      │─── taladb:changed ────────────────→│  Tab B reloads snapshot
```

Inserts are forwarded as whole documents carrying their `_id`s, so an id your
code already holds stays valid after the hand-off. Filter-based updates and
deletes forward the filter and update instead, and the primary re-evaluates them
against authoritative data. Concurrent edits resolve by arrival order at the
primary — two tabs on one device share a clock, so there is no skew to arbitrate.

For bulk write workloads across many tabs, prefer routing mutations through the
primary tab. Forwarding adds one BroadcastChannel round-trip per write.

### `isPrimary()`

Ask whether this tab's writes land authoritatively:

```ts
if (await db.isPrimary?.() ?? true) {
  await drainOutbox();
}
```

Use it for work that must not run in more than one tab at once, or that depends
on reading its own writes straight back — a background queue drainer, a
scheduled cleanup pass, an outbound sync loop. On a secondary tab both
assumptions fail: other tabs' writes arrive up to ~500 ms late, and its own
writes only become visible to everyone once the primary has applied them. A
drainer running there will re-send work it has already sent.

Primary status changes during a session — closing the owning tab promotes
another — so **re-check it rather than caching the answer**. A per-cycle check
at the top of a loop is enough.

It returns `true` on Node.js and React Native, where a single process owns the
database, and on the in-memory Safari fallback, where each tab holds its own
isolated database and forwards nothing. It may be absent on older `@taladb/web`
builds, so treat absence as `true`.

## Change webhook

Report every committed write to a backend over HTTP:

```ts
import { openDB } from 'taladb'

const db = await openDB('myapp.db', {
  webhook: {
    enabled: true,
    endpoint: 'https://api.example.com/taladb',
    headers: { Authorization: `Bearer ${myToken}` },
    exclude_fields: ['embedding'],  // omit large vector fields from payloads
  },
})

// Every write now fires an HTTP request after the commit
const users = db.collection('users')
await users.insert({ name: 'Alice', role: 'admin' })   // → POST
```

`POST` on insert, `PUT` on update, `DELETE` on delete. Requests are dispatched
after the commit with 3 retries and exponential backoff (200 / 400 / 800 ms) on
5xx and network errors. Writes are never blocked.

::: warning Tab lifetime
Delivery is **best effort**. Closing the tab can lose queued events; a lost
response can cause a retry. Retries carry a stable `event_id` and
`Idempotency-Key` so the receiver can deduplicate them.

Drain the queue at a moment you control:

```ts
window.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden') void db.flushWebhook?.(2_000)
})

db.webhookStats?.()   // { pending, delivered, failed, dropped }
```

`db.close()` drains automatically.
:::

Per-op endpoint overrides are supported:

```ts
const db = await openDB('myapp.db', {
  webhook: {
    enabled: true,
    endpoint: 'https://api.example.com/events',
    insert_endpoint: 'https://api.example.com/events/insert',
    update_endpoint: 'https://api.example.com/events/update',
    delete_endpoint: 'https://api.example.com/events/delete',
  },
})
```

See the [Change Webhook reference](/api/webhook) for the full config reference,
payload shapes, and delivery semantics.

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

::: warning Not available through `openDB()`
Snapshot export lives on the raw `@taladb/web` WASM handle, not on the database
object `openDB()` returns, and the Node binding does not expose it at all. The
example below uses the binding directly.
:::

```ts
import init, { TalaDbWeb } from '@taladb/web'

await init()
const raw = TalaDbWeb.open('myapp.db')

// Export the whole database to a Uint8Array
const bytes = raw.exportSnapshot()

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
