---
title: Node.js Guide
description: Use TalaDB in Node.js via a prebuilt native module. Fast Rust engine with no subprocess — works in Express, Fastify, and CLI tools.
---

# Node.js

TalaDB's Node.js integration uses a prebuilt native `.node` module via [napi-rs](https://napi.rs). The Rust engine runs natively — no WASM, no subprocess — with performance identical to embedding the library directly in Rust.

## Requirements

- Node.js **18+**
- Supported platforms: `linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64` (Apple Silicon), `win32-x64`

## Installation

```bash
pnpm add taladb @taladb/node
```

## Quick start

```ts
import { openDB } from 'taladb'

const db = await openDB('./myapp.db')
const users = db.collection('users')

await users.insert({ name: 'Alice', age: 30 })
const all = await users.find()
```

`openDB` detects Node.js automatically and routes calls through the native module. The database file is created if it does not exist. Parent directories must exist.

## Change webhook

Report every committed write to a backend over HTTP. Add a `webhook` block to
`taladb.config.yml` next to the engine's `durability` settings:

```yaml
webhook:
  enabled: true
  endpoint: "https://api.example.com/taladb"
  headers:
    Authorization: "Bearer my-token"
  exclude_fields:
    - embedding
```

`openDB` auto-discovers the file — no code change needed. Or pass it inline:

```ts
const db = await openDB('app.db', {
  webhook: { enabled: true, endpoint: 'https://api.example.com/taladb' },
})
```

See the [Change Webhook reference](/api/webhook) for payload shapes, retry
behaviour, and the best-effort delivery/idempotency contract.

## Basic CRUD

```ts
import { openDB } from 'taladb'

interface Task {
  _id?: string
  title: string
  done: boolean
  priority: 1 | 2 | 3
  createdAt: number
}

const db = await openDB('./tasks.db')
const tasks = db.collection<Task>('tasks')

// Indexes — create once at startup, idempotent
await tasks.createIndex('done')
await tasks.createIndex('priority')

// Insert — returns the generated ULID string
const id = await tasks.insert({
  title: 'Write documentation',
  done: false,
  priority: 1,
  createdAt: Date.now(),
})

// Find
const urgent = await tasks.find({ $and: [{ done: false }, { priority: 1 }] })
const task   = await tasks.findOne({ _id: id })

// Update
await tasks.updateOne({ _id: id }, { $set: { done: true } })

// Delete
const removed = await tasks.deleteMany({ done: true })
```

## Full-text search — BM25 ranking

Create an FTS index on any string field, then rank by relevance with `searchText` (BM25) or filter booleanly with `$contains`:

```ts
const posts = db.collection<Post>('posts')

// Create once at startup — idempotent
await posts.createFtsIndex('body')

// Ranked search — best matches first (OR semantics)
const hits = await posts.searchText('body', 'embedded rust database', 5)
// [{ document: Post, score: 3.14 }, ...]

// Boolean filter — every token must be present
const all = await posts.find({ body: { $contains: 'taladb' } })
```

## Hybrid search — keyword + vector

Fuse BM25 and vector rankings with reciprocal rank fusion — the on-device RAG retrieval recipe. Requires an FTS index and a vector index on the collection:

```ts
const results = await posts.hybridSearch(
  { textField: 'body',      text: 'how do I get a refund' },
  { vectorField: 'embedding', vector: await embed('how do I get a refund') },
  5,
)
// [{ document, score, textRank, vectorRank }, ...]
```

See the [Search reference](/api/search) for filters and tuning.

## Inspecting indexes

```ts
const { btree, fts, vector } = await tasks.listIndexes()
// btree: ['done', 'priority']
// fts:   []
// vector: []
```

## Live queries

`subscribe` fires the callback immediately and again after any write that affects the result:

```ts
const unsub = tasks.subscribe({ done: false }, (pending) => {
  console.log(`${pending.length} tasks remaining`)
})

// Later — stop listening
unsub()
```

## Vector search

TalaDB's vector index works on Node.js with any embedding source — a local model, the OpenAI API, or anything that returns a `number[]`.

