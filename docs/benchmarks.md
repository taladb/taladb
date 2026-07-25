---
title: Benchmarks
description: TalaDB performance benchmarks — on-device document, query, and vector-search timings for the browser (WASM + OPFS) and Node.js (native), measured with the open-source suites in scripts/.
---

# Benchmarks

Real numbers, measured honestly. All results below come from the two benchmark suites that ship in the repository, so you can reproduce every row on your own hardware:

```bash
# Browser (WASM + OPFS — drives your installed Chrome headlessly)
pnpm --filter @taladb/web build
pnpm bench:web

# Node.js (native module)
pnpm --filter @taladb/node build
pnpm bench
```

## Setup

| | |
|---|---|
| TalaDB | v0.9.4 browser release build; v0.9.0 Node baseline |
| Machine | 2018 MacBook Pro — Intel i5-8259U @ 2.30 GHz, 8 GB RAM |
| Browser runtime | Chrome 150 headless · `@taladb/web` (WASM) · OPFS-backed |
| Node.js runtime | Node v22 · `@taladb/node` (napi) · file-backed database |
| Date | Browser re-run 2026-07-13; Node baseline 2026-07-11 |

Latencies are the **median** of repeated timed iterations after warmup, with deterministic seeded data. Documents are realistic small records (~7 fields); vectors are 384-dimensional unit vectors — the output shape of `all-MiniLM-L6-v2`, the most common on-device embedding model. The browser suite drives the `@taladb/web` worker over its message protocol; the Node suite uses the raw N-API binding. In both cases that is the same path the `taladb` wrapper uses, so timings include everything an application pays.

This is deliberately modest hardware. On a recent Apple Silicon or desktop-class machine, expect meaningfully better numbers across the board.

# Browser — WASM + OPFS

The browser is TalaDB's flagship runtime: the same Rust engine compiled to WebAssembly, persisting to the Origin Private File System from a Web Worker. Every timing below includes the page ↔ worker `postMessage` round-trip and JSON serialisation — the full cost an app pays. Measured in real headless Chrome with OPFS active.

## Browser — document writes

The browser engine is redb running directly on an OPFS file, fsync-flushing every commit by default — so single writes carry a per-commit flush cost (hence ~900 ops/s) while batched `insertMany` amortises one flush across the batch. See the durability note below for how to trade that for throughput.

| Operation | Detail | Result |
|---|---|---|
| `insert` (single doc) | one transaction per call | **~900 ops/s** |
| `insertMany` (batch 100) | single transaction per batch | **~27k docs/s** |
| `insertMany` (batch 1,000) | single transaction per batch | **~43k docs/s** |
| `insertMany` (batch 5,000) | bulk ingest of 100k docs | **~57k docs/s** |
| `updateOne` (by `_id`) | point update, `$set` one field | **~714 ops/s** |
| `deleteOne` (by `_id`) | point delete | **~667 ops/s** |

::: warning Browser durability model
By default the browser OPFS engine `fsync`s (flushes the OPFS access handle) on **every commit** — the same per-commit durability as Node.js — so a hard crash does not lose acknowledged writes. That per-commit flush is what the single-write number above measures. To trade it for throughput, open with `durability: { flush_every_write: false }` (redb *Eventual* — commits are batched) and call `await db.flush()` at "save now" moments (before checkout, on `visibilitychange`). Independently, the worker also writes a **500 ms-debounced** snapshot to *IndexedDB* — but that is only the auxiliary copy other tabs read and the offline fallback; it is not the authoritative store on the OPFS path, and its debounce is tunable via `durability: { flush_ms }`.
:::

## Browser — query latency at 100,000 documents

| Operation | Detail | Result |
|---|---|---|
| `findOne` by `_id` | primary-key point get | **100 µs** |
| `find`, indexed equality | secondary index, ~10 matches | **300 µs** |
| `find`, indexed range (`$gte`) | newest ~100 docs | **800 µs** |
| `find`, unindexed field | full scan of 100k docs | **168 ms** |
| `count`, unindexed equality | scan, 12.5k matches | **175 ms** |

