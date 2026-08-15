---
title: Roadmap
description: Planned and in-progress features for TalaDB
---

# Roadmap

This page tracks planned and in-progress work for TalaDB. Sections and items are ordered by estimated impact — things at the top affect the most users and unblock the most use cases.

Have an idea or want to help prioritise? Open a [GitHub Discussion](https://github.com/taladb/taladb/discussions) or a feature request issue.

---

## 1 · Developer experience

Better DX drives adoption and reduces time-to-production.

### Change webhook

The [change webhook](/api/webhook) shipped in **v0.11.0**, replacing the sync
engine. Remaining work:

- **Coalescing.** Delivery fires one request per affected document, with no
  debounce or batch window; the only timing knob is per-request retry backoff
  (200/400/800 ms × 3), which is unrelated. A `batch: { maxEvents, maxWaitMs }`
  option would let a bulk import send one request instead of five hundred.
- **A durable outbox, opt-in.** Delivery is at most once — a crash or a closed
  tab drops in-flight events. Some applications genuinely need at-least-once;
  those need events written to a reserved collection inside the mutation's own
  transaction and drained separately. Deliberately opt-in: it re-adds the
  per-write cost that removing tombstones just reclaimed.
- **Multi-tab write election.** Removing the changeset machinery also removed
  cross-tab write merging in the browser IndexedDB-fallback path (see the
  [web guide](/guide/web#multi-tab-behaviour)). A Web-Locks-based leader
  election that routes writes through the primary tab would restore it without
  reintroducing a merge protocol.

### Schema evolution on React Native — pending on-device verification

The `openDB({ migrations })` version accessors (`userVersion` / `setUserVersion`)
are wired through the full React Native stack — Rust FFI, C header, and the JSI
HostObject (C++) — and feature-detected by the TS client so an older native
module degrades gracefully. The Rust and TS layers compile and typecheck, but the
JSI native glue has **not** been built or exercised on a device or simulator;
that verification (iOS + Android) is the remaining gate. Read-time
`migrateDocument` + `persistMigrations` and `_v` upgrades ship on browser + Node
— see [Schema Validation](/api/schema).

### Compound indexes — remaining work

Multi-field B-tree indexes shipped (Node.js + browser; React Native pending on-device verification). Still to do:

- Partial-prefix and trailing-range matching — use the index when only the leading field(s) are constrained, or the last is a range.
- Per-field **descending** order (`CompoundIndexDef` has no direction yet).
- The `createIndex(['a','b'])` array-sugar overload.

### `taladb generate` — TypeScript type generation

Inspect a live database and emit TypeScript interfaces for each collection, inferred from the stored documents. Useful for projects that don't start with a schema.

### Framework adapters — Svelte and Vue

- **`@taladb/svelte`** — `readable` stores backed by `Collection.subscribe`, plus a `TalaDBContext` Svelte context helper. `$findResult` is a readable store that re-derives on every write matching the filter.
- **`@taladb/vue`** — `useFind` and `useFindOne` composables built on Vue's `ref` + `watchEffect`, mirroring the `@taladb/react` hook API.

Both packages are thin wrappers over the same event model used by the React hooks.

### VS Code extension

Syntax highlighting for TalaDB filter expressions in JSON, inline document previews, and a collection browser panel in the VS Code sidebar.

---

## 2 · Performance & vector search

Driven by findings from the [benchmark suites](/benchmarks) (`pnpm bench`, `pnpm bench:web`). The goal: keep TalaDB among the fastest embedded databases on every JS runtime.

### Cached decoded vectors — avoid re-reading storage per query

The flat path still `scan_all`s the entire vector table from redb on **every** query (150 MB of reads for 100k × 384-dim). A persistent in-memory decoded-vector cache — invalidated on writes, mirroring the existing HNSW-graph cache — would make repeated queries memory-bound instead of storage-bound. Likely the single largest remaining flat-search win.

### SIMD dot products (WASM validated, native next)

The scoring reductions are scalar today. The WASM lever is **measured and confirmed**, and productizing it is the top browser-perf task:

- **WASM** — a `+simd128` build was A/B'd on the benchmark laptop and **~halves** browser vector search (50k: 172 ms → 81 ms; 10k: 35 ms → 17 ms), restoring near-native parity. The remaining work is *shipping* it safely: a single simd128 module fails to instantiate on browsers without WASM SIMD (Safari 15.2–16.3, which TalaDB otherwise supports via OPFS), so this needs either dual builds with runtime feature detection (load simd or scalar `.wasm`) or a deliberate baseline bump to simd128-capable browsers. The build itself is just `RUSTFLAGS="-C target-feature=+simd128"` — LLVM autovectorizes the v0.9.0 byte-streaming loops with no code change. (Also: the `release-wasm` profile in `Cargo.toml` sets `opt-level = "z"` but is *unused* — remove it so nobody ships size-optimized vectors by accident.)
- **Native**: the release profile sets no `target-cpu`, so distributed binaries can't assume AVX2/NEON. An explicit `std::simd` (or chunked-FMA) dot-product kernel with runtime feature detection would vectorise the multiply-add without breaking portability of the prebuilt `.node`.

### Query planner — remaining work

Bounded two-sided range plans (`$gte` + `$lt` on one indexed field → a single bounded index scan) shipped. Still to do: extend to `$in` + range combinations on compound indexes once partial-prefix matching lands.

### Non-blocking HNSW graph builds

`createVectorIndex(..., { indexType: 'hnsw' })` blocks while the graph is constructed — tens of minutes at 50k × 384-dim on laptop hardware:

- Build on a background thread with an `onProgress` callback; queries fall back to the flat scan until the graph is ready
- Incremental graph inserts, so steady-state writes don't require a full `upgradeVectorIndex` rebuild
- Document expected build cost by collection size so apps can schedule rebuilds during idle periods

### Faster filtered-vector pre-filters (id-only path)

The [v0.9.x scan rewrite](#faster-flat-vector-search-shipped-in-v0-9-x) already skips *scoring* filtered-out vectors, but the pre-filter itself still runs `find()`, which materialises every matching document — embedding arrays included — just to collect their ids. An id-only execution path in the query executor (return ids without decoding document bodies) would cut the filter cost, especially for low-selectivity filters over large documents.

### HNSW on web and React Native

The `vector-hnsw` feature ships in `@taladb/node` since v0.8.3 but not in the WASM or JSI builds. Evaluate enabling it per platform: WASM bundle size, mobile memory ceilings, and graph build time on phone CPUs all need numbers first.

### Continuous benchmarks

Run the Node and browser suites in CI on a fixed runner class per release and publish the trend, so performance regressions are caught before they ship. Extend with a React Native suite (the one runtime not yet covered).

---

## 3 · Storage

Internal improvements that improve efficiency and interoperability.

### Pluggable serialisation

Allow the caller to swap `postcard` for `MessagePack` or `CBOR` via a `Codec` trait, making it easier to interoperate with databases or wire formats that already use those encodings.

### Document TTL (time-to-live)

Set an expiry on any document at write time:

- `collection.insert({ ...doc, _ttl: Date.now() + 60_000 })` — document auto-deleted after the TTL elapses
- Background reaper runs on a configurable interval (default: 60 s in Node.js, on next open in browser)

---

## 4 · Platform

Expanding the runtimes TalaDB can target.

### Swift / Kotlin native packages

First-party Swift (`TalaDB.swift`) and Kotlin (`taladb-kotlin`) packages that wrap the C FFI layer directly, without React Native, for native iOS and Android apps that want an embedded document store.

### WASI target

Compile `taladb-core` to WASI (`wasm32-wasip1`) so it can run inside WASI runtimes (Wasmtime, WasmEdge, Fastly Compute) with filesystem access — bringing the same engine to server-side WASM environments.
