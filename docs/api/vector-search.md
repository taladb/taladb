---
title: Vector Search
description: On-device vector similarity search in TalaDB — createVectorIndex, findNearest, metadata pre-filtering, exact k-NN and optional HNSW, and pairing with client-side embedding models.
---

# Vector Search

TalaDB stores vector embeddings alongside your documents and searches them
on-device — no vector database service, no API key, no data leaving the
device. Document queries and vector similarity search run in the same engine,
on every platform TalaDB supports.

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

Search traversal keeps the query at full precision, including when stored nodes
use binary codes. This preserves query component magnitudes during candidate
selection; exact rescoring then ranks the candidates using their original
vectors. Existing binary graphs use this search behavior without a rebuild.

Binary graph construction compares packed sign codes directly during neighbour
selection and link pruning, avoiding repeated expansion into float vectors.
This applies to initial builds, resumable rebuilds and graph updates on writes.
It preserves similarity scores and the stored graph format.

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

Indexed scalar equalities, ranges and `$in` predicates resolve matching IDs directly from index keys. This also applies to covered AND/OR branches and equalities on every field of a compound index. Array elements keep the same matching semantics as `find`. Predicates such as negation, existence and regex still require document fields; indexed conditions narrow those reads when possible.

AND filters use bounded previews of up to 64 keys per indexed branch to choose a small candidate set. When cheaper than enumerating a broad equality or `$in` range, remaining candidates are checked with batched index point lookups. Broad branches retain only IDs in the current intersection. These previews are estimates; they do not guarantee the cheapest plan when every branch exceeds the preview limit.

Multiple comparisons on a field share narrower scans only when transactional index metadata confirms that the field has no array-valued documents. Array fields keep independent comparisons because different elements can satisfy each bound. Indexes created or rebuilt with this release have that metadata. Older indexes without it stay conservative; dropping and recreating an index enables scalar narrowing. Residual predicates still require document fields.

Vector filter execution retains at most 256 IDs per intermediate branch or union for exact search, and one ID per KiB of the current search budget (at least 256) for ANN, so memory pressure lowers the ANN limit too. Nested branches under a selected AND seed stop earlier, at 16 IDs per seed ID. Larger matches stream from unique index keys when covered; array unions, residual predicates and other shapes stream document projections instead. Residual AND predicates retain an available unique index seed. Text predicates use bounded posting-list previews before projected document checks. Exact search reuses an existing decoded-vector block or reads bounded batches from that stream. Nearby IDs use short ordered scans; scattered IDs fall back to point reads after a bounded scan. Smaller filters keep the sparse point-read or dense scan/cache path. Filtered searches do not populate the full decoded-vector cache. Filter keys, vectors and returned documents all come from the same read snapshot.

Large filtered ANN queries use a compact eligibility bitmap indexed by graph-node ordinal. Building it reads one document-to-node mapping per match, which is why moderate filters keep the ID set. The bitmap counts toward the active graph reservation, leaving less allowance for decoded nodes and traversal buffers. Graphs with many historical ordinals can need a larger bitmap; if it cannot fit, search falls back to exact with `execution.reason: 'memoryBudget'`.

`efSearch` defaults to 100. The effective candidate count is at least `(offset + topK) * oversampling`; oversampling defaults to 4 and accepts 1–100. Grouped ANN expands the pool when necessary. Every returned ANN score is recomputed from the original f32 vector in the same read snapshot as the filter and document.

Grouping retains the highest scoring `groupSize` hits per field value before pagination. Ranking keeps at most `offset + topK + 1` candidates and their group keys, rather than retaining every group or loading every candidate's full document. Only top-level fields needed for grouping are decoded for competitive candidates; a nested group field retains its parent value. Full documents are loaded for the retained window. Large offsets still increase memory and work. Missing and null group values form one group. `scoreThreshold` uses the index metric's similarity score, inclusive. `offset` and `nextOffset` support pagination over live queries; writes between pages can change ordering. ANN pages are approximate and increasing the candidate pool may change earlier rankings; use exact mode when stable ranking on unchanged data matters.

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

Build steps reuse decoded graph nodes within the database's shared retained
cache budget. Reuse is best effort: eviction or a process restart reloads nodes
from storage without losing committed progress. A zero cache budget disables
retention. Failed transactions discard their cached edits; cancelled or failed
builds release their partial graph cache. Batch size limits the number of
insertions per step, but does not impose a fixed time or process memory limit.

`upgradeVectorIndex(field)` now promotes flat/legacy indexes and rebuilds existing HNSW graphs. `dropVectorIndex(field)` removes both flat and graph records while retaining documents. Old HNSW metadata opens in `rebuildRequired` state and exact search remains available until promotion/rebuild.

## Measure recall on your embeddings

```ts
const report = await articles.measureVectorRecall('embedding', sampleQueries, 10,
  undefined, { efSearch: 200 })
// recallAtK, queries, topK, exactMs, annMs
```