Sub-millisecond operations pay the worker `postMessage` round-trip (~50–100 µs), so browser point reads land around 100–300 µs — still far below anything network-bound. Scans are actually *faster* than Node's here because the memory-resident engine reads no disk pages.

## Browser — vector search (384-dim, cosine, top-10, flat)

Exact k-nearest-neighbour over all vectors — no approximation, no recall trade-off.

Re-run twice with the v0.9.4 release build on the same machine. The values below use the second run; the first measured 3.3 ms, 17.2 ms, and 87.1 ms respectively, confirming the improvement is repeatable. Against the v0.9.0 baseline, latency is 36% lower at 1k vectors, 52% lower at 10k, and 50% lower at 50k.

| Collection size | `findNearest` (median) |
|---|---|
| 1,000 vectors | **3.4 ms** |
| 10,000 vectors | **16.7 ms** |
| 50,000 vectors | **84.6 ms** |

| Operation | Detail | Result |
|---|---|---|
| `findNearest` + filter, 50k vectors | indexed pre-filter `locale: "en"` (10%), then rank | **123 ms** |
| Vector ingest, 50k vectors | `insertMany` with a live vector index | **~2.4k docs/s** |

The filtered vector query is 24% lower-latency than the v0.9.0 result (162 ms). Vector ingest did not improve, so its baseline remains unchanged.

Semantic search over a typical on-device corpus (1k–10k chunks) answers in **~17 ms or less** — faster than a network round-trip to any cloud vector database, with zero data leaving the device.

::: tip Near-native flat-search parity
The v0.9.4 browser release build measures **84.6 ms at 50k vectors**, close to Node's 93 ms baseline on the same hardware. This is a measured release-build result, not an approximate-index result: both rows use the exact flat index with identical 384-dimensional queries.
:::

# Node.js — native

The `@taladb/node` native module (napi), against a file-backed database — every committed write is `fsync`-durable when the call returns.

A v0.9.4 re-run on 2026-07-13 showed no material native improvement (26 µs point reads and 200 ms exact search at 100k, versus 25 µs and 198 ms below), so the established v0.9.0 Node baseline is retained rather than replacing it with run-to-run noise.

## Node.js — document writes

| Operation | Detail | Result |
|---|---|---|
| `insert` (single doc) | one transaction per call | **~48 ops/s** |
| `insertMany` (batch 100) | single transaction per batch | **~4.0k docs/s** |
| `insertMany` (batch 1,000) | single transaction per batch | **~19k docs/s** |
| `insertMany` (batch 5,000) | single transaction per batch | **~36k docs/s** |
| `updateOne` (by `_id`) | point update, `$set` one field | **~46 ops/s** |
| `deleteOne` (by `_id`) | point delete | **~46 ops/s** |

::: tip Batch your writes
Every individual write is a full ACID transaction — the ~48 ops/s ceiling for single-document calls is the cost of a durable `fsync` to disk, not of TalaDB's write path. The same machine sustains **36k docs/s** when writes share a transaction. If you are inserting more than a handful of documents, use `insertMany`.
:::

## Node.js — query latency at 100,000 documents

| Operation | Detail | Result |
|---|---|---|
| `findOne` by `_id` | primary-key point get | **25 µs** |
| `find`, indexed equality | secondary index, ~10 matches | **169 µs** |
| `find`, indexed range (`$gte`) | newest ~100 docs by `publishedAt` | **1.4 ms** |
| `find`, two-sided range (`$gte`+`$lt`) | ~100-doc `publishedAt` window | **1.4 ms** |
| `find`, unindexed field | full scan of 100k docs | **444 ms** |
| `count`, unindexed equality | scan, 12.5k matches | **470 ms** |

Point gets and indexed lookups stay in the microsecond range at 100k documents — the B-tree index layout gives `O(log n)` lookups regardless of collection size. Since v0.9.0 a **two-sided range is a single bounded index scan** (the ~100-doc window costs the same 1.4 ms as the one-sided form, down from ~463 ms in v0.8.x). Unindexed queries fall back to a full scan; if a field appears in your filters regularly, `createIndex` turns a 444 ms scan into a 169 µs lookup — a **~2,600×** difference.

