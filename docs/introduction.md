---
title: Introduction
description: Learn what TalaDB is, how it works, and why it was built — the embedded vector and document database for on-device AI, powered by a Rust core that runs in the browser, Node.js, and React Native.
---

# Introduction

## What is TalaDB?

AI inference is moving onto the device — transformers.js and ONNX Runtime Web in the browser, Core ML and native models on mobile. The model runs locally, but the *retrieval* layer usually doesn't: embeddings get shipped to a hosted vector database, which puts back the latency, the per-query cost, and the privacy exposure that running locally was supposed to remove.

**TalaDB is the embedded vector and document database for on-device AI.** Built in Rust, it runs entirely on the user's device across the browser, Node.js, and React Native. Store schemaless JSON-like documents in named **collections**, query them with a MongoDB-inspired filter DSL, and use secondary indexes, ACID transactions, and live queries — with no database server required.

Vector search is built into that same document model. Store embeddings alongside regular fields and search them with [vector similarity](/api/vector-search), [BM25 full-text](/api/search), or [hybrid search](/api/search#hybrid-search) (the two fused, RAG-style). A single query can rank by embedding similarity *and* filter by document metadata — with no API key and no data leaving the device.

The result: everything an AI-powered app needs to store, query, and retrieve data lives in one embedded engine, on the device, behind one TypeScript API.

The same Rust core powers every runtime:

| Runtime | Package | Mechanism |
|---|---|---|
| Browser | `@taladb/web` | `wasm-bindgen` + OPFS |
| Node.js | `@taladb/node` | `napi-rs` native module |
| React Native | `@taladb/react-native` | JSI HostObject (C FFI) |

All three surfaces expose a single unified TypeScript API from the `taladb` package, so application code never needs to branch on platform.

## Architecture overview

TalaDB is built in three layers:

```
┌──────────────────────────────────────────────────────────────┐
│  Layer 3 — TypeScript / JavaScript API                        │
│  wasm-bindgen (browser) · napi-rs (Node.js) · JSI (RN)       │
└──────────────────────────────┬───────────────────────────────┘
                               │  postcard bytes
┌──────────────────────────────▼───────────────────────────────┐
│  Layer 2 — Document + Vector Engine  (taladb-core)            │
│  Document model · B-tree indexes · Vector indexes             │
│  Query planner/executor · FTS · Migrations · Live queries     │
└──────────────────────────────┬───────────────────────────────┘
                               │  raw key/value bytes
┌──────────────────────────────▼───────────────────────────────┐
│  Layer 1 — KV Storage Engine                                  │
│  redb (native / Node.js) · OPFS backend (browser)            │
└──────────────────────────────────────────────────────────────┘
```

**Layer 1 — Storage.** [redb](https://github.com/cberner/redb) is a pure-Rust, B-tree embedded key-value store. In the browser, TalaDB replaces it with a custom OPFS backend that uses `FileSystemSyncAccessHandle` inside a SharedWorker, giving durable on-device persistence without IndexedDB's overhead.

**Layer 2 — Document + vector engine.** `taladb-core` sits above the storage layer and knows nothing about JavaScript bindings. It provides the document model, secondary index key encoding, vector index storage and similarity search, the filter/update AST, the query planner, full-text search, and schema migrations. Documents and vector entries live in separate redb tables (`docs::`, `idx::`, `vec::`) but are updated atomically in the same transaction.

**Layer 3 — Bindings.** Thin platform-specific wrappers translate JavaScript values into the Rust types that `taladb-core` expects and route them through the storage layer.

## Repository structure

Packages are grouped by role, so it's clear at a glance what is the engine, what wraps it, and what consumes it:

```
taladb/
├── Cargo.toml                      # Rust workspace
├── pnpm-workspace.yaml
│
├── packages/
│   ├── core/                       # THE engine — pure Rust, no JS bindings (crate: taladb-core)
│   │   └── src/                    #   document/engine/index/collection/vector/query/…
│   │
│   ├── bindings/                   # Thin runtime WRAPPERS over core
│   │   ├── node/                   #   Node.js (napi-rs)          → @taladb/node
│   │   ├── web/                    #   Browser (wasm-bindgen+OPFS) → @taladb/web
│   │   └── react-native/           #   React Native (JSI + C FFI) → @taladb/react-native
│   │
│   ├── clients/                    # What apps import directly (pure TS)
│   │   ├── taladb/                 #   Unified meta-package        → taladb
│   │   └── react/                  #   React hooks                 → @taladb/react
│   │
│   └── tools/
│       └── cli/                    #   Dev CLI (crate: taladb-cli)
│
└── examples/
    ├── web-vite/                   # React + Vite demo
    ├── expo-app/                   # Expo React Native demo
    └── node-script/                # Node.js script demo
```

**core** is the engine; **bindings** are the runtime wrappers over it; **clients** and **integrations** consume it. npm package names (right column) are unchanged by this layout — only their folders are grouped.

## Packages

TalaDB is published as four focused npm packages:

| Package | Purpose |
|---------|---------|
| `taladb` | Unified TypeScript API — auto-detects the platform and delegates to the right backend |
| `@taladb/web` | Browser WASM + OPFS backend |
| `@taladb/node` | Node.js napi-rs native module |
| `@taladb/react-native` | React Native JSI TurboModule |

### Which packages do I install?

**Browser (Vite, Next.js, etc.)**
```bash
pnpm add taladb @taladb/web
```

**Node.js**
```bash
pnpm add taladb @taladb/node
```

**React Native — shared codebase with web or Node.js**
```bash
pnpm add taladb @taladb/react-native
```
Use `openDB` from `taladb` everywhere. It detects React Native automatically.

**React Native — standalone app (RN only, no shared code)**
```bash
pnpm add @taladb/react-native
```
Import directly from `@taladb/react-native`. Calls are synchronous via JSI — no `await` needed. See the [React Native guide](/guide/react-native#standalone-installation) for details.

The `taladb` package lists the platform packages as `optionalDependencies`, which means npm/pnpm won't fail the install if one isn't present — but it won't install them automatically either. You must add whichever platform package you need alongside `taladb`.

## TalaDB vs Recached

[Recached](https://recached.dev) is our sibling project at TalaDB. They are deliberately complementary, not competing — the question is never "which one," it's "which layer."

| | TalaDB | Recached |
|---|---|---|
| What it is | Embedded **database** inside the app | Cache + **sync fabric** between backend and clients |
| Data model | Documents with MongoDB-like queries, indexes, ACID transactions | Keys — strings, collections, JSON |
| Superpower | On-device vector + filtered search, rich queries | Multi-client sync: scoped auth, live fan-out, offline outbox, exactly-once delivery |
| Server | None — runs entirely on-device | The server is the product (Redis-compatible) |
| Truth model | **Device-local truth** | **Shared truth** across users and devices |

The one-line rule: **TalaDB is where one device's data lives; Recached is how many devices agree.** Reach for TalaDB when you need queryable structured data and semantic search on-device. Reach for Recached when many users or devices need to see the same live state.

They meet where an app needs both — locally queryable data that must also reach other users. TalaDB deliberately does not replicate: it owns the on-device query and vector engine and reports its writes outward through a [change webhook](/api/webhook), leaving cross-device agreement to a layer built for it. Recached is a natural one.

## Status

TalaDB is production-ready. The Rust core, browser WASM, Node.js bindings, and React Native JSI layer are fully functional, tested, and stable across all supported platforms.

Try the [web demo](https://demo-web.taladb.dev/) to see TalaDB running in the browser with OPFS persistence and on-device semantic search, or the [mobile demo](https://appetize.io/app/b_ugmjhjghdkgnjux4lzkepvsfma) to see it running on React Native. Follow the [GitHub repository](https://github.com/taladb/taladb) for progress updates.
