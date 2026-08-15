---
title: Filters
description: TalaDB filter DSL reference — $eq, $ne, $gt, $gte, $lt, $lte, $in, $nin, $exists, $regex, $and, $or, $not, $contains, and dot-notation nested field queries.
---

# Filters

A filter is a plain JavaScript/TypeScript object that describes which documents to match. TalaDB evaluates filters against each document's fields.

## TypeScript type

```ts
type Filter<T extends Document = Document> = {
  [K in keyof T]?: T[K] | FieldOps<T[K]>
} & {
  $and?: Filter<T>[]
  $or?:  Filter<T>[]
  $not?: Filter<T>
}
```

## Field shorthand (equality)

Providing a bare value for a field is equivalent to `{ $eq: value }`.

```ts
// These are identical:
await users.find({ name: 'Alice' })
await users.find({ name: { $eq: 'Alice' } })
```

## Comparison operators

| Operator | Meaning | Example |
|---|---|---|
| `$eq` | Equal to | `{ age: { $eq: 30 } }` |
| `$ne` | Not equal to | `{ status: { $ne: 'deleted' } }` |
| `$gt` | Greater than | `{ score: { $gt: 90 } }` |
| `$gte` | Greater than or equal | `{ age: { $gte: 18 } }` |
| `$lt` | Less than | `{ price: { $lt: 100 } }` |
| `$lte` | Less than or equal | `{ age: { $lte: 65 } }` |

```ts
// Age between 25 and 40 (inclusive)
await users.find({ age: { $gte: 25, $lte: 40 } })
```

Multiple operators on the same field are implicitly ANDed.

## Membership operators

| Operator | Meaning | Example |
|---|---|---|
| `$in` | Value is in the array | `{ role: { $in: ['admin', 'editor'] } }` |
| `$nin` | Value is not in the array | `{ tag: { $nin: ['spam', 'archive'] } }` |

```ts
const staff = await users.find({ role: { $in: ['admin', 'moderator', 'editor'] } })
const clean  = await posts.find({ tags: { $nin: ['spam', 'hidden'] } })
```

## Array fields

A comparison against a field holding an array matches when **any element**
satisfies it. There is no separate operator: the ordinary ones reach into the
list.

```ts
await posts.insert({ title: 'Local-first', tags: ['rust', 'database'] })

await posts.find({ tags: 'rust' })                    // ✓ an element matches
await posts.find({ tags: { $in: ['rust', 'swift'] } }) // ✓ any element, any candidate
await posts.find({ scores: { $gte: 90 } })            // ✓ any element ≥ 90
await posts.find({ tags: { $ne: 'rust' } })           // ✗ excluded — an element matches
```

The whole-array comparison is tried first, so an exact list still matches
exactly:

```ts
await posts.find({ tags: ['rust', 'database'] })       // ✓ this document only
```

A document is returned **once**, however many of its elements match.

Indexing an array field is worthwhile and needs no special syntax —
`createIndex('tags')` stores one entry per element, so `{ tags: 'rust' }` is a
point lookup rather than a scan:

```ts
await posts.createIndex('tags')
```

Two limits are worth knowing:

- **Nested arrays are not flattened.** `[['a']]` does not match `'a'`. One level
  is what the index stores, and matching deeper would make the same query answer
  differently once an index existed.
- **A compound index covers at most one array field.** Two arrays in one document
  would put the product of the two lists in the index, so the write is rejected
  rather than silently admitted.

## Logical operators

### `$and`

All conditions must match. Equivalent to a regular object with multiple fields.

```ts
// These are identical:
await users.find({ age: { $gte: 18 }, active: true })
await users.find({ $and: [{ age: { $gte: 18 } }, { active: true }] })
```

Use the explicit `$and` form when you need two conditions on the same field or when mixing field conditions with logical operators.

### `$or`

At least one condition must match.

```ts
await users.find({
  $or: [{ role: 'admin' }, { role: 'superuser' }],
})
```

::: tip Index acceleration for `$or`
When every branch of an `$or` uses an equality or range filter on indexed fields, TalaDB uses an **IndexOr** plan: it runs one index range scan per branch and merges the results in memory. Otherwise it falls back to a full scan.
:::

### `$not`

The condition must not match.

```ts
await users.find({ $not: { active: false } })
```

## Existence operator

```ts
// Documents that have an `avatar` field (value may be null)
await users.find({ avatar: { $exists: true } })

// Documents without a `deletedAt` field
await users.find({ deletedAt: { $exists: false } })
```

## Pattern matching (`$regex`)

Match string fields against a regular expression. The regex is compiled at query time and evaluated per document — there is no index acceleration.

```ts
await users.find({ email: { $regex: '@example\\.com$' } })
```

| Behaviour | Notes |
|---|---|
| Non-string fields | Never match — `$regex` against a number or boolean always returns no results |
| Invalid pattern | Returns no results — the engine does not throw on a malformed regex |
| Character classes | Full PCRE-like syntax: `\d`, `\w`, `\s`, anchors `^` / `$`, groups, quantifiers |
| Case-insensitive | Use the inline flag: `(?i)pattern` |

