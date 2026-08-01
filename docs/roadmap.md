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

### Sync

- 🟡 **Bidirectional sync — React Native** *(implemented, pending on-device verification)* — the changeset primitives (`exportChanges` / `importChanges` / `listCollectionNames`) are now wired through the full stack: Rust FFI → C header → JSI HostObject (C++) → the TS adapter, which feature-detects them and enables the same `db.sync()` used on Node/web (falling back to a clear error on older binaries). The Rust FFI and TS layers are compiled/typechecked; **the JSI native glue has not yet been built or run on a device/simulator** — that verification (iOS Simulator + Android emulator) is the remaining gate before it's marked shipped. Still to follow: an `AppState`-driven sync example and background-sync integration docs (iOS BGTaskScheduler / Android WorkManager via e.g. `react-native-background-fetch`) — mobile background execution is OS-scheduled, so the guide will teach "opportunistic background catch-up, guaranteed reconciliation on launch".
- 🟡 **Schema evolution + migrations on React Native — pending on-device verification** — the validate-on-import path (`importChangesValidated` / `quarantined`) and the `openDB({ migrations })` version accessors (`userVersion` / `setUserVersion`) are now wired through the full React Native stack — Rust FFI, C header, and the JSI HostObject (C++) — and feature-detected by the TS client so an older native module degrades gracefully. As with RN bidirectional sync, the Rust and TS layers compile/typecheck but the JSI native glue has **not** yet been built or exercised on a device or simulator; that verification (iOS + Android) is the remaining gate. *(Read-time `migrateDocument` + `persistMigrations`, structural `syncSchema.renames`, validate-on-import, and `_v` upgrades ship on browser + Node — see [Schema & Sync Standards](/guide/schema-and-sync-standards).)*
- **Application schema migrations — React Native + optional transactional mode** — expose the `userVersion`/`setUserVersion` accessors on the React Native JSI HostObject (the Rust FFI is already in place), and add an optional whole-batch-atomic mode once a multi-write transaction primitive exists in the high-level API. *(`openDB({ migrations })` already ships on browser + Node with per-version checkpointing — see [Migrations](/api/migrations).)*
- **Server-assigned sync sequence cursor** — pull filtering currently relies on `changed_at` timestamps (see the two-watermark design in the guide); a server-assigned monotonic sequence would make pull cursors fully robust against clock skew between peers. Requires a small `SyncAdapter` contract extension.
- **HTTP sync — configurable push batching & pull interval** — today neither direction is coalescible. The Rust-core `SyncConfig` / `HttpSyncHook` (see [HTTP Push Sync](/guide/http-sync)) POSTs on *every* committed write with no debounce or flush window — the only existing timing knob is per-request retry backoff (200/400/800 ms × 3), which is unrelated. The JS `HttpSyncAdapter` (see [Bidirectional Sync](/guide/bidirectional-sync)) has no polling loop of its own at all; every guide example hand-rolls `setInterval(syncNow, 30_000)`, and `@taladb/next/client`'s `<SyncProvider interval={30_000}>` just wraps that same pattern one layer up rather than exposing it from core. Proposed:
  - Push: a `flushMs` option on `SyncConfig`, mirroring the existing storage-layer `durability.flushMs` knob — coalesce writes inside the window into one POST instead of one-per-write. Default `flushMs: 0` keeps today's immediate behavior so this is additive, not breaking.
  - Pull: a `pollMs` option so `db.sync()` (or a new `db.startSync()`) owns a cancellable, cron-like interval loop instead of every app re-implementing `setInterval`. No default exists anywhere today — ship one (30 s matches the guide's own example and `SyncProvider`'s default) so `pollMs` works unset.
- Native NoSQL adapters — for **server-side** TalaDB, sync directly to a database with no intermediate API. (Browser/mobile apps still relay through your own API — a database credential must never reach a client.) `@taladb/sync-mongodb` shipped; `@taladb/sync-firestore` and `@taladb/sync-dynamodb` are next, same `SyncAdapter` interface.

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

### First-party sync backend adapters

Thin adapter packages that implement the `SyncAdapter` interface for popular backends, so no custom server is required:

- **`@taladb/sync-recached`** — uses [Recached](https://recached.dev), our sibling sync fabric, as the transport. Unlike the poll-based adapters below, this one is push-native: scoped live queries deliver changes in real time, and Recached's durable outbox provides offline queueing and exactly-once delivery, so the adapter inherits reconnect and conflict handling rather than reimplementing them. The flagship adapter for real-time, multi-user apps.
- **`@taladb/sync-supabase`** — uses a Supabase table + Realtime channel as the changeset transport
- **`@taladb/sync-turso`** — writes changesets to a Turso (libSQL) table; useful for Electron and server-side apps
- **`@taladb/sync-d1`** — Cloudflare D1 table as the sync relay; pairs naturally with `@taladb/cloudflare`

Each adapter handles auth, changeset serialisation, and incremental polling/push. CRDT or LWW merge is still applied client-side.

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

## 3 · Advanced sync

### Sync over WebSockets

The CRDT merge protocol (field-level logical clocks, `CrdtSyncAdapter`) shipped in **v0.7.11**. What remains is the transport: a reference sync server (`taladb-sync-server`) that accepts changesets over a persistent WebSocket connection and fans them out to connected peers, enabling multi-device sync without a managed cloud database. Merge logic stays client-side via `import_crdt_changes`.

---

## 4 · Storage

Internal improvements that improve efficiency and interoperability.

### Pluggable serialisation

Allow the caller to swap `postcard` for `MessagePack` or `CBOR` via a `Codec` trait, making it easier to interoperate with databases or wire formats that already use those encodings.

### Document TTL (time-to-live)

Set an expiry on any document at write time:

- `collection.insert({ ...doc, _ttl: Date.now() + 60_000 })` — document auto-deleted after the TTL elapses
- Background reaper runs on a configurable interval (default: 60 s in Node.js, on next open in browser)
- Tombstone generated for TTL deletions so expiry propagates correctly through sync

---

## 5 · Platform

Expanding the runtimes TalaDB can target.

### Swift / Kotlin native packages

First-party Swift (`TalaDB.swift`) and Kotlin (`taladb-kotlin`) packages that wrap the C FFI layer directly, without React Native, for native iOS and Android apps that want an embedded document store.

### WASI target

Compile `taladb-core` to WASI (`wasm32-wasip1`) so it can run inside WASI runtimes (Wasmtime, WasmEdge, Fastly Compute) with filesystem access — bringing the same engine to server-side WASM environments.
