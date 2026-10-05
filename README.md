<div align="center">

<img src=".github/assets/tala-db-banner.png" alt="TalaDB" width="800" />

**An open-source embedded vector and document database for building local-first AI applications.**<br/>
Store documents, metadata, and vectors together. Query structured data and semantic similarity from one embedded database — across the browser, Node.js, React Native, and native Android (Kotlin) and iOS/macOS (Swift) apps. No cloud required.

[![npm](https://img.shields.io/npm/v/taladb?label=npm)](https://www.npmjs.com/package/taladb)
[![Status: Stable](https://img.shields.io/badge/Status-Stable-green)](https://github.com/taladb/taladb)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT_OR_Apache--2.0-blue.svg)](#license)
[![Rust](https://img.shields.io/badge/Rust-2024_Edition-orange?logo=rust)](https://www.rust-lang.org)
[![WASM](https://img.shields.io/badge/WASM-wasm--bindgen-purple?logo=webassembly)](https://rustwasm.github.io/wasm-bindgen/)
[![Platform](https://img.shields.io/badge/Platform-Browser%20%7C%20Node.js%20%7C%20React%20Native%20%7C%20Android%20%7C%20iOS-green)](https://github.com/taladb/taladb)
[![Sponsor](https://img.shields.io/badge/Sponsor-taladb-red?logo=github-sponsors)](https://github.com/sponsors/tala-sh)

**[Documentation](https://taladb.dev) · [Web Demo](https://demo-web.taladb.dev/) · [React Native Demo](https://play.google.com/store/apps/details?id=dev.thinkgrid.kepta) · [Android Demo](#) · [iOS Demo](#)**<br/>
**[Web Guide](https://taladb.dev/guide/web) · [Node.js Guide](https://taladb.dev/guide/node) · [React Native Guide](https://taladb.dev/guide/react-native) · [Kotlin Guide](https://taladb.dev/guide/android) · [Swift Guide](https://taladb.dev/guide/swift)**

</div>


---

AI inference is moving onto the device — transformers.js and ONNX Runtime Web in the browser, Core ML and ExecuTorch on mobile. The model runs locally, but the *retrieval* layer usually doesn't: embeddings get shipped to a hosted vector database, which puts back the latency, the per-query cost, and the privacy exposure that running locally was supposed to remove.

TalaDB combines a document database and a vector database in one embedded engine. Store JSON-like documents, query them with familiar filters, and run vector similarity search entirely on the user's device — with the same Rust core everywhere: one TypeScript API across the browser, Node.js, and React Native, plus native Kotlin and Swift packages for Android and iOS/macOS.

## Why TalaDB?

|  | TalaDB | RxDB | Dexie | Expo SQLite | LanceDB |
|---|---|---|---|---|---|
| Runs in browser | ✓ | ✓ | ✓ | Alpha | ✗ |
| React Native | ✓ | ✓ | ✗ | ✓ | ✗ |
| Native Android & iOS (Kotlin, Swift) | ✓ | ✗ | ✗ | ✗ | ✗ |
| Built-in vector index | ✓ | ✗ ¹ | ✗ | Via `sqlite-vec` | ✓ |
| Rust core | ✓ | ✗ | ✗ | ✗ | ✓ |

<sub>¹ RxDB ships distance helpers for building vector search yourself, not a vector index.</sub>

*One embedded engine for documents and vectors — from the browser to native mobile apps — with no server to run.*

The same Rust core powers every platform:

| Platform | Package | Mechanism | Status |
|---|---|---|---|
| Browser | `@taladb/web` | `wasm-bindgen` + OPFS via DedicatedWorker | Stable |
| Node.js | `@taladb/node` | `napi-rs` native module | Stable |
| React Native | `@taladb/react-native` | JSI HostObject (C FFI via `cbindgen`) | Stable |
| Android (Kotlin) | [`taladb-kotlin`](https://github.com/taladb/taladb-kotlin) · `dev.taladb:taladb-android` | JNI over the C FFI · [guide](https://taladb.dev/guide/android) | Early release |
| iOS & macOS (Swift) | [`taladb-swift`](https://github.com/taladb/taladb-swift) | SwiftPM over the C FFI · [guide](https://taladb.dev/guide/swift) | Early release |
| Rust | [`taladb`](https://crates.io/crates/taladb) | The engine crate itself · [guide](https://taladb.dev/guide/rust) | Early release |

On the web, Node.js, and React Native, application code uses the unified `taladb` package with a single TypeScript API. Kotlin, Swift, and Rust apps get idiomatic native APIs over the same engine, with the same JSON filters, vector and full-text search, and live queries.

Early-release packages may still change their APIs, and the Kotlin and Swift packages are not yet published to Maven Central or tagged for SwiftPM.

## Highlights

- **Vector search** — exact k-NN by default (cosine, dot, euclidean) with no recall trade-off, plus optional HNSW when scale demands it; pairs directly with on-device embedding models (transformers.js, ONNX Runtime Web)
- **Full-text search** — BM25-ranked keyword search (`searchText`), the same relevance model Elasticsearch and Lucene use, running on-device
- **Hybrid search** — fuse keyword and vector rankings with reciprocal rank fusion (`hybridSearch`), so an exact SKU and a semantic paraphrase both surface from one query
- **Filtered similarity search** — narrow by metadata *before* ranking, in one call: the 5 most semantically similar *english-language support articles*, without two round-trips or a post-filter that silently drops your top-k
- **MongoDB-like API** — familiar filter and update DSL, fully typed with TypeScript generics
- **ACID transactions** — powered by [redb](https://github.com/cberner/redb), a pure-Rust B-tree storage engine
- **Live queries** — subscribe to a filter and receive results after changes, with polling fallback

\+ encryption at rest, schema migrations, snapshot export/import, CLI tools.

## Usage

### Install

JavaScript apps (browser, Node.js, React Native) install the unified **`taladb`** package plus **one runtime binding** for their platform. Native Android, iOS/macOS and Rust apps use their own package instead — see [Native apps](#native-apps-kotlin-swift-rust) below.

#### JavaScript and TypeScript

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
```

| Package | Web | Mobile (RN) | Node | Role |
|---|:--:|:--:|:--:|---|
| `taladb` | ✅ required | ✅ required | ✅ required | Unified API |
| `@taladb/web` | ✅ required | — | — | Browser WASM binding |
| `@taladb/react-native` | — | ✅ required | — | React Native (JSI) binding |
| `@taladb/node` | — | — | ✅ required | Node.js native binding |
| `@taladb/react` | ⭕ optional | ⭕ optional | — | React / React Native hooks |

#### Native apps (Kotlin, Swift, Rust)

**Android (Kotlin)** — [`taladb-kotlin`](https://github.com/taladb/taladb-kotlin), `minSdk` 24+, Kotlin 2.2+

```kotlin
dependencies {
    implementation("dev.taladb:taladb-android:<version>")
}
```

**iOS & macOS (Swift)** — [`taladb-swift`](https://github.com/taladb/taladb-swift), iOS 13+ / macOS 10.15+, Swift 5.9+

```swift
.package(url: "https://github.com/taladb/taladb-swift", from: "<version>")
// target dependency: .product(name: "TalaDB", package: "taladb-swift")
```

**Rust** — the engine crate itself, Rust 1.90+

```bash
cargo add taladb
```

The Kotlin and Swift packages are not on Maven Central or tagged for SwiftPM yet; until they are, build them from source as described in the [Kotlin guide](https://taladb.dev/guide/android#build-from-source) and [Swift guide](https://taladb.dev/guide/swift#build-from-source). The examples below use the TypeScript API; each native guide has the same examples in Kotlin, Swift or Rust.

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

### Change webhook

TalaDB does not replicate. When a backend needs to know about local writes, every
committed mutation can fire one HTTP request — `POST` on insert, `PUT` on update,
`DELETE` on delete:

```ts
const db = await openDB('app.db', {
  webhook: {
    enabled: true,
    endpoint: 'https://api.example.com/taladb',
    headers: { Authorization: `Bearer ${token}` },
    exclude_fields: ['embedding'],   // keep 768-float vectors out of the payload
  },
})
```

Identical on the web, Node.js and React Native (webhooks are delivered by the
TypeScript client, so the Kotlin, Swift and Rust packages don't send them). Delivery is at most once — it is a notification
channel, not a replication log. See [/api/webhook](https://taladb.dev/api/webhook).

## Documentation

Full documentation is at **[taladb.dev](https://taladb.dev)**.

| Section | Link |
|---|---|
| Introduction & architecture | [/introduction](https://taladb.dev/introduction) |
| Core concepts | [/concepts](https://taladb.dev/concepts) |
| Feature overview | [/features](https://taladb.dev/features) |
| Web (Browser / WASM) guide | [/guide/web](https://taladb.dev/guide/web) |
| Node.js guide | [/guide/node](https://taladb.dev/guide/node) |
| React Native guide | [/guide/react-native](https://taladb.dev/guide/react-native) |
| Android (Kotlin) guide | [/guide/android](https://taladb.dev/guide/android) |
| iOS & macOS (Swift) guide | [/guide/swift](https://taladb.dev/guide/swift) |
| Rust guide | [/guide/rust](https://taladb.dev/guide/rust) |
| React hooks | [/guide/react](https://taladb.dev/guide/react) |
| CLI dev tools | [/guide/cli](https://taladb.dev/guide/cli) |
| Collection API | [/api/collection](https://taladb.dev/api/collection) |
| Filters | [/api/filters](https://taladb.dev/api/filters) |
| Updates | [/api/updates](https://taladb.dev/api/updates) |
| Vector search | [/api/vector-search](https://taladb.dev/api/vector-search) |
| Full-text & hybrid search | [/api/search](https://taladb.dev/api/search) |
| Migrations | [/api/migrations](https://taladb.dev/api/migrations) |
| Encryption | [/api/encryption](https://taladb.dev/api/encryption) |
| Live queries | [/api/live-queries](https://taladb.dev/api/live-queries) |
| Change webhook | [/api/webhook](https://taladb.dev/api/webhook) |

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

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option — the Rust ecosystem's convention, so downstream crates can
depend on TalaDB without a licence-compatibility review.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project shall be dual-licensed as above, without any
additional terms or conditions.

---

<div align="center">

One database for documents + vectors, on-device. · [taladb.dev](https://taladb.dev)

</div>
