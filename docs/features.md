---
title: Features
description: Document storage and vector similarity search in one embedded database, with MongoDB-like queries, aggregation pipelines, secondary indexes, ACID transactions, full-text search, live queries, encryption, and more.
---

# Features

TalaDB combines a full document database with built-in vector search. Documents, metadata, and embeddings share one embedded engine, one transactional model, and one TypeScript API across the browser, Node.js, and React Native.

## Vector index and similarity search

TalaDB v0.2 introduces on-device vector indexes — the first embedded JavaScript database to natively combine document queries with vector similarity search.

### Creating a vector index

```ts
// Register a vector index on any numeric-array field
await articles.createVectorIndex('embedding', { dimensions: 384 })

// Choose a metric (default: cosine)
await articles.createVectorIndex('embedding', {
  dimensions: 1536,
  metric: 'cosine', // | 'dot' | 'euclidean'
})
```

Existing documents are backfilled automatically. New inserts and updates maintain the index atomically alongside the document write.

### Pure vector search

```ts
const queryVec = await embed('how do I reset my password?') // your on-device model

const results = await articles.findNearest('embedding', queryVec, 5)
// [{ document: Article, score: 0.94 }, { document: Article, score: 0.91 }, ...]
```

Results are ordered by descending similarity score. Score range depends on the metric:
- `cosine` — [-1, 1], identical vectors score 1.0
- `dot` — unbounded, depends on vector magnitude
- `euclidean` — (0, 1], identical vectors score 1.0

### Filtered vector search — narrow, then rank

The killer feature. Pass a regular document filter as the fourth argument to restrict the candidate set before ranking:

```ts
// Find the 5 most semantically similar english-language support articles
const results = await articles.findNearest('embedding', queryVec, 5, {
  locale: 'en',
  category: 'support',
  published: true,
})
```

This is the pattern cloud vector databases (Qdrant, Weaviate, Pinecone) charge for — running entirely on device, with no network latency and no data leaving the user's device.

### Optional HNSW index (Node.js)

The default index is **flat** — an exact scan over every vector, with no approximation and no recall trade-off. On Node.js (since v0.8.3) you can opt into an approximate HNSW graph for larger corpora:

```ts
await articles.createVectorIndex('embedding', {
  dimensions: 384,
  indexType: 'hnsw', // default: 'flat'
})

// The graph is built at creation time and NOT updated by later writes —
// rebuild it after bulk ingests (e.g. during an idle period):
await articles.upgradeVectorIndex('embedding')
```

Two caveats before reaching for it: graph construction is CPU-intensive (a one-off cost that grows quickly with collection size), and recall depends on your data's structure — HNSW is approximate, so measure both speed and recall on your own embeddings. The flat index remains the right default for most on-device corpora, and is what ships on web and React Native.

### Dropping a vector index

```ts
await articles.dropVectorIndex('embedding')
```

### Pairing with on-device AI

TalaDB vector indexes are designed to work alongside client-side embedding models:

```ts
import { pipeline } from '@xenova/transformers'

const embedder = await pipeline('feature-extraction', 'Xenova/all-MiniLM-L6-v2')

async function embed(text: string): Promise<number[]> {
  const output = await embedder(text, { pooling: 'mean', normalize: true })
  return Array.from(output.data)
}

// Insert a document with its embedding
const vec = await embed(article.body)
await articles.insert({ ...article, embedding: vec })

// Search later
const results = await articles.findNearest('embedding', await embed(query), 5)
```

No cloud API key. No rate limit. No round-trip.

## Document model with ULID IDs

