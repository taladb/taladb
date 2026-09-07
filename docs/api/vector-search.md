---
title: Vector Search
description: On-device vector similarity search in TalaDB — createVectorIndex, findNearest, metadata pre-filtering, exact k-NN and optional HNSW, and pairing with client-side embedding models.
---

# Vector Search

TalaDB stores vector embeddings alongside your documents and searches them
on-device — no vector database service, no API key, no data leaving the
device. It is the first embedded JavaScript database to combine document
queries with native vector similarity search across the browser, Node.js, and
React Native.

The typical flow: generate an embedding with an on-device model, store it on a
document, then rank documents by similarity to a query embedding — optionally
filtered by metadata first.

## `createVectorIndex(field, options)`

Register a vector index on a numeric-array field. Existing documents are
backfilled automatically; later inserts and updates maintain the index in the
same atomic transaction as the document write.

```ts
createVectorIndex(
  field: keyof Omit<T, '_id'> & string,
  options: VectorIndexOptions,
): Promise<void>
```

```ts
await articles.createVectorIndex('embedding', { dimensions: 384 })

// Explicit metric (default is cosine)
await articles.createVectorIndex('embedding', {
  dimensions: 1536,
  metric: 'cosine', // 'cosine' | 'dot' | 'euclidean'
})
```

**`VectorIndexOptions`:**

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `dimensions` | `number` | — | Required. Enforced on every insert and query. |
| `metric` | `'cosine' \| 'dot' \| 'euclidean'` | `'cosine'` | Similarity metric. |
| `indexType` | `'flat' \| 'hnsw'` | `'flat'` | Exact scan, or approximate HNSW (browser, Node.js, React Native). |
| `hnswM` | `number` | `32` | HNSW connectivity, 2–128. |
| `quantization` | `'none' \| 'scalar' \| 'binary'` | `'none'` | Compress graph vectors; exact originals remain stored. |
| `hnswEfConstruction` | `number` | `200` | HNSW build-time quality. |

## `findNearest(field, vector, topK, filter?)`

Return the `topK` documents most similar to `vector`, most similar first.

```ts
findNearest(
  field: keyof Omit<T, '_id'> & string,
  vector: number[],
  topK: number,
  filter?: Filter<T>,
): Promise<VectorSearchResult<T>[]>
```

```ts
const query = await embed('how do I reset my password?')
const results = await articles.findNearest('embedding', query, 5)
// [{ document: Article, score: 0.94 }, { document: Article, score: 0.91 }, ...]
```

Score range depends on the metric:

- `cosine` — [-1, 1]; identical vectors score `1.0`
- `dot` — unbounded; depends on vector magnitude
- `euclidean` — (0, 1]; identical vectors score `1.0`

### Filtered vector search — narrow, then rank

Pass a metadata filter as the fourth argument. It is resolved **before**
ranking, so `topK` is `k` documents that actually match — not a post-filter
that quietly returns three rows because the other seven were the wrong locale.

```ts
// The 5 most similar english-language support articles
const results = await articles.findNearest('embedding', query, 5, {
  locale: 'en',
  category: 'support',
  published: true,
})
```

The filter accepts any operator supported by `find` — `$and`, `$or`, `$in`,
`$gt`, `$exists`, etc. This is the pattern cloud vector databases (Qdrant,
Weaviate, Pinecone) charge for, running entirely on-device with no network
latency.

**Errors:**

- `VectorIndexNotFound` — no vector index exists on `field`
- `VectorDimensionMismatch` — `vector.length` ≠ the index's configured `dimensions`

## Persistent HNSW on browser, React Native and Node

Flat indexes use exact scanning. HNSW nodes and links are persisted in the same database as documents and updated in the same transaction as embedding writes. Reopening a database requires no graph rebuild. Metadata-only updates leave the graph unchanged.

```ts
await articles.createVectorIndex('embedding', {
  dimensions: 384,
  indexType: 'hnsw',
  hnswM: 16,
  hnswEfConstruction: 200,
  quantization: 'scalar',
})
```

HNSW supports cosine and euclidean metrics. Dot product remains available through exact indexes. Binary quantization requires cosine; its quality depends strongly on the embedding model. Scalar and binary codes reduce the graph's vector payload by approximately 4× and 32× respectively, excluding headers and edges. Original vectors stay in the database for exact rescoring, so these are not total storage or RAM reduction guarantees.

## Search controls and execution details

