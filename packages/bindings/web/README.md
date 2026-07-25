# @taladb/web

Browser WASM bindings for TalaDB — persistent local-first storage via WASM + OPFS.

[![npm](https://img.shields.io/npm/v/@taladb/web)](https://www.npmjs.com/package/@taladb/web)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache_2.0-orange.svg)](LICENSE)

> **Note:** Most users should install [`taladb`](https://www.npmjs.com/package/taladb) instead, which auto-selects this package when running in the browser.

## What it provides

- Rust core compiled to WebAssembly via `wasm-bindgen`
- Persistent storage using [OPFS](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API/Origin_private_file_system) (`FileSystemSyncAccessHandle`) — runs on a dedicated SharedWorker so the main thread is never blocked
- In-memory fallback for environments without OPFS support
- Bundle size target: < 400 KB gzipped

## Browser support

| Browser | OPFS (persistent) | In-memory fallback |
|---------|-------------------|-------------------|
| Chrome 109+ | ✅ | ✅ |
| Safari 16.4+ | ✅ | ✅ |
| Firefox | — | ✅ |
| Edge 109+ | ✅ | ✅ |

## Installation

```bash
pnpm add taladb @taladb/web
```

## Usage

Use through the unified [`taladb`](https://www.npmjs.com/package/taladb) package:

```ts
import { openDB } from 'taladb';

const db = await openDB('myapp.db');
const col = db.collection('notes');

await col.insert({ title: 'Hello', body: 'World' });
const notes = await col.find();
```

### Vite setup

```ts
// vite.config.ts
import { defineConfig } from 'vite';

export default defineConfig({
  optimizeDeps: {
    exclude: ['@taladb/web'],
  },
});
```

### Web Worker (OPFS)

TalaDB spawns a SharedWorker internally to own the OPFS file handle. No extra configuration is required — the worker script is bundled inside this package at `@taladb/web/worker/taladb.worker.js`.

## Direct usage (advanced)

If you need to use the WASM bindings directly:

```ts
import init, { TalaDBWasm } from '@taladb/web';

await init();

const db = TalaDBWasm.openInMemory();
const col = db.collection('items');
col.insert({ x: 1 });
```

## Building from source

```bash
# Install wasm-pack
cargo install wasm-pack

# Build
pnpm --filter @taladb/web build
```

## Full Documentation

**[https://thinkgrid-labs.github.io/taladb/guide/web](https://thinkgrid-labs.github.io/taladb/guide/web)**

## License

Apache 2.0 © [ThinkGrid Labs](https://github.com/thinkgrid-labs)
