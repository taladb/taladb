---
title: Schema Validation
description: Optional runtime type safety for TalaDB collections using Zod, Valibot, or any library with a parse() method.
---

# Schema Validation

Schema validation is **optional**. TalaDB is schemaless by default — you can store any document in any collection without declaring a schema first. When you do want runtime type safety (catching bad data at the boundary before it reaches storage), pass a `schema` option to `db.collection()`.

## Basic usage

Pass a schema as the second argument to `db.collection()`. Any object with a `parse(data: unknown): T` method works — Zod, Valibot, or a hand-rolled validator.

```ts
import { z } from 'zod'

const userSchema = z.object({
  name: z.string().min(1),
  age:  z.number().int().nonnegative(),
  email: z.string().email().optional(),
})

type User = z.infer<typeof userSchema>

const users = db.collection<User>('users', { schema: userSchema })
```

Without the second argument, the collection behaves exactly as before — no validation, no overhead.

## Validation on write

When a schema is set, every call to `insert` and `insertMany` runs the document through `schema.parse()` **before** sending it to storage. If validation fails, a `TalaDbValidationError` is thrown and nothing is written.

```ts
// ✓ valid — stored normally
await users.insert({ name: 'Alice', age: 30 })

// ✗ invalid — throws TalaDbValidationError, nothing stored
await users.insert({ name: '', age: 30 })

// ✗ insertMany — throws on first invalid doc, nothing is stored
await users.insertMany([
  { name: 'Bob', age: 25 },
  { name: '',    age: 0  },  // fails at index 1 → error, nothing committed
])
```

## TalaDbValidationError

Import `TalaDbValidationError` to catch schema errors specifically:

```ts
import { TalaDbValidationError } from 'taladb'

try {
  await users.insert({ name: '', age: -1 })
} catch (err) {
  if (err instanceof TalaDbValidationError) {
    console.error('Validation failed:', err.message)
    console.error('Caused by:', err.cause)
  }
}
```

`err.message` includes the context (`insert`, `insertMany[2]`, etc.) and the underlying schema library's error message. `err.cause` is the raw error thrown by `schema.parse()`.

## Validate on read (optional)

By default, documents returned by `find` and `findOne` are **not** parsed — they come back as stored. Set `validateOnRead: true` if you want to catch schema drift (old documents written before a schema change):

```ts
const users = db.collection<User>('users', {
  schema: userSchema,
  validateOnRead: true,
})

// find() and findOne() now run every returned document through schema.parse()
const all = await users.find()   // throws if any doc fails
```

This is useful during migrations or when evolving a schema on existing data. For most production use cases, leave it off — `validateOnRead` adds a parse call per document on every read.

::: tip Unknown fields survive the parse
`z.object({...})` and `v.object({...})` **strip keys they don't declare**. When one build of an app reads data another build wrote — a user on an old app version opening a database a newer version has already upgraded — that is a live data-loss bug: the old client gets the document back minus the new fields, and its next read-modify-write persists the truncation.

So, when a document's `_v` is newer than this client's `syncSchema.version`, `validateOnRead` parses for the *check* (and any coercions the schema applies), then restores keys the parse dropped. Your v1 code sees the v1 shape it expects; the v2 field rides along untouched. Documents that are not future-version keep the validator's normal strict behavior. See [Migrations](/api/migrations).
:::

## Works with Valibot

The `schema` option is duck-typed — it only requires a `parse(data: unknown): T` method. Valibot schemas expose this via `v.parse()`, but you can also pass a wrapped version:

```ts
import * as v from 'valibot'

const schema = v.object({
  name: v.string(),
  age:  v.number(),
})

const users = db.collection<v.InferOutput<typeof schema>>('users', {
  schema: {
    parse: (data) => v.parse(schema, data),
  },
})
```

Or any custom validator:

```ts
const users = db.collection<User>('users', {
  schema: {
    parse(data: unknown): User {
      const doc = data as Record<string, unknown>
      if (typeof doc.name !== 'string') throw new Error('name must be a string')
      return doc as User
    },
  },
})
```

## CollectionOptions reference

| Option | Type | Default | Description |
|---|---|---|---|
| `schema` | `{ parse(data: unknown): T }` | `undefined` | Schema validator. When set, `insert` and `insertMany` run every document through `schema.parse()`. Compatible with Zod, Valibot, or any object with a `parse` method. |
| `validateOnRead` | `boolean` | `false` | When `true`, documents returned by read and shape-preserving aggregate APIs are passed through `schema.parse()`. Stripped keys are restored only for documents newer than `syncSchema.version` — see above. |
| `syncSchema` | `SyncSchema` | `undefined` | Declares the collection's shape `version`, which the two hooks below migrate toward and which is stamped as `_v` on every write. |
| `migrateDocument` | `(doc: T, from: number) => T` | `undefined` | **Upcast.** Lazily normalizes a document whose `_v` is *below* `syncSchema.version` on read. |
| `downgradeDocument` | `(doc: Readonly<T>, from: number) => T` | `undefined` | **Downcast.** Projects a document whose `_v` is *above* `syncSchema.version` into the shape this build reads — for an old app build opening a database a newer build already upgraded. Always view-only; never persisted. |
| `persistMigrations` | `boolean` | `false` | Write an upcast document back to storage so the migration becomes permanent. The write-back is additive-only (`$set`) unless fields are named in `retiredFields`. |
| `retiredFields` | `(keyof T & string)[]` | `[]` | Fields an upcast may intentionally remove during persist-on-read. Prefer this precise retirement list. |
| `allowFieldRemoval` | `boolean` | `false` | **Deprecated.** Treat every field absent from `migrateDocument`'s output as an intentional removal. Destructive when app versions differ — prefer `retiredFields`. |

## FAQ

**Does the schema affect storage?**
No. The schema only runs at the TypeScript layer — TalaDB stores documents exactly as returned by `schema.parse()`. The underlying storage is still schemaless.

**Can I change the schema later?**
Yes. Existing documents are not re-validated when you update the schema definition — only new writes are checked. Use `validateOnRead: true` temporarily to audit existing data, then run a migration to fix any non-conforming documents.

**Is there a performance cost?**
Only on operations where validation runs (`insert`, `insertMany`, and `find`/`findOne` when `validateOnRead` is `true`). Collections without a `schema` option have zero overhead — the code path is identical to before.
