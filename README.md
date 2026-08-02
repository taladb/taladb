<div align="center">

<img src=".github/assets/tala-db-banner.png" alt="TalaDB" width="800" />

**The embedded vector database for on-device AI.**<br/>
Documents + similarity search built in Rust — browser, Node.js, and React Native. Runs where your model runs. No cloud. No compromise.

[![npm](https://img.shields.io/npm/v/taladb?label=npm)](https://www.npmjs.com/package/taladb)
[![Status: Stable](https://img.shields.io/badge/Status-Stable-green)](https://github.com/taladb/taladb)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-2024_Edition-orange?logo=rust)](https://www.rust-lang.org)
[![WASM](https://img.shields.io/badge/WASM-wasm--bindgen-purple?logo=webassembly)](https://rustwasm.github.io/wasm-bindgen/)
[![Platform](https://img.shields.io/badge/Platform-Browser%20%7C%20React%20Native%20%7C%20Node.js-green)](https://github.com/taladb/taladb)
[![Sponsor](https://img.shields.io/badge/Sponsor-taladb-red?logo=github-sponsors)](https://github.com/sponsors/taladb)

**[Documentation](https://taladb.dev) · [Web Demo](https://taladb-playground.vercel.app/) · [Mobile Demo](https://appetize.io/app/b_ugmjhjghdkgnjux4lzkepvsfma) · [Web Guide](https://taladb.dev/guide/web) · [Node.js Guide](https://taladb.dev/guide/node) · [React Native Guide](https://taladb.dev/guide/react-native)**

</div>


---

AI inference is moving onto the device — transformers.js and ONNX Runtime Web in the browser, Core ML and ExecuTorch on mobile. The model runs locally, but the *retrieval* layer usually doesn't: embeddings get shipped to a hosted vector database, which puts back the latency, the per-query cost, and the privacy exposure that running locally was supposed to remove.

TalaDB is the other half. Structured queries, vector similarity search, and offline-first storage in a single embedded database that runs entirely on the user's device — the same Rust core in the browser, React Native, and Node.js, behind one TypeScript API.

## Why TalaDB?

|  | TalaDB | RxDB / Dexie | Expo SQLite | LanceDB |
|---|---|---|---|---|
| Runs in browser | ✓ | ✓ | ✗ | ✗ |
| React Native | ✓ | ✗ | ✓ | ✗ |
| On-device vector search | ✓ | ✗ | ✗ | ✓ |
| Unified API across runtimes | ✓ | ✗ | ✗ | ✗ |
| No cloud required | ✓ | ✓ | ✓ | ✗ |
| Rust core | ✓ | ✗ | ✗ | ✓ |

*The only embedded database with vector search that runs on all three JS runtimes with a single API.*

The same Rust core powers all three runtimes:

| Runtime | Package | Mechanism |
|---|---|---|
| Browser | `@taladb/web` | `wasm-bindgen` + OPFS via SharedWorker |
| Node.js | `@taladb/node` | `napi-rs` native module |
| React Native | `@taladb/react-native` | JSI HostObject (C FFI via `cbindgen`) |

Application code uses the unified `taladb` package with a single TypeScript API on every platform.

## Highlights

- **Vector search** — exact k-NN by default (cosine, dot, euclidean) with no recall trade-off, plus optional HNSW when scale demands it; pairs directly with on-device embedding models (transformers.js, ONNX Runtime Web)
- **Full-text search** — BM25-ranked keyword search (`searchText`), the same relevance model Elasticsearch and Lucene use, running on-device
- **Hybrid search** — fuse keyword and vector rankings with reciprocal rank fusion (`hybridSearch`), so an exact SKU and a semantic paraphrase both surface from one query
- **Filtered similarity search** — narrow by metadata *before* ranking, in one call: the 5 most semantically similar *english-language support articles*, without two round-trips or a post-filter that silently drops your top-k
- **MongoDB-like API** — familiar filter and update DSL, fully typed with TypeScript generics
- **ACID transactions** — powered by [redb](https://github.com/cberner/redb), a pure-Rust B-tree storage engine
- **Live queries** — subscribe to a filter and receive snapshots after every write, no polling

\+ encryption at rest, schema migrations, snapshot export/import, CLI tools.

## Performance

Measured with the reproducible suites in [`scripts/`](scripts/) (`pnpm bench:web` for the browser, `pnpm bench` for Node.js) on a **2018 MacBook Pro** (Intel i5-8259U, 8 GB) — deliberately modest hardware; treat these as a floor. Node.js results are from TalaDB v0.10.2; browser vector results are from v0.9.4 and have not yet been re-run against the v0.10.2 engine, so they understate current performance. File-backed / OPFS, medians after warmup.

**Browser (WASM + OPFS)** — the flagship runtime, measured in headless Chrome (every timing includes the worker round-trip):

| Operation | Scale | Result |
|---|---|---|
| `findNearest` (384-dim, exact k-NN) | 10k vectors | **17 ms** |
| `findNearest` (384-dim, exact k-NN) | 50k vectors | **85 ms** |
| Filtered vector search (metadata + rank) | 50k vectors | **123 ms** |
| `findOne` by `_id` | 100k docs | **100 µs** |
| `find` on indexed field | 100k docs | **300 µs** |
| Bulk ingest (`insertMany`) | batches of 5k | **~57k docs/s** |

**Node.js (native)** — file-backed, `fsync`-durable per write:

| Operation | Scale | Result |
|---|---|---|
| `findNearest` (384-dim, exact k-NN) | 10k / 100k vectors | **7.7 ms / 65 ms** |
| Filtered vector search (metadata + rank) | 100k vectors | **171 ms** |
| `findOne` by `_id` | 100k docs | **25 µs** |
| `find` on indexed field | 100k docs | **174 µs** |
| `find`, two-sided range (`$gte`+`$lt`) | 100k docs | **1.4 ms** |
| `find`, unindexed field (full scan) | 100k docs | **317 ms** |
| Bulk ingest (`insertMany`) | batches of 5k | **~39k docs/s** |

Vector search is exact by default — no approximation, no recall trade-off — with an optional HNSW index on Node.js. **0.10.2 cut exact vector search by ~63%** at 100k vectors (174 ms → 65 ms) by scoring each candidate in a single pass with the query norm hoisted out of the loop, and made the query engine resolve a storage table once per batch instead of once per key — worth **−30% on full scans**, **−32% on unindexed `count`**, and **+17% on bulk ingest**. `findOne` and bounded `find` no longer materialise the whole result set before discarding it, so an unindexed `findOne` is now microseconds rather than a full scan. Full tables, methodology, and tuning notes: **[taladb.dev/benchmarks](https://taladb.dev/benchmarks)**.

## Usage

### Install

Every app installs the unified **`taladb`** package plus **one runtime binding** for its platform. Everything else is optional.

**Web app (browser)**

```bash
pnpm add taladb @taladb/web          # required
pnpm add @taladb/react               # optional — React hooks (useFind, useFindOne, …)
```

**Mobile app (React Native / Expo)**

```bash
pnpm add taladb @taladb/react-native # required
pnpm add @taladb/react               # optional — the same hooks work in React Native
```

**Node.js (server / scripts)**

```bash
pnpm add taladb @taladb/node                 # required
pnpm add @taladb/sync-mongodb mongodb        # optional — sync to MongoDB (server-side only)
```

| Package | Web | Mobile (RN) | Node | Role |
|---|:--:|:--:|:--:|---|
| `taladb` | ✅ required | ✅ required | ✅ required | Unified API |
| `@taladb/web` | ✅ required | — | — | Browser WASM binding |
| `@taladb/react-native` | — | ✅ required | — | React Native (JSI) binding |
| `@taladb/node` | — | — | ✅ required | Node.js native binding |
| `@taladb/react` | ⭕ optional | ⭕ optional | — | React / React Native hooks |
| `@taladb/sync-mongodb` | — | — | ⭕ optional | MongoDB sync ([server-side only](https://taladb.dev/guide/bidirectional-sync#mongodb-adapter)) |
| `@taladb/cloudflare` | — | — | ⭕ optional | Cloudflare Workers deploy target |

Bidirectional HTTP sync (`HttpSyncAdapter`) ships inside `taladb` — no extra install. Database sync adapters like `@taladb/sync-mongodb` hold DB credentials and run **server-side only**; a web or mobile app syncs through your own API, never a direct database connection.

### Quick start

```ts
import { openDB } from 'taladb'

const db = await openDB('myapp.db')  // OPFS in browser, file on Node.js / React Native
```

### As a document database

```ts
interface Article {
  _id?: string
  title: string
  category: string
  locale: string
  publishedAt: number
}

const articles = db.collection<Article>('articles')

// Insert
const id = await articles.insert({
  title: 'How to reset your password',
  category: 'support',
  locale: 'en',
  publishedAt: Date.now(),
})

// Query with filters
const results = await articles.find({
  category: 'support',
  locale: 'en',
  publishedAt: { $gte: Date.now() - 86_400_000 },
})

// Update
await articles.updateOne({ _id: id }, { $set: { title: 'Reset your password' } })

// Delete
await articles.deleteOne({ _id: id })

// Secondary index for fast lookups
await articles.createIndex('category')
await articles.createIndex('publishedAt')
```

---

### As a vector database

```ts
import { pipeline } from '@xenova/transformers'

// Any on-device embedding model works
const embedder = await pipeline('feature-extraction', 'Xenova/all-MiniLM-L6-v2')
const embed = async (text: string) => {
  const out = await embedder(text, { pooling: 'mean', normalize: true })
  return Array.from(out.data) as number[]
}

// 1. Create the vector index once (backfills existing documents automatically)
await articles.createVectorIndex('embedding', { dimensions: 384 })

// 2. Insert documents with their embeddings
await articles.insert({
  title: 'How to reset your password',
  category: 'support',
  locale: 'en',
  publishedAt: Date.now(),
  embedding: await embed('How to reset your password'),
})

// 3. Semantic search — find the 5 most similar articles
const query = await embed('forgot my login credentials')
const results = await articles.findNearest('embedding', query, 5)

results.forEach(({ document, score }) => {
  console.log(score.toFixed(3), document.title)
})
// 0.941  How to reset your password
// 0.887  Account recovery options
// 0.823  Two-factor authentication setup
```

---

### Filtered vector search — narrow, then rank

Metadata filter is applied **before** ranking, so your top-k is k results that actually match — not a post-filter that quietly returns three rows because the other seven were the wrong locale.

```ts
// "Find the 5 most relevant english support articles for this query"
const results = await articles.findNearest('embedding', query, 5, {
  category: 'support',
  locale: 'en',
})

// Works across all runtimes — browser, React Native, Node.js
// Data never leaves the device
```

---

### Full-text search — BM25 ranking

Keyword search that ranks by relevance, not just presence. Create an FTS index,
then `searchText` returns the best matches first — the same BM25 model Lucene
and Elasticsearch use, running on-device.

```ts
await articles.createFtsIndex('body')

const hits = await articles.searchText('body', 'reset my password', 5)
hits.forEach(({ document, score }) => console.log(score.toFixed(2), document.title))
// 3.14  How to reset your password
// 1.87  Account recovery options
```

Unlike the `$contains` filter — which requires *every* token — `searchText` uses
OR semantics: matching more of the query simply scores higher. Pass a filter to
scope the search, and `{ k1, b }` to tune term saturation and length
normalisation.

---

### Hybrid search — keyword + vector, fused

Keyword search misses paraphrases; vector search misses exact identifiers, SKUs,
and rare proper nouns. `hybridSearch` runs both and fuses the rankings with
[reciprocal rank fusion](https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf),
so a document both retrievers like beats one only a single retriever found — the
standard recipe for RAG retrieval, entirely on-device.

```ts
await articles.createFtsIndex('body')
await articles.createVectorIndex('embedding', { dimensions: 384 })

const results = await articles.hybridSearch(
  { textField: 'body',      text: 'how do I get my money back' },
  { vectorField: 'embedding', vector: await embed('how do I get my money back') },
  5,
)

results.forEach(({ document, score, textRank, vectorRank }) => {
  // textRank / vectorRank show which retriever found each hit (null = missed)
  console.log(document.title, { textRank, vectorRank })
})
```

Fusion works on *ranks*, not scores, so no fragile normalisation between
unbounded BM25 and cosine ∈ [-1, 1]. A metadata filter applies to both
retrievers; `{ rrfK, textWeight, vectorWeight, candidates }` tune the fusion.

---

### Live queries

```ts
// Subscribe to changes — callback fires after every matching write
const unsub = articles.subscribe({ category: 'support' }, (docs) => {
  console.log('support articles updated:', docs.length)
})

// Stop listening
unsub()
```

## Documentation

Full documentation is at **[taladb.dev](https://taladb.dev)**.

| Section | Link |
|---|---|
| Introduction & architecture | [/introduction](https://taladb.dev/introduction) |
| Core concepts | [/concepts](https://taladb.dev/concepts) |
| Feature overview | [/features](https://taladb.dev/features) |
| Benchmarks | [/benchmarks](https://taladb.dev/benchmarks) |
| Web (Browser / WASM) guide | [/guide/web](https://taladb.dev/guide/web) |
| Node.js guide | [/guide/node](https://taladb.dev/guide/node) |
| React Native guide | [/guide/react-native](https://taladb.dev/guide/react-native) |
| Collection API | [/api/collection](https://taladb.dev/api/collection) |
| Filters | [/api/filters](https://taladb.dev/api/filters) |
| Updates | [/api/updates](https://taladb.dev/api/updates) |
| Migrations | [/api/migrations](https://taladb.dev/api/migrations) |
| Encryption | [/api/encryption](https://taladb.dev/api/encryption) |
| Live queries | [/api/live-queries](https://taladb.dev/api/live-queries) |

## Development

### Prerequisites

- [Rust](https://rustup.rs/) stable 1.75+
- [wasm-pack](https://rustwasm.github.io/wasm-pack/) — for browser builds
- [Node.js](https://nodejs.org/) 18+ and [pnpm](https://pnpm.io/) 9+
- `@napi-rs/cli` — for Node.js native module builds

### Running tests

```bash
# Rust unit + integration tests
cargo test --workspace

# TypeScript tests
pnpm --filter taladb test

# Browser WASM tests (requires Chrome)
wasm-pack test packages/@taladb/web --headless --chrome
```

### Building

```bash
# Browser WASM
pnpm --filter @taladb/web build

# Node.js native module
pnpm --filter @taladb/node build

# TypeScript package
pnpm --filter taladb build

# All packages
pnpm build
```

### Local docs

```bash
pnpm docs:dev     # dev server at http://localhost:5173
pnpm docs:build   # production build
pnpm docs:preview # preview production build
```

## Contributing

Bug reports, PRs, and feedback are all welcome.

1. Fork the repo and create a branch: `git checkout -b feat/my-feature`
2. Make your changes and add tests
3. Run `cargo test --workspace` and `pnpm --filter taladb test`
4. Open a pull request with a clear description

Open an issue before large features or architectural changes. See [CONTRIBUTING.md](CONTRIBUTING.md) for the full development workflow.

Reach out: [dennis@thinkgrid.dev](mailto:dennis@thinkgrid.dev)

## Support TalaDB

TalaDB is free and open-source, maintained by one person. If it saves you time, [sponsoring on GitHub](https://github.com/sponsors/taladb) directly funds continued development: new runtimes, query operators, and performance work.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

Copyright © [taladb](https://github.com/taladb)

---

<div align="center">

Documents + vectors, on-device. No cloud. · [taladb.dev](https://taladb.dev)

</div>