Documents are schemaless key-value maps. Every document is automatically assigned a [ULID](https://github.com/ulid/spec) — a 128-bit, time-sortable, lexicographically ordered identifier. ULIDs are safe for URLs, monotonically increasing within the same millisecond, and require no coordination between writers.

## MongoDB-like query API

TalaDB exposes a filter and update DSL that mirrors the MongoDB query language:

- **Comparison:** `$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`
- **Membership:** `$in`, `$nin`
- **Logical:** `$and`, `$or`, `$not`
- **Existence:** `$exists`
- **Update operators:** `$set`, `$unset`, `$inc`, `$push`, `$pull`

The full API is fully typed — TypeScript narrows filter and update shapes to the document type passed to `db.collection<T>()`.

## Aggregation pipelines

Compute summaries inside the Rust engine instead of materialising every document in JavaScript. The pipeline mirrors MongoDB's aggregation framework and is available on every runtime:

```ts
const byStatus = await orders.aggregate([
  { $match: { createdAt: { $gte: monthStart } } }, // leading $match uses an index
  { $group: { _id: '$status', total: { $sum: '$amount' }, n: { $sum: 1 } } },
  { $sort: { total: -1 } },
  { $limit: 10 },
])
```

- **Stages:** `$match`, `$group`, `$sort`, `$skip`, `$limit`, `$project`
- **Accumulators:** `$sum`, `$count`, `$avg`, `$min`, `$max`, `$push`, `$addToSet`, `$first`, `$last`
- Runs as a single pass over the collection; a leading `$match` goes through the query planner, so indexed filters skip the full scan
- Fully typed via `AggregatePipeline<T>`

See the [Aggregation reference](/api/aggregation) for the full API.

## Secondary indexes with automatic index selection

Indexes are created per field with `createIndex('fieldName')`. The underlying storage is a redb B-tree keyed by `[type_prefix][encoded_value][ulid]`. This layout gives:

- **O(log n) point lookups** via `$eq` or `$in`
- **O(log n + k) range scans** via `$gt`, `$gte`, `$lt`, `$lte`
- **Correct cross-type ordering** — integers, floats, strings, and booleans each carry a type prefix so they never collide in the same index

The query planner examines the filter and picks the most selective available index automatically. No hints or query annotations are needed.

### Compound (multi-field) indexes

Index an ordered list of fields to serve a multi-field equality query with a single index scan:

```ts
await orders.createCompoundIndex(['userId', 'status'])

// One index scan instead of a full-collection scan:
await orders.find({ userId: 'u_123', status: 'open' })
```

The planner uses the compound index when an `$and` constrains **every** field of the index by equality. Fields are ascending; partial-prefix and trailing-range matches, and per-field descending order, are on the [roadmap](/roadmap). Available on Node.js and the browser; React Native support is implemented and pending on-device verification.

## ACID transactions via redb

Every write is wrapped in an ACID transaction at the storage layer. Document writes and index updates happen atomically — there is no window where a document exists but its index entry is missing, or vice versa. On crash or power loss, redb recovers to the last committed state.

## Full-text search — BM25 ranking

`createFtsIndex` builds an inverted token index over a string field. It powers two things: the `$contains` filter (boolean, "does this field contain all these tokens") and **`searchText`** — relevance-ranked keyword search using [BM25](https://en.wikipedia.org/wiki/Okapi_BM25), the same model behind Lucene and Elasticsearch, running on-device.

```ts
await posts.createFtsIndex('body')

// Ranked search — best matches first, OR semantics (more matching terms rank higher)
const hits = await posts.searchText('body', 'rust embedded database', 5)
// [{ document: Post, score: 3.14 }, { document: Post, score: 1.87 }, ...]

// Boolean filter — every token must be present
const all = await posts.find({ body: { $contains: 'rust embedded database' } })
```

`searchText` accepts an optional metadata filter and `{ k1, b }` to tune term-frequency saturation and length normalisation. See [Search](/api/search) for the full API.

## Hybrid search — keyword + vector, fused

Keyword search misses paraphrases; vector search misses exact identifiers, SKUs, and rare proper nouns. **`hybridSearch`** runs both retrievers and fuses their rankings with [reciprocal rank fusion](https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf) (RRF) — the standard recipe for production RAG retrieval, running entirely on-device.

```ts
await articles.createFtsIndex('body')
await articles.createVectorIndex('embedding', { dimensions: 384 })

const results = await articles.hybridSearch(
  { textField: 'body',      text: 'how do I get a refund' },
  { vectorField: 'embedding', vector: await embed('how do I get a refund') },
  5,
)

results.forEach(({ document, score, textRank, vectorRank }) => {
  // textRank / vectorRank show which retriever found each hit (null = missed)
})
```

A document both retrievers rank well beats one only a single retriever found. Fusion operates on **ranks, not scores**, so there's no fragile normalisation between unbounded BM25 and cosine ∈ [-1, 1]. A metadata filter applies to both retrievers before ranking. See [Search](/api/search) for tuning (`rrfK`, `textWeight`, `vectorWeight`, `candidates`).

## Live queries

Subscribe to a filter and receive a fresh snapshot after every matching write — no polling, no websockets:

```ts
const unsub = articles.subscribe({ category: 'support' }, (docs) => {
  render(docs) // fires immediately with the current results, then on every write
})
unsub()
```

In the browser, writes from *other tabs* trigger subscriptions too (via `BroadcastChannel`). At the Rust level the same mechanism is exposed as `collection.watch(filter)` → `WatchHandle`; multiple handles can watch the same collection with different filters, with no per-watch overhead beyond an MPSC channel. See [Live Queries](/api/live-queries).

## Schema migrations

`openDB` accepts a `migrations` array. TalaDB stores the current version number inside the database and runs any pending `up` functions in order at startup, inside a single atomic transaction. Failed migrations roll back automatically.

## Encryption at rest

Pass a `passphrase` to `openDB` (Node.js and the browser) — or to `TalaDBModule.initialize` on React Native — and every value is transparently encrypted with **AES-GCM-256** before it touches disk, on all three runtimes:

```ts
const db = await openDB('myapp.db', { passphrase: userSuppliedPassphrase })
```

- `EncryptedBackend` — a `StorageBackend` wrapper that encrypts every value with AES-GCM-256 before writing and decrypts on read; it wraps the file backend (Node/RN) or the OPFS backend (browser) identically
- Keys are derived with **PBKDF2-HMAC-SHA256** (600,000 iterations) from the passphrase and a random 16-byte salt stored beside the database
- Per-write random nonces; the 16-byte GCM tag detects tampering and rejects a wrong passphrase before returning a handle

Document IDs and index keys are not encrypted (they must remain comparable for lookups) — so don't index a field whose confidentiality you rely on encryption for. See the [Encryption reference](/api/encryption) for the full model, including the browser's OPFS/single-tab requirement.

## OPFS-backed browser persistence

In the browser, TalaDB runs inside a Dedicated Worker per tab and persists to the Origin Private File System (OPFS) via `FileSystemSyncAccessHandle` — durable, origin-isolated storage without IndexedDB's overhead. Multi-tab safety comes from the Web Locks API: the first tab's worker holds an exclusive lock on the OPFS file; other tabs coordinate through a `BroadcastChannel`, which also powers instant cross-tab live-query updates.

The engine runs redb directly on the OPFS file and flushes every commit, so the browser is durable per commit — the same model as Node.js. Opt into batched commits with `durability: { flush_every_write: false }` plus an explicit `db.flush()` when you want throughput instead. When OPFS is unavailable (cross-origin iframes, older browsers), TalaDB falls back to an in-memory database seeded from an IndexedDB snapshot — that auxiliary snapshot is the one written on a short debounce — so data still survives page reloads.

## Platform-detecting unified package

The `taladb` npm package auto-detects the runtime at import time:

- Presence of `globalThis.nativeCallSyncHook` → React Native JSI
- Presence of `globalThis.window` + `navigator` → browser WASM
- Otherwise → Node.js native module

Application code never branches on platform. Swap the runtime and the same TypeScript continues to work.

## Schema validation (optional)

Pass a `schema` option to `db.collection()` to get runtime type safety on writes. Any object with a `parse(data: unknown): T` method works — Zod, Valibot, or a hand-rolled validator.

```ts
import { z } from 'zod'

const schema = z.object({ name: z.string().min(1), age: z.number() })

// Without a schema — schemaless, no overhead (default behaviour)
const users = db.collection<User>('users')

// With a schema — validates every insert before writing
const users = db.collection<User>('users', { schema })

await users.insert({ name: 'Alice', age: 30 }) // ✓ stored
await users.insert({ name: '', age: 30 })       // ✗ throws TalaDbValidationError
```

`insert` and `insertMany` run the document through `schema.parse()` before storage. If validation fails, a `TalaDbValidationError` is thrown and nothing is written. Collections without a `schema` option have zero overhead.

See the [Schema Validation reference](/api/schema) for the full API including `validateOnRead` and Valibot usage.

## TypeScript generics

```ts
interface Product {
  _id?: string
  name: string
  price: number
  inStock: boolean
}

const products = db.collection<Product>('products')

// Filter<Product> — TypeScript validates field names and value types
await products.find({ price: { $lte: 99.99 }, inStock: true })

// Update<Product> — $set only accepts fields that exist on Product
await products.updateOne({ name: 'Widget' }, { $set: { price: 49.99 } })
```

## Change webhook

TalaDB is an embedded database — it does not replicate. When a backend needs to
know about local writes, enable the change webhook: every committed mutation
fires one HTTP request, keyed by verb.

```ts
const db = await openDB('app.db', {
  webhook: {
    enabled: true,
    endpoint: 'https://api.example.com/taladb',
    headers: { Authorization: `Bearer ${token}` },
    exclude_fields: ['embedding'],   // keep 768-float vectors out of every payload
  },
})
```

| Mutation | Method | Body |
|---|---|---|
| `insert`, `insertMany` | `POST` | `{ event_id, collection, id, document, timestamp }` |
| `updateOne`, `updateMany` | `PUT` | `{ event_id, collection, id, document, timestamp }` |
| `deleteOne`, `deleteMany` | `DELETE` | `{ event_id, collection, id, document, timestamp }` |

Delivery happens after the commit, on a bounded in-memory queue that **drops
rather than blocking** — a slow endpoint can never stall a write. Failed
deliveries retry three times with backoff on 5xx and network errors; a 4xx is
permanent and is not retried. Events for one document always arrive in order.

Delivery is **best effort**: a crash can lose queued events and a lost response
can cause a retry. Retries reuse `event_id` and `Idempotency-Key`, which the
receiver should deduplicate. This is a notification channel, not a replication log.

Delivery is implemented once in the `taladb` client on top of `fetch`, above the
platform bindings, so Node.js, the browser, and React Native behave identically.
See the [Change Webhook reference](/api/webhook).

## CLI dev tools

Download the pre-built `taladb-cli` binary for your platform from the [GitHub Releases page](https://github.com/taladb/taladb/releases):

```bash
taladb inspect myapp.db          # show collections, document counts, index names
taladb export myapp.db           # dump all documents as JSON
taladb import myapp.db data.json # bulk-import from JSON
taladb count myapp.db users      # count documents in a collection
taladb drop myapp.db sessions    # drop a collection
```

## Snapshot export / import

```ts
import init, { TalaDbWeb } from '@taladb/web'

await init()
const raw = TalaDbWeb.open('myapp.db')

// Export the entire database to a portable binary snapshot
const bytes = raw.exportSnapshot()

// Restore by opening with the bytes
const restored = TalaDbWeb.openWithSnapshot('myapp.db', bytes)
```

Available on the raw `@taladb/web` handle and in the Rust core. It is **not**
exposed by the `taladb` wrapper that `openDB()` returns, nor by the Node
binding — on Node, copy the database file instead.

Snapshots use a compact binary format (`TDBS` magic + version + length-prefixed KV pairs) and include every table — documents and indexes — so the restored database is immediately queryable without re-indexing.
