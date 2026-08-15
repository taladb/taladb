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

Hybrid search is the **retrieval** step of an on-device RAG loop. The full loop
has four stages, and TalaDB owns one of them:

| Stage | Who provides it |
| --- | --- |
| **Chunk** — split source text into passages | Your application |
| **Embed** — turn a passage into a vector | An embedding model you choose |
| **Retrieve** — find the passages that answer a question | **TalaDB** |
| **Generate** — write an answer from those passages | An LLM you choose |

TalaDB does not ship an embedding model or a chunker. It stores vectors and
ranks them; it does not produce them. That keeps the database free of a model
runtime and lets you pick your own trade-off between quality, bundle size, and
whether anything leaves the device.

The rest of this section is a complete, runnable pipeline with nothing left
undefined.

### 1 · Chunk

Retrieval quality is decided here more than anywhere else. Split on structure
you already have — paragraphs, headings, sections — rather than a fixed
character window, and carry a little overlap so a sentence spanning a boundary
is still findable.

```ts
function chunk(text: string, target = 1200, overlap = 200): string[] {
  const paragraphs = text.split(/\n\s*\n/).filter((p) => p.trim())
  const chunks: string[] = []
  let current = ''

  for (const paragraph of paragraphs) {
    if (current.length + paragraph.length > target && current) {
      chunks.push(current.trim())
      current = current.slice(-overlap)
    }
    current += paragraph + '\n\n'
  }
  if (current.trim()) chunks.push(current.trim())

  return chunks
}
```

If your source has headings, prepend the heading path to each chunk
(`"Chapter 3 › Pricing › Enterprise\n\n…"`). Both retrievers index that text, so
it improves keyword and semantic matching at once.

### 2 · Embed

Any function of type `(text: string) => Promise<number[]>` works. Two common
choices:

**On-device**, using [Transformers.js](https://huggingface.co/docs/transformers.js).
Nothing leaves the browser:

```ts
import { pipeline } from '@huggingface/transformers'

const extractor = await pipeline('feature-extraction', 'Xenova/all-MiniLM-L6-v2')

async function embed(text: string): Promise<number[]> {
  const output = await extractor(text, { pooling: 'mean', normalize: true })
  return Array.from(output.data as Float32Array) // 384 floats
}
```

**Remote**, using a hosted embedding API — better quality, no model download,
but your text goes to a third party:

```ts
async function embed(text: string): Promise<number[]> {
  const res = await fetch('https://api.example.com/embeddings', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ input: text }),
  })
  return (await res.json()).data[0].embedding
}
```

::: warning Dimensions must match
`createVectorIndex(field, { dimensions })` has to equal your model's output
width — 384 for `all-MiniLM-L6-v2`, 768 and 1536 are also common. A mismatch is
rejected at insert time, and changing models later means re-embedding every
document and rebuilding the index.
:::

Embedding is CPU-bound. On the browser, run it in a worker so it doesn't block
the main thread, and embed at write time only — never per query result.

### 3 · Retrieve

This part is TalaDB. Index once, then run one hybrid query per question:

```ts
// Index (once)
await docs.createFtsIndex('text')
await docs.createVectorIndex('embedding', { dimensions: 384 })

for (const passage of chunk(sourceText)) {
  await docs.insert({ text: passage, embedding: await embed(passage) })
}

// Retrieve (per question)
const passages = await docs.hybridSearch(
  { textField: 'text',        text: question },
  { vectorField: 'embedding', vector: await embed(question) },
  8,
)
```

Store a `source` or `locator` alongside each chunk if you want the answer to
cite where it came from — retrieval returns the whole document, so anything you
put on it comes back with the hit.

### 4 · Generate

Assemble the retrieved passages into a prompt and hand them to a model. Keeping
this step on-device too — with [WebLLM](https://webllm.mlc.ai/) or
Transformers.js — is what makes the loop fully local:

```ts
const context = passages.map((p) => p.document.text).join('\n\n---\n\n')
const prompt = `Answer using only the context below.\n\n${context}\n\nQ: ${question}`
// → send `prompt` to your LLM
```

::: tip Local retrieval, remote generation
Running retrieval on-device and generation through a hosted API is a reasonable
default, but note what it implies: the passages you send are by construction the
*most relevant* parts of the document. If "the data never leaves the device" is
a requirement rather than a preference, the generation step has to be local too.
:::