## Node.js — vector search (384-dim, cosine, top-10)

The default (flat) index is exact k-NN over all vectors. The v0.9.0 scan rewrite (byte-streaming scoring, hoisted query norm, partial top-k selection) made it roughly **2× faster** than earlier releases:

| Collection size | `findNearest` (median) | v0.8.x |
|---|---|---|
| 1,000 vectors | **2.5 ms** | 4.0 ms |
| 10,000 vectors | **18 ms** | 40 ms |
| 50,000 vectors | **93 ms** | 188 ms |
| 100,000 vectors | **198 ms** | 369 ms |

Filtered vector search — metadata pre-filter, then rank — is cheaper still, because filtered-out vectors are skipped before scoring when the filter field is indexed:

| Operation | Detail | Result |
|---|---|---|
| `findNearest` + filter, 100k vectors | indexed pre-filter matching 10% of docs, then rank | **346 ms** |
| Vector ingest, 100k vectors | `insertMany` with a live vector index | **~4.5k docs/s** |

::: tip Index your filter fields
The pre-filter is an ordinary document query, so it benefits from secondary indexes exactly like `find` does — index the field you filter on.
:::

### Optional HNSW index (Node.js, since 0.8.3)

For larger corpora, `@taladb/node` ships an approximate HNSW index (`createVectorIndex(field, { dimensions, indexType: 'hnsw' })`):

| Metric | 50,000 × 384-dim vectors |
|---|---|
| `findNearest` (HNSW) | **15 ms** (vs 93 ms flat — ~6× faster) |
| Graph build (one-off) | **~36 min** on this hardware (47 s at 10k) |
| recall@10, uniform random vectors | 41% — the adversarial worst case |
| recall@10, clustered vectors (embedding-like) | **100%** |

Read the recall rows carefully: uniform random vectors have no neighbourhood structure and are the known worst case for graph-based ANN. Real model embeddings are strongly clustered, where HNSW recall is excellent — but measure on *your* data before relying on it. Two operational caveats: the graph is built at `createVectorIndex` / `upgradeVectorIndex` time and is **not** updated by later writes (rebuild during idle periods after bulk ingests), and graph construction is CPU-intensive and superlinear — plan the one-off build cost. The flat index stays the right default for most on-device corpora; `@taladb/web` and React Native are currently flat-only.

## Reading these numbers

- **Scaling** — exact vector search is linear in collection size; document point lookups are logarithmic. Both behave predictably as your data grows.
- **Latency floor, not ceiling** — the test machine is a 2018 dual-fan ultrabook. Treat these as conservative.
- **Durability is per-commit on both platforms by default** — Node.js and the browser OPFS engine both `fsync` every commit (see the note above); opt into batched commits with `durability: { flush_every_write: false }` + `db.flush()` for throughput. The 500 ms debounce is only the browser's auxiliary IndexedDB snapshot, not the OPFS store.
- **Browser vs native** — the v0.9.4 browser release build and native baseline are now near parity for flat vector search on this machine (84.6 ms vs 93 ms at 50k). Browser scans are also faster here; point operations are broadly comparable after the worker round-trip.
- **React Native — not yet benchmarked.** RN runs the same Rust core via JSI with a file-backed database (like Node.js), so expect broadly Node-like numbers scaled to the device CPU — but these are an *expectation, not a measurement*. A device-driven suite (running inside an app on a simulator/emulator) is planned; until it lands, there are deliberately no RN figures here.
- **Methodology** — deterministic seeded data, warmup before measurement, medians reported, one process at a time on an otherwise idle machine. Read [`scripts/bench-web.mjs`](https://github.com/thinkgrid-labs/taladb/blob/main/scripts/bench-web.mjs) + [`scripts/bench-web/bench.browser.js`](https://github.com/thinkgrid-labs/taladb/blob/main/scripts/bench-web/bench.browser.js) (browser) and [`scripts/bench.mjs`](https://github.com/thinkgrid-labs/taladb/blob/main/scripts/bench.mjs) (Node) for the exact workloads.