```ts
// Option A: local model, no API key
import { pipeline } from '@huggingface/transformers'
const embedder = await pipeline('feature-extraction', 'Xenova/all-MiniLM-L6-v2')
async function embed(text: string): Promise<number[]> {
  const out = await embedder(text, { pooling: 'mean', normalize: true })
  return Array.from(out.data) as number[]
}

// Option B: OpenAI
import OpenAI from 'openai'
const openai = new OpenAI()
async function embed(text: string): Promise<number[]> {
  const res = await openai.embeddings.create({ model: 'text-embedding-3-small', input: text })
  return res.data[0].embedding
}
```

```ts
interface Doc {
  _id?: string
  content: string
  source: string
  embedding: number[]
}

const docs = db.collection<Doc>('docs')
await docs.createVectorIndex('embedding', { dimensions: 384 })

// Insert
await docs.insert({ content, source: 'readme', embedding: await embed(content) })

// Semantic search
const results = await docs.findNearest('embedding', await embed('local database'), 5)
for (const { document, score } of results) {
  console.log(score.toFixed(3), document.content)
}

// Filtered vector search: filter + rank in one call
const filtered = await docs.findNearest('embedding', await embed('local database'), 5, {
  source: 'readme',
})
```

### HNSW for large collections

On Node.js, HNSW (approximate nearest-neighbour) is available for faster search on large collections:

```ts
// Create with HNSW from the start
await docs.createVectorIndex('embedding', {
  dimensions: 384,
  indexType: 'hnsw',
  hnswM: 16,              // graph connectivity — higher = better recall, more memory
  hnswEfConstruction: 200, // build-time quality
})

// Or upgrade an existing flat index in-place (no data loss)
await docs.upgradeVectorIndex('embedding')

// findNearest API is identical — HNSW is transparent
const results = await docs.findNearest('embedding', queryVec, 10)
```

### Ingestion script

```ts
import { openDB } from 'taladb'
import fs from 'node:fs/promises'

const db   = await openDB('./knowledge.db')
const docs = db.collection<Doc>('docs')
await docs.createVectorIndex('embedding', { dimensions: 1536 })

const files = await fs.readdir('./content')
for (const file of files) {
  const content = await fs.readFile(`./content/${file}`, 'utf8')
  await docs.insert({ content, source: file, embedding: await embed(content) })
}

console.log(`Indexed ${files.length} documents`)
await db.close()
```

## Server example

```ts
// server.ts — Express + TalaDB
import express from 'express'
import { openDB } from 'taladb'

interface Event {
  _id?: string
  type: string
  payload: Record<string, unknown>
  ts: number
}

const app    = express().use(express.json())
const db     = await openDB('./events.db')
const events = db.collection<Event>('events')
await events.createIndex('type')
await events.createIndex('ts')

app.post('/events', async (req, res) => {
  const id = await events.insert({ type: req.body.type, payload: req.body.payload ?? {}, ts: Date.now() })
  res.json({ id })
})

app.get('/events', async (req, res) => {
  const filter: Record<string, unknown> = {}
  if (req.query.type)  filter.type = req.query.type
  if (req.query.since) filter.ts   = { $gte: Number(req.query.since) }
  res.json(await events.find(filter))
})

app.listen(3000)
```

## Migrations

Each migration runs once at open time, in version order, inside an atomic transaction:

```ts
const db = await openDB('./myapp.db', {
  migrations: [
    {
      version: 1,
      description: 'Index users by email',
      up: async (db) => {
        await db.collection('users').createIndex('email')
      },
    },
  ],
})
```

## Snapshot export / import

```ts
import fs from 'node:fs/promises'

// Export
const bytes = await db.exportSnapshot()
await fs.writeFile('backup.taladb', bytes)
```

## Testing with an in-memory database

```ts
// vitest / jest
import { TalaDBNode } from '@taladb/node'

let db: ReturnType<typeof TalaDBNode.openInMemory>

beforeEach(() => { db = TalaDBNode.openInMemory() })
afterEach(() => { db.close() })
```

No file system cleanup, no interference between test runs.

## Closing

```ts
await db.close()
```

Always close the database before the process exits to flush any pending writes and release the file lock.
