---
title: Search (Full-Text & Hybrid)
description: On-device keyword and hybrid search in TalaDB — BM25 relevance ranking with searchText, reciprocal rank fusion with hybridSearch, the $contains filter, and RAG retrieval tuning.
---

# Search — Full-Text & Hybrid

TalaDB has three ways to find documents by text, from cheapest to most
powerful:

| API | What it does | When |
| --- | --- | --- |
| `$contains` filter | Boolean — every token must be present | Exact keyword gating inside a `find` |
| `searchText` | BM25 relevance ranking (OR semantics) | "Best matches first" keyword search |
| `hybridSearch` | Fuses BM25 + vector similarity | Production RAG retrieval |

All three require a full-text index on the field.

## `createFtsIndex(field)` / `dropFtsIndex(field)`

Build or drop an inverted token index over a string field. Existing documents
are backfilled on creation; later writes maintain it automatically.

```ts
await posts.createFtsIndex('body')
await posts.dropFtsIndex('body')
```

Tokenization is lowercase, splits on non-alphanumeric characters, and drops
single-character tokens.

## The `$contains` filter

Boolean full-text matching inside an ordinary `find` — a document matches when
its field contains **all** the supplied tokens.

```ts
const results = await posts.find({ body: { $contains: 'rust embedded database' } })
```

Use this when text is a gate on a larger query, not the thing you're ranking by.

## `searchText(field, query, topK, filter?, options?)`

Rank documents against a free-text query using [BM25](https://en.wikipedia.org/wiki/Okapi_BM25)
— the relevance model behind Lucene and Elasticsearch — most relevant first.

```ts
searchText(
  field: keyof Omit<T, '_id'> & string,
  query: string,
  topK: number,
  filter?: Filter<T>,
  options?: TextSearchOptions,
): Promise<TextSearchResult<T>[]>
```

Unlike `$contains`, which requires **every** token, `searchText` uses OR
semantics: a document matching more of the query scores higher.

```ts
const hits = await articles.searchText('body', 'reset my password', 5)
// [{ document: Article, score: 3.14 }, { document: Article, score: 1.87 }, ...]

// Scope with a filter, and tune BM25:
await articles.searchText('body', 'invoice', 10, { locale: 'en' }, { k1: 1.5, b: 0.75 })
```

**`TextSearchOptions`:**

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `k1` | `number` | `1.2` | Term-frequency saturation. Higher lets repeated terms keep adding relevance. |
| `b` | `number` | `0.75` | Length normalisation. `0` ignores document length; `1` normalises fully. |

**Errors:**

- `IndexNotFound` — no FTS index on `field`
- `InvalidOperation` — the FTS index predates ranking support (created before v0.10); drop and recreate it

## Hybrid search

`hybridSearch` is the reason to use TalaDB for retrieval rather than a plain
key-value store: it combines keyword and semantic search so both an exact SKU
and a paraphrase surface from one query.

### `hybridSearch(text, vector, topK, filter?, options?)`

Rank by BM25 keyword relevance **and** vector similarity, then fuse the two
rankings with [reciprocal rank fusion](https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf).
Requires an FTS index on `text.textField` and a vector index on
`vector.vectorField`.

```ts
hybridSearch(
  text: { textField: keyof Omit<T, '_id'> & string; text: string },
  vector: { vectorField: keyof Omit<T, '_id'> & string; vector: number[] },
  topK: number,
  filter?: Filter<T>,
  options?: HybridSearchOptions,
): Promise<HybridSearchResult<T>[]>
```

```ts
await articles.createFtsIndex('body')
await articles.createVectorIndex('embedding', { dimensions: 384 })

const results = await articles.hybridSearch(
  { textField: 'body',      text: 'how do I get a refund' },
  { vectorField: 'embedding', vector: await embed('how do I get a refund') },
  5,
)

results.forEach(({ document, score, textRank, vectorRank }) => {
  // textRank / vectorRank: zero-based position in each retriever's list,
  // or null if that retriever did not return the document.
  console.log(document.title, { textRank, vectorRank })
})
```

### Why fusion, not score-mixing

The two retrievers fail differently — keyword search misses paraphrases, vector
search misses exact identifiers and rare proper nouns — so fusing them recovers
both. A document both retrievers rank well outranks one only a single retriever
found.

Fusion operates on **ranks, not scores**. That is deliberate: BM25 is unbounded
and cosine lives in [-1, 1], so adding the raw numbers would need a
normalisation step that quietly changes meaning as the corpus grows. Ranks are
already comparable. The returned `score` is the fused RRF value — small by
construction and meaningful only for ordering within one result set.

The optional `filter` is applied to **both** retrievers before ranking, so the
candidate pools stay comparable.

**`HybridSearchOptions`** (extends `TextSearchOptions`):

| Option | Type | Default | Description |
| --- | --- | --- | --- |
| `rrfK` | `number` | `60` | RRF smoothing constant. Larger flattens the advantage of the very top ranks. |
| `textWeight` | `number` | `1` | Weight of the text ranking. `0` disables it. |
| `vectorWeight` | `number` | `1` | Weight of the vector ranking. `0` disables it. |
| `candidates` | `number` | `max(topK*4, 20)` | Candidates pulled from each retriever before fusing. Raise for better recall at more cost. |

**Errors:**

- `IndexNotFound` — no FTS index on `text.textField`
- `VectorIndexNotFound` — no vector index on `vector.vectorField`
- `InvalidOperation` — the FTS index predates ranking support; drop and recreate it

## Building a local RAG pipeline

Hybrid search is the retrieval step of an on-device RAG loop: embed and index
your documents once, then for each user question retrieve the top passages and
feed them to a local LLM (WebLLM, transformers.js) — all without a server.

```ts
// Index (once)
await docs.createFtsIndex('text')
await docs.createVectorIndex('embedding', { dimensions: 384 })
for (const chunk of chunks) {
  await docs.insert({ ...chunk, embedding: await embed(chunk.text) })
}

// Retrieve (per question)
const passages = await docs.hybridSearch(
  { textField: 'text',       text: question },
  { vectorField: 'embedding', vector: await embed(question) },
  8,
)
const context = passages.map((p) => p.document.text).join('\n\n')
// → feed `context` + `question` to your on-device LLM
```
