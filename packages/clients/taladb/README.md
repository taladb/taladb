# taladb

Local-first document database for React, React Native, and Node.js — powered by a Rust/WASM core with zero GC pauses.

[![npm](https://img.shields.io/npm/v/taladb)](https://www.npmjs.com/package/taladb)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-orange.svg)](LICENSE)

## What is TalaDB?

TalaDB gives you a MongoDB-style document API that runs entirely on-device — in the browser via WASM + OPFS, in React Native via JSI, and in Node.js via a native module. No cloud, no sync server, no garbage-collection pauses.

```ts
import { openDB } from 'taladb';

const db = await openDB('myapp.db');
const users = db.collection('users');

await users.createIndex('email');

const id = await users.insert({ name: 'Alice', email: 'alice@example.com', age: 30 });

const alice  = await users.findOne({ email: 'alice@example.com' });
const adults = await users.find({ age: { $gte: 18 } });

await users.updateOne({ email: 'alice@example.com' }, { $inc: { age: 1 } });
await users.deleteOne({ email: 'alice@example.com' });

await db.close();
```

## Installation

```bash
# npm
npm install taladb

# pnpm
pnpm add taladb

# yarn
yarn add taladb
```

The package automatically loads the right backend for your platform:

| Platform | Backend |
|----------|---------|
| Browser  | `@taladb/web` (WASM + OPFS) |
| Node.js  | `@taladb/node` (napi-rs native module) |
| React Native | `@taladb/react-native` (JSI TurboModule) |

Install the appropriate platform package as well:

```bash
# Browser
pnpm add taladb @taladb/web

# Node.js
pnpm add taladb @taladb/node

# React Native
pnpm add taladb @taladb/react-native
```

## API

### Database

```ts
const db = await openDB(name?: string): Promise<TalaDB>

db.collection<T>(name: string): Collection<T>
db.close(): Promise<void>
```

### Collection

```ts
// Write
col.insert(doc): Promise<string>                          // returns ULID id
col.insertMany(docs): Promise<string[]>
col.updateOne(filter, update): Promise<boolean>
col.updateMany(filter, update): Promise<number>
col.deleteOne(filter): Promise<boolean>
col.deleteMany(filter): Promise<number>

// Read
col.find(filter?): Promise<T[]>
col.findOne(filter): Promise<T | null>
col.count(filter?): Promise<number>

// Indexes
col.createIndex(field): Promise<void>                     // idempotent
col.dropIndex(field): Promise<void>
```

### Filters

```ts
{ field: value }                                // equality
{ field: { $eq, $ne, $gt, $gte, $lt, $lte } }  // comparisons
{ field: { $in: [...], $nin: [...] } }          // set membership
{ field: { $exists: true | false } }            // field presence
{ $and: [...filters] }
{ $or: [...filters] }
{ $not: filter }
```

### Updates

```ts
{ $set:   { field: value } }      // set or replace
{ $unset: { field: true } }       // remove field
{ $inc:   { field: number } }     // increment / decrement
{ $push:  { field: value } }      // append to array
{ $pull:  { field: value } }      // remove from array
```

## Migrations

Run schema migrations at open time — each migration runs exactly once, in version order:

```ts
import { runMigrations } from 'taladb';

await runMigrations(db, [
  {
    fromVersion: 0,
    toVersion: 1,
    description: 'add default role',
    async up(col) {
      await col('users').updateMany({}, { $set: { role: 'viewer' } });
    },
  },
]);
```

## Full Documentation

**[https://taladb.github.io/taladb](https://taladb.github.io/taladb)**

- [Introduction](https://taladb.github.io/taladb/introduction)
- [Web (Browser / WASM) Guide](https://taladb.github.io/taladb/guide/web)
- [Node.js Guide](https://taladb.github.io/taladb/guide/node)
- [React Native Guide](https://taladb.github.io/taladb/guide/react-native)
- [API Reference](https://taladb.github.io/taladb/api/collection)

## License

Apache 2.0 © [TalaDB](https://github.com/taladb)