```ts
// Domain suffix
await accounts.find({ website: { $regex: '\\.(io|com|dev)$' } })

// Starts with a digit
await codes.find({ ref: { $regex: '^\\d' } })

// Case-insensitive match (inline flag)
await tags.find({ label: { $regex: '(?i)^todo' } })

// Combined with other operators
await users.find({
  $and: [
    { role: 'admin' },
    { email: { $regex: '@company\\.com$' } },
  ],
})
```

::: warning Performance
`$regex` always performs a full collection scan — even when an index exists on the same field. For large collections, consider adding additional equality or range filters in an `$and` to narrow the candidate set first.
:::

## Nested field queries (dot-notation)

Use dot-notation to filter on fields inside nested objects. Any comparison or existence operator works on a nested path.

```ts
// Equality on a nested field
await users.find({ 'address.city': 'London' })

// Range on a nested field
await users.find({ 'location.altitude': { $gt: 1000 } })

// Three levels deep
await config.find({ 'server.tls.enabled': true })

// Existence check on a nested field
await profiles.find({ 'meta.avatarUrl': { $exists: true } })
```

Nested paths resolve into `Value::Object` entries recursively. If any segment along the path is absent or is not an object, the document is treated as not matching (no error is thrown).

```ts
// Documents where address.city == 'London' are matched;
// documents where `address` is absent, not an object, or has no `city` are not matched.
await users.find({ 'address.city': 'London' })
```

### With logical operators

Dot-notation paths work inside `$and`, `$or`, and `$not`:

```ts
await users.find({
  $or: [
    { 'address.city': 'London' },
    { 'address.city': 'Edinburgh' },
  ],
})

await users.find({
  $and: [
    { 'address.country': 'UK' },
    { 'address.postcode': { $regex: '^SW' } },
  ],
})
```

::: tip Index acceleration with nested fields
Standard secondary indexes (created with `createIndex`) work on top-level fields only. Filtering on a nested field always uses a full scan. If you frequently query a nested field, consider flattening it to a top-level field or using a compound index on the top-level fields you need.
:::

## Full-text search (`$contains`)

Available on fields indexed with `createFtsIndex`:

```ts
await posts.createFtsIndex('body')

// Matches documents whose `body` contains all three terms (after normalisation)
const results = await posts.find({ body: { $contains: 'rust embedded database' } })
```

Search terms are normalised (lowercased, punctuation stripped). Terms shorter than 3 characters are ignored. The result set is post-filtered to ensure all terms appear.

`$contains` is a boolean gate — *every* token must be present. For relevance-**ranked** keyword search (BM25) or hybrid keyword-plus-vector retrieval, see [`searchText` and `hybridSearch`](/api/search).

## Match all documents

Pass an empty filter or no filter at all:

```ts
const all = await users.find()
const all = await users.find({})
```

## Index acceleration summary

The query planner selects an index when the filter matches one of these patterns:

| Pattern | Plan |
|---|---|
| `{ field: value }` or `{ field: { $eq: value } }` on an indexed field | `IndexEq` — point lookup |
| `{ field: { $gt/$gte/$lt/$lte } }` on an indexed field | `IndexRange` — B-tree range scan |
| `{ field: { $in: [...] } }` on an indexed field | `IndexIn` — one point lookup per value |
| `$or` where every branch is an indexed equality/range | `IndexOr` — one range scan per branch, merged |
| `$and` with equality on all fields of a compound index | `CompoundIndexEq` — single B-tree range scan |
| `{ field: { $contains: '...' } }` on an FTS-indexed field | `FtsSearch` — inverted token index |
| All other filters | `FullScan` — every document evaluated in memory |

An array field uses the same plans: it is indexed once per element, so element
matches are point lookups and a range over a list is one B-tree scan.

## Filter examples

```ts
// Exact match
await users.find({ email: 'alice@example.com' })

// Range
await products.find({ price: { $gte: 10, $lte: 50 } })

// Compound
await orders.find({
  $and: [
    { status: 'shipped' },
    { createdAt: { $gte: Date.now() - 86_400_000 } },
  ],
})

// OR with multiple values
await notifications.find({
  $or: [
    { type: 'mention' },
    { type: 'reply' },
    { type: 'reaction' },
  ],
})

// NOT
await sessions.find({ $not: { expired: true } })

// Existence
await profiles.find({ bio: { $exists: true } })

// Full-text
await articles.find({ content: { $contains: 'open source rust' } })

// Regex — domain match
await accounts.find({ email: { $regex: '@acme\\.com$' } })

// Nested field
await orders.find({ 'shipping.address.city': 'Berlin' })

// Compound index — equality on both indexed fields in one B-tree scan
await people.find({ $and: [{ lastName: 'Smith' }, { firstName: 'Alice' }] })
```
