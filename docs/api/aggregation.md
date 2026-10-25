---
title: Aggregation
description: TalaDB aggregation pipeline reference — $match, $group, $sort, $skip, $limit, $project stages and $sum, $avg, $min, $max, $count, $push, $addToSet, $first, $last accumulators, computed inside the engine.
---

# Aggregation

`collection.aggregate(pipeline)` runs a MongoDB-style pipeline **inside the engine**, so summaries are computed in Rust over the B-tree instead of loading every document into JavaScript. A pipeline is an array of stages; each stage transforms the stream of documents produced by the previous one.

```ts
const byStatus = await orders.aggregate([
  { $match: { status: { $ne: 'draft' } } },
  { $group: { _id: '$status', total: { $sum: '$amount' }, n: { $sum: 1 } } },
  { $sort: { total: -1 } },
])
// → [{ _id: 'paid', total: 4820, n: 37 }, { _id: 'pending', total: 910, n: 6 }]
```

## TypeScript type

```ts
type AggregatePipeline<T extends Document = Document> = AggregateStage<T>[]

aggregate<R = Document>(pipeline: AggregatePipeline<T>): Promise<R[]>
```

The result type `R` is unconstrained — aggregation produces new shapes (grouped keys, computed fields), so you pass the shape you expect:

```ts
type Row = { _id: string; total: number; n: number }
const rows = await orders.aggregate<Row>([/* … */])
```

## Stages

| Stage | Purpose |
|---|---|
| `$match` | Keep only documents matching a [filter](/api/filters). Uses an index when it is the first stage. |
| `$group` | Group documents by a key and compute accumulators per group. |
| `$sort` | Order documents. `1` ascending, `-1` descending. |
| `$skip` | Drop the first *n* documents. |
| `$limit` | Keep at most *n* documents. |
| `$project` | Choose which fields to keep. Truthy value keeps a field. |

### `$match`

Takes the same [filter](/api/filters) object as `find`. When `$match` is the **first** stage, the query planner uses an index if one is available, so filtering happens before any documents are materialised.

```ts
{ $match: { status: 'active', amount: { $gte: 100 } } }
```

### `$group`

`_id` defines the grouping key. Use a field reference (`'$field'`) to group by that field, or `null` to aggregate the whole stream into a single group. Every other key is an accumulator whose value names the operation.

```ts
{ $group: {
    _id: '$customerId',
    spent: { $sum: '$amount' },
    orders: { $sum: 1 },       // count — $sum of the constant 1
    avgOrder: { $avg: '$amount' },
    firstSeen: { $min: '$createdAt' },
} }
```

The output document of each group has `_id` set to the group key plus one field per accumulator.

#### Accumulators

| Accumulator | Result |
|---|---|
| `{ $sum: '$field' }` | Sum of the field across the group |
| `{ $sum: 1 }` | Count of documents in the group |
| `{ $count: {} }` | Count of documents in the group |
| `{ $avg: '$field' }` | Mean of the field |
| `{ $min: '$field' }` | Minimum value |
| `{ $max: '$field' }` | Maximum value |
| `{ $first: '$field' }` | Field of the first document in the group |
| `{ $last: '$field' }` | Field of the last document in the group |
| `{ $push: '$field' }` | Array of the field's values, in order |
| `{ $addToSet: '$field' }` | Array of the field's distinct values |

### `$sort`, `$skip`, `$limit`

```ts
{ $sort: { total: -1, name: 1 } }   // total desc, then name asc
{ $skip: 20 }
{ $limit: 10 }
```

### `$project`

Reshape each document. A projection is either an **inclusion** or an **exclusion** — not both.

```ts
{ $project: { name: 1, city: 1 } }   // inclusion — keep ONLY these fields
{ $project: { description: 0 } }     // exclusion — keep everything EXCEPT this
```

`_id` is kept unless you set `_id: 0`, and that is the one exclusion allowed to sit alongside an inclusion:

```ts
{ $project: { total: 1, _id: 0 } }   // fine
{ $project: { name: 1, bulky: 0 } }  // throws — mixed inclusion and exclusion
```

::: warning Changed in 0.9.3
Before 0.9.3 the engine implemented inclusion only. An exclusion such as `{ description: 0 }` parsed to an *empty inclusion list*, so every document came back stripped of every field but `_id` — silently, with no error. Exclusion now works, and mixing the two modes raises an error instead of resolving to something arbitrary.
:::

## Paging: sort + skip + limit

A `$sort` followed by `$skip`/`$limit` is a **bounded** query, and the engine treats it as one. When the sort's first key is indexed and no `$match` precedes it, the page is served from that index, and no document after the page is decoded:

```ts
await listings.aggregate([
  { $sort: { rating: -1 } },   // `rating` is indexed
  { $skip: 0 },
  { $limit: 24 },
  { $project: { name: 1, city: 1, rating: 1 } },
]);
```

An ascending sort stops reading the index at the end of the page. A descending sort reads every index key, but no documents beyond the page. Memory is bounded by `skip + limit` in both directions. Multi-key sorts qualify too: only the *first* key needs an index, and the documents of a run of ties that the page cuts through are decoded so the later keys can order them.

The index is used only when its order is provably the sort's order. Otherwise the query reads the collection, still ordering only the reachable `skip + limit` documents. That happens when:

- some documents lack the field;
- some document currently holds an array in it, or the index was built before the engine began counting array values (rebuilding the index fixes that);
- it mixes integers and floats, or holds NaN or zero floats;
- it is field-encrypted;
- the walk reaches a string or byte array containing a NUL character, in a collection where some `_id` starts with byte `0xFF` (as about 1 in 256 `deriveDocId` ids do). Such a value's index key can sort ahead of a shorter value it extends.

`find` with `sort` and `limit` takes the same path. There, documents wholly before `skip` are not decoded either.

Paging is stable: ties are broken on the unique `_id`, so a document can never appear on two pages or fall between them as `$skip` grows.

## Runtime availability

`aggregate()` is available on every runtime — Node.js, the browser (both the direct main-thread build and the OPFS worker), and React Native. The pipeline is parsed and executed inside the engine on each platform; only the transport differs (native call, WASM, worker message, or JSI).

::: tip Key ordering
`$group` accumulator fields and multi-key `$sort` are keyed from the pipeline object. Because pipelines are parsed with a `BTreeMap`, multiple keys within a single `$sort` object are applied in **alphabetical** key order, not the order you wrote them. Accumulator output names within a `$group` are unaffected — each names its own field. When multi-key sort precedence matters, prefer a single sort key whose ordering already reflects your intent.
:::