```ts
const result = await articles.searchVectors('embedding', queryVector, 10,
  { category: 'memory' },
  { mode: 'ann', efSearch: 200, scoreThreshold: 0.85,
    groupBy: 'parentId', groupSize: 1, offset: 0 })

console.log(result.execution) // path, reason, revision, effective efSearch, distanceComputations
console.log(result.hits)      // { document, score }[]
```

`mode` is `auto` (default), `exact`, or `ann`. Auto uses a ready HNSW index for unfiltered queries and exact search under filters. Explicit ANN errors if the graph is unavailable or stale. Filtered ANN traverses nonmatching nodes as routing bridges and returns only matches; it does not promise exact recall or use filter-specific precomputed edges.

`efSearch` defaults to 100. The effective candidate count is at least `(offset + topK) * oversampling`; oversampling defaults to 4 and accepts 1–100. Grouped ANN expands the pool when necessary. Every returned ANN score is recomputed from the original f32 vector in the same read snapshot as the filter and document.

Grouping retains the highest scoring `groupSize` hits per field value before pagination. Missing and null group values form one group. `scoreThreshold` uses the index metric's similarity score, inclusive. `offset` and `nextOffset` support pagination over live queries; writes between pages can change ordering. ANN pages are approximate and increasing the candidate pool may change earlier rankings; use exact mode when stable ranking on unchanged data matters.

The existing `findNearest` accepts these controls as an optional fifth argument and still returns a hit array. To retrieve every result meeting a threshold, use exact range search:

```ts
const matches = await articles.findWithin('embedding', queryVector, 0.85)
```

## Index status and resumable rebuilds

```ts
const status = await articles.vectorIndexStatus('embedding')
// state: flat | ready | stale | rebuildRequired
// indexedVectors, totalVectors, deletedNodes, revision, indexRevision,
// options, persistent, and current/last build progress

const controller = new AbortController()
await articles.rebuildVectorIndex('embedding', {
  m: 16, quantization: 'binary', batchSize: 32,
  signal: controller.signal,
  onProgress: p => console.log(p.processed, p.total, p.state),
})
```

Rebuilding compacts tombstones and can change graph settings or promote a flat index. Batches run off the JS thread on Node and React Native, and in the browser worker. Cancellation is cooperative between batches (1–1024 vectors; default 32), not an interruption of an individual insertion. Rebuilds keep the active graph available and publish the replacement atomically. Embedding mutations during a rebuild cause it to fail instead of publishing stale data; retry when ingestion is idle. Metadata-only changes are allowed.

For explicit resume after a process restart, use `beginVectorBuild(field, options)`, `stepVectorBuild(field, buildId, batchSize)` and `cancelVectorBuild(field, buildId)`. The status response includes the build ID and progress. Only one staged build per field may run at a time. Cancellation preserves the active graph; the storage compactor can reclaim freed pages later.

`upgradeVectorIndex(field)` now promotes flat/legacy indexes and rebuilds existing HNSW graphs. `dropVectorIndex(field)` removes both flat and graph records while retaining documents. Old HNSW metadata opens in `rebuildRequired` state and exact search remains available until promotion/rebuild.

## Measure recall on your embeddings

```ts
const report = await articles.measureVectorRecall('embedding', sampleQueries, 10,
  undefined, { efSearch: 200 })
// recallAtK, queries, topK, exactMs, annMs
```

Measurement compares ANN with exact top-k using the same database snapshot. Use representative query vectors; this is an explicit evaluation operation, not automatic telemetry. Graph construction and selective filtered ANN can be expensive on a phone, so measure with your target devices and workload.

## Pairing with on-device embedding models

TalaDB is the storage-and-search half of an on-device AI stack. Any model that
returns a `number[]` works — transformers.js and ONNX Runtime Web in the
browser, native models on mobile.

```ts
import { pipeline } from '@xenova/transformers'

const embedder = await pipeline('feature-extraction', 'Xenova/all-MiniLM-L6-v2')
const embed = async (text: string): Promise<number[]> => {
  const out = await embedder(text, { pooling: 'mean', normalize: true })
  return Array.from(out.data)
}

// Store a document with its embedding
await articles.insert({ ...article, embedding: await embed(article.body) })

// Search later
const results = await articles.findNearest('embedding', await embed(query), 5)
```

No cloud API key. No rate limit. No round-trip.

## When to reach for hybrid search

Pure vector search misses exact identifiers, SKUs, and rare proper nouns that a
query shares verbatim with a document. For production retrieval — the kind that
feeds a RAG prompt — combine vector similarity with BM25 keyword ranking using
[`hybridSearch`](/api/search#hybrid-search). It is usually the better default
for user-facing search.
