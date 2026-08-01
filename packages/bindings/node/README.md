# @taladb/node

Node.js native module for TalaDB — high-performance local-first storage via a napi-rs Rust binding.

[![npm](https://img.shields.io/npm/v/@taladb/node)](https://www.npmjs.com/package/@taladb/node)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-orange.svg)](LICENSE)

> **Note:** Most users should install [`taladb`](https://www.npmjs.com/package/taladb) instead, which auto-selects this package when running in Node.js.

## What it provides

- TalaDB's Rust core exposed via [napi-rs](https://napi.rs/) — zero marshalling overhead
- Synchronous hot-path methods + `async` variants for non-blocking server use
- Pre-built binaries for `linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`, `win32-x64`
- No `node-gyp` required — binaries ship with the package

## Requirements

- Node.js 18+

## Installation

```bash
pnpm add taladb @taladb/node
```

## Usage

Use through the unified [`taladb`](https://www.npmjs.com/package/taladb) package:

```ts
import { openDB } from 'taladb';

const db = await openDB('./myapp.db');
const users = db.collection('users');

await users.createIndex('email');
await users.insert({ name: 'Alice', email: 'alice@example.com', role: 'admin' });

const alice = await users.findOne({ email: 'alice@example.com' });
const admins = await users.find({ role: 'admin' });
```

### CommonJS

```js
const { openDB } = require('taladb');

async function main() {
  const db = await openDB('./myapp.db');
  const col = db.collection('items');

  await col.insert({ name: 'Widget', price: 9.99 });
  const results = await col.find({ price: { $lt: 20 } });
  console.log(results);

  await db.close();
}

main();
```

## Direct usage (advanced)

```js
const { TalaDBNode } = require('@taladb/node');

const db = TalaDBNode.open('./myapp.db');
const col = db.collection('items');

// Synchronous (no await)
const id = col.insert({ x: 1 });
const docs = col.find(null);

db.close();
```

## Building from source

```bash
# Install napi-rs CLI
pnpm add -g @napi-rs/cli

# Build for current platform
pnpm --filter @taladb/node build
```

## Full Documentation

**[https://taladb.github.io/taladb/guide/node](https://taladb.github.io/taladb/guide/node)**

## License

Apache 2.0 © [TalaDB](https://github.com/taladb)