Measurement compares ANN with exact top-k using the same database snapshot. Use representative query vectors; this is an explicit evaluation operation, not automatic telemetry. Graph construction and selective filtered ANN can be expensive on a phone, so measure with your target devices and workload.

## Search memory budget

Exact vectors, decoded graph nodes and active ANN traversal buffers share one
estimated memory allowance per database. With a host memory hint, adaptive sizing
uses 1/512 of that hint, clamped to 1–16 MiB on Android, iOS and WASM, or
1–64 MiB on other native targets. Without a hint, the fallback is 8 MiB on
Android, iOS and WASM, or 64 MiB elsewhere. These are conservative starting
values per database; account for other databases, embedding models and storage
in the application's total budget.

The browser worker reads approximate device RAM from
[`navigator.deviceMemory`](https://www.w3.org/TR/device-memory/) when available.
This is a capability hint, not currently free RAM. Other hosts, and browsers
without that API, can supply `vector_cache_memory_bytes` in the opening config.
An explicit `vector_cache_bytes` takes precedence over hints.
Configure a budget for your application's available memory when opening Node
or browser databases:

```ts
const db = await openDB('articles', {
  config: { vector_cache_bytes: 16 * 1024 * 1024 },
})
```

React Native accepts the same field in its initialization config JSON. C FFI
callers can pass it to `taladb_open_with_config`. Rust applications can call
`db.set_vector_cache_budget(bytes)` and inspect `db.vector_cache_stats()`.
Zero disables retention. A shared 64 KiB minimum workspace still supports short
ANN walks when the configured budget is smaller. Existing collection handles
observe budget changes; active loans finish under their original allowance.
Reducing the budget prevents new admissions until those loans return.

Cache controls on any collection apply to its whole database:

```ts
await articles.setVectorCacheAdaptive(2 * 1024 ** 3) // 2 GiB hint → 4 MiB baseline
await articles.notifyMemoryPressure('moderate')     // one quarter of baseline
await articles.notifyMemoryPressure('critical')     // evict retained caches
await articles.notifyMemoryPressure('normal')       // explicit recovery
await articles.setVectorCacheBudget(8 * 1024 ** 2)   // switch to a fixed baseline
const stats = await articles.vectorCacheStats()
```

Pressure applies to fixed and adaptive baselines. Critical pressure disables
retention and keeps the shared 64 KiB workspace; larger walks fall back to exact.
Changing the baseline while pressure is active keeps that pressure reduction.
Repeated identical signals do not evict caches again. Recovery is explicit:
neither elapsed time nor returning to the foreground proves that memory is free.
`setVectorCacheAdaptive()` without a hint restores the platform fallback.
Rust hosts use `set_vector_cache_adaptive(Some(memory_bytes))` and
`notify_memory_pressure(MemoryPressure::Critical)`.

Applications forward platform signals; TalaDB does not poll process memory or
install native OS listeners. For example, React Native exposes
[`AppState.memoryWarning` on iOS](https://reactnative.dev/docs/appstate#memorywarning).
Register once per database, handle errors, and remove the listener on cleanup:

```ts
const subscription = AppState.addEventListener('memoryWarning', () => {
  articles.notifyMemoryPressure('critical').catch(reportError)
})
// On teardown:
subscription.remove()
```

Android hosts can forward trim-memory signals through the same command API.
Browser hosts should forward only signals they actually have; a hidden tab
alone does not indicate memory pressure. No automatic recovery is inferred.

Retained caches, pinned exact-vector blocks, full-vector cache construction and
concurrent graph loans share this allowance. A walk reserves at most three
quarters of what is free, so walks that start while it runs still get room; its
node cache evicts within that share. ANN uses a compact visited bitset
and checks buffer growth before allocating. If a walk cannot fit, even when
`mode: 'ann'` was requested, search switches to exact on the same snapshot.
`execution.path` is `'exact'` and `execution.reason` is `'memoryBudget'`. Filters,
thresholds, grouping and pagination still apply; exact ranking can differ from
approximate ranking, and the fallback can take longer.

Rust cache statistics, also available through the binding command
`{ op: 'cacheStats' }`, include `retainedBytes`, `activeBytes`,
`memoryBudgetBytes` and `peakBytes` in JSON, plus `policy`, `memoryHintBytes`,
`pressure` and `baselineBudgetBytes`. `budgetBytes` is the effective allowance
under the current pressure. Active and peak bytes conservatively
charge the full reservation of a graph loan or vector-cache builder. They are
accounting estimates, not measured heap use or RSS. Peak resets when the budget
is set. Large-filter ANN bitmaps count toward the graph reservation. Capped
filter ID sets, page windows and group keys, result documents, transient record
decoding, rebuild scratch and storage-engine caches use additional memory.

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
