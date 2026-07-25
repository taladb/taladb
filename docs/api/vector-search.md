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
| `indexType` | `'flat' \| 'hnsw'` | `'flat'` | Exact scan, or approximate HNSW (Node.js only). |
| `hnswM` | `number` | `16` | HNSW connectivity. Higher = better recall, more memory. |
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

## Exact by default, HNSW when you need it

The default `flat` index is an **exact** scan over every vector — no
approximation, no recall trade-off. It is the right default for most on-device
corpora and is what ships on the browser and React Native.

On Node.js you can opt into an approximate **HNSW** graph for large corpora:

```ts
await articles.createVectorIndex('embedding', {
  dimensions: 384,
  indexType: 'hnsw',
})

// The graph is built at creation time and NOT updated by later writes —
// rebuild after a bulk ingest (e.g. during an idle period):
await articles.upgradeVectorIndex('embedding')
```

Measured on the [benchmarks page](/benchmarks): 14.6 ms vs 188 ms flat at
50k × 384-dim vectors, with 100% recall@10 on clustered (embedding-like) data.
Two caveats: graph construction is CPU-intensive (a one-off cost that grows
with collection size), and recall depends on your data's structure — measure on
your own embeddings.

### `dropVectorIndex(field)` / `upgradeVectorIndex(field)`

```ts
await articles.dropVectorIndex('embedding')      // remove the index and its vectors
await articles.upgradeVectorIndex('embedding')   // rebuild the HNSW graph (Node.js; no-op on flat)
```

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
