---
title: Migrations
description: Version-based schema migrations for TalaDB. Define an ordered array of up functions — TalaDB runs pending migrations at database open time, checkpointing after each.
---

# Migrations

Migrations let you evolve your database schema as your application changes. Pass
an ordered `migrations` array to `openDB` and TalaDB runs the pending ones at
open time, in version order, advancing a stored version counter after each.

::: tip Runtime support
Available on **browser (WASM + OPFS worker)** and **Node.js**. On React Native
it throws a clear error until the JSI HostObject exposes the version accessors
(tracked on the [roadmap](/roadmap)). This is separate from TalaDB's **built-in
storage migrations** (index-encoding format, etc.), which run automatically at
every open with no configuration — you never write those.
:::

## How migrations work

1. TalaDB reads the current application version (`0` on a fresh database), stored separately from the engine's own storage-schema version.
2. Every migration whose `version` is greater than the stored version is pending.
3. Pending migrations are sorted by `version` and their `up` bodies run in order.
4. **The stored version advances after each migration's `up` fully resolves** — a checkpoint per version.
5. If an `up` throws, the run stops and the error propagates from `openDB`. The stored version stays at the last fully-applied migration, so the next open resumes from the one that failed.

::: warning Checkpoint-per-version, not whole-batch atomic
A migration `up` runs through the normal collection API, so it is **not** wrapped
in a single all-or-nothing transaction: if an `up` throws halfway, the writes it
already made persist and it re-runs from the top on the next open. **Write
migration bodies idempotently** (guard with existence checks; `createIndex` is
already a no-op if the index exists). Whole-batch transactional rollback would
require a transaction primitive the high-level API does not expose yet.
:::

When an app ships more than one build against the same database — a user on an
old version opening data a newer version already upgraded — migrations pair with
additive-only schema changes and a per-collection `syncSchema.version`, which
stamps `_v` and drives read-time up/downcasting. See
[Schema Validation](/api/schema).

## Defining migrations

Pass a `migrations` array to `openDB`:

```ts
import { openDB } from 'taladb'

const db = await openDB('myapp.db', {
  migrations: [
    {
      version: 1,
      description: 'Create indexes',
      up: async (db) => {
        await db.collection('users').createIndex('email')
        await db.collection('users').createIndex('createdAt')
      },
    },
    {
      version: 2,
      description: 'Add role field to existing users',
      up: async (db) => {
        const users = db.collection('users')
        const all = await users.find({})
        for (const user of all) {
          if (!user.role) {
            await users.updateOne(
              { _id: user._id },
              { $set: { role: 'user' } },
            )
          }
        }
      },
    },
    {
      version: 3,
      description: 'Create posts collection index',
      up: async (db) => {
        await db.collection('posts').createIndex('authorId')
        await db.collection('posts').createIndex('publishedAt')
      },
    },
  ],
})
```

## Migration shape

```ts
interface Migration {
  version: number        // positive integer, must be unique and monotonically increasing
  description: string   // human-readable label (shown in logs)
  up: (db: TalaDB) => Promise<void>
}
```

## Rules

**Versions must be positive integers in ascending order.** Gaps are allowed — you can go from version 1 directly to version 5. But you must never reuse or lower a version number once it has been applied.

**Never modify an existing migration.** A migration runs exactly once. If you change the `up` function after it has run on a device, that device will not re-run it. Instead, add a new migration at a higher version.

**Migrations checkpoint per version — write them idempotently.** The stored version advances after each `up` succeeds, but a single `up` is not one atomic transaction. If migration 3 fails after 1 and 2 succeeded, versions 1 and 2 stay applied and only 3 re-runs on the next open. Guard writes so a partial-then-retried `up` is safe.

**`createIndex` is idempotent.** Calling it for an index that already exists is safe — it does nothing.

## Fresh installs

On a fresh install, the stored version is `0`. All migrations run in order on first open. This means there is no separate "initial schema" step — version 1 is your initial schema.

## Inspecting the current version

In code (advanced), the native binding exposes the stored application version
directly:

```ts
import { TalaDBNode } from '@taladb/node'

const db = TalaDBNode.open('./myapp.db')
db.userVersion()      // → number (0 if no migrations have run)
```

The application version lives in the `meta::user_version` table, kept separate
from the engine's own `meta::db_version` storage-schema counter so the two never
collide.

## Example: adding a new collection over time

```ts
const migrations = [
  {
    version: 1,
    description: 'Initial schema',
    up: async (db) => {
      await db.collection('users').createIndex('email')
    },
  },
  {
    version: 2,
    description: 'Add posts collection',
    up: async (db) => {
      await db.collection('posts').createIndex('authorId')
      await db.collection('posts').createFtsIndex('body')
    },
  },
  {
    version: 3,
    description: 'Normalise email to lowercase',
    up: async (db) => {
      const users = db.collection('users')
      const all = await users.find({})
      for (const user of all) {
        if (typeof user.email === 'string') {
          await users.updateOne(
            { _id: user._id },
            { $set: { email: user.email.toLowerCase() } },
          )
        }
      }
    },
  },
]
```
