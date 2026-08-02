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
| TalaDB | v0.10.2 Node (full re-run); v0.9.4 browser release build |
| Machine | 2018 MacBook Pro — Intel i5-8259U @ 2.30 GHz, 8 GB RAM |
| Browser runtime | Chrome 150 headless · `@taladb/web` (WASM) · OPFS-backed |
| Node.js runtime | Node v22 · `@taladb/node` (napi) · file-backed database |
| Date | Node re-run 2026-08-02; browser re-run 2026-07-13 |

::: warning Browser figures predate 0.10.2
The browser tables below were measured against v0.9.4 and have **not** been
re-run since. The 0.10.2 engine changes are runtime-agnostic — they live in the
shared Rust core — so the browser numbers almost certainly understate current
performance, but that is an inference, not a measurement. They will be refreshed
when the browser suite is next run.
:::

Latencies are the **median** of repeated timed iterations after warmup, with deterministic seeded data. Documents are realistic small records (~7 fields); vectors are 384-dimensional unit vectors — the output shape of `all-MiniLM-L6-v2`, the most common on-device embedding model. The browser suite drives the `@taladb/web` worker over its message protocol; the Node suite uses the raw N-API binding. In both cases that is the same path the `taladb` wrapper uses, so timings include everything an application pays.

This is deliberately modest hardware. On a recent Apple Silicon or desktop-class machine, expect meaningfully better numbers across the board.

# Browser — WASM + OPFS

The browser is TalaDB's flagship runtime: the same Rust engine compiled to WebAssembly, persisting to the Origin Private File System from a Web Worker. Every timing below includes the page ↔ worker `postMessage` round-trip and JSON serialisation — the full cost an app pays. Measured in real headless Chrome with OPFS active.

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

::: tip Measured, not approximated
The v0.9.4 browser release build measures **84.6 ms at 50k vectors**. This is a
measured release-build result, not an approximate-index result: it uses the exact
flat index with 384-dimensional queries.

This row used to be described as near-parity with Node's 93 ms. That comparison
is now stale — the 0.10.2 scoring rewrite took native flat search at 50k to
33 ms, and the browser has not been re-measured since v0.9.4. The two are
currently different engine versions, not a browser-vs-native result.
:::

## Browser — cold start

Before a browser tab can answer its first query it must download, decode and
compile the WASM module, then open the OPFS file. 0.10.2 reduced both halves.

| | 0.10.1 | 0.10.2 | Change |
|---|---|---|---|
| `taladb_web_bg.wasm`, raw | 2,009,406 B | **1,694,846 B** | −15.7% |
| gzip (`-9`) | 748,583 B | **635,567 B** | −15.1% |
| brotli (`-q 11`) | 548,354 B | **471,161 B** | −14.1% |

The payload shrank by building the browser bundle without the YAML config parser
and the HTTP client crate (a single JSON `POST` now goes through the platform
`fetch` directly), by dropping a URL-parsing dependency whose Unicode tables were
carried for a scheme-and-host check, and by optimising the module for size. Link-time
dead-code elimination had already removed much of the unused code, which is why
the saving is ~15% rather than the larger figure the dependency count suggests.

Three sequencing changes also removed work from the critical path, though they
are not captured by a byte count:

- The worker starts fetching and compiling the WASM at module scope instead of
  waiting for the page's `init` message, so the download overlaps the worker
  spawn round-trip rather than following it.
- WASM compilation and OPFS setup now run concurrently. OPFS setup is four
  awaited round-trips (`getDirectory` → `getFileHandle` → lock →
  `createSyncAccessHandle`), which previously ran *after* the compile finished.
- The OPFS capability check no longer creates, opens, closes and deletes a probe
  file on every start — four more round-trips whose only purpose was to discover
  something the real open reveals anyway.

::: warning Not yet measured end-to-end
The byte counts above are measured. The wall-clock effect of the sequencing
changes has **not** been measured in a browser — doing so needs the headless
suite, which has not been re-run for 0.10.2. Treat the payload reduction as fact
and the sequencing as a reasoned improvement pending measurement.
:::

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

# Node.js — native

The `@taladb/node` native module (napi), against a file-backed database — every committed write is `fsync`-durable when the call returns.

Every Node row below was re-measured on **0.10.2** (2026-08-02). The comparison is controlled: the 0.10.1 binary and a freshly built 0.10.2 binary ran the same suite back-to-back on the same machine within minutes of each other. The 0.10.1 pass reproduced the previously published table within a few percent (filtered vector search 199 ms exactly, `findOne` 24 vs 25 µs, unindexed scan 452 vs 444 ms), which is what makes the deltas below trustworthy rather than an artefact of a different day.

## Node.js — vector search (384-dim, cosine, top-10)

The default (flat) index is exact k-NN over all vectors — no approximation, no recall trade-off. **0.10.2** rewrote the scoring loop: the query vector's L2 norm is constant across every candidate, so it is computed once instead of per candidate, and the dot product and the stored vector's norm are accumulated in a **single pass** over each vector rather than three separate ones. The second effect dominates — it is far friendlier to cache and autovectorisation:

| Collection size | `findNearest` (0.10.2) | 0.10.1 | v0.9.0 | v0.8.x |
|---|---|---|---|---|
| 1,000 vectors | **1.9 ms** | 3.0 ms | 2.5 ms | 4.0 ms |
| 10,000 vectors | **7.7 ms** | 18 ms | 18 ms | 40 ms |
| 50,000 vectors | **33 ms** | 85 ms | 93 ms | 188 ms |
| 100,000 vectors | **65 ms** | 174 ms | 198 ms | 369 ms |

That is a **~63% reduction at 100k vectors** on top of the 0.10.1 decoded-vector cache, and the gain grows with collection size because the scan is exactly where the time goes.

| Operation | Detail | Result |
|---|---|---|
| `findNearest` + filter, 100k vectors | indexed pre-filter `locale: "en"` (10%), then rank | **171 ms** *(0.10.1: 199 ms)* |
| Vector ingest, 100k vectors | `insertMany` with a live vector index | **~4.0k docs/s** *(0.10.1: 3.6k)* |

Filtered vector search is the on-device RAG path — the metadata pre-filter narrows the candidate set, then only matching vectors are scored. It improves less than unfiltered search (−14%) because a large share of its time is the pre-filter query, not the scoring loop.

::: tip Index your filter fields
The pre-filter is an ordinary document query, so it benefits from secondary indexes exactly like `find` does — index the field you filter on.
:::

### Optional HNSW index (Node.js, since 0.8.3)

For larger corpora, `@taladb/node` ships an approximate HNSW index (`createVectorIndex(field, { dimensions, indexType: 'hnsw' })`):

| Metric | 50,000 × 384-dim vectors |
|---|---|
| `findNearest` (HNSW) | **15 ms** *(measured on v0.9.0)* |
| `findNearest` (flat, 0.10.2) | 33 ms — so HNSW is now ~2.2× faster, not ~6× |
| Graph build (one-off) | **~36 min** on this hardware (47 s at 10k) |
| recall@10, uniform random vectors | 41% — the adversarial worst case |
| recall@10, clustered vectors (embedding-like) | **100%** |

::: warning HNSW's advantage narrowed sharply in 0.10.2
This row previously read "15 ms vs 93 ms flat — ~6× faster". Flat search has
since dropped to 33 ms at 50k, so the same 15 ms HNSW figure is now only ~2.2×
faster. The HNSW number itself has **not** been re-measured on 0.10.2 (the graph
build takes ~36 minutes, so it is excluded from routine runs) — but since the
scoring rewrite speeds up the flat scan, not the graph walk, the ratio above is
the honest reading. Weigh a ~36-minute one-off build and a graph that goes stale
on every write against a 2× query gain before reaching for it.
:::

Read the recall rows carefully: uniform random vectors have no neighbourhood structure and are the known worst case for graph-based ANN. Real model embeddings are strongly clustered, where HNSW recall is excellent — but measure on *your* data before relying on it. Two operational caveats: the graph is built at `createVectorIndex` / `upgradeVectorIndex` time and is **not** updated by later writes (rebuild during idle periods after bulk ingests), and graph construction is CPU-intensive and superlinear — plan the one-off build cost. The flat index is now the right default for a wider range of corpora than before; `@taladb/web` and React Native are flat-only in any case.

## Node.js — document writes

| Operation | Detail | Result |
|---|---|---|
| `insert` (single doc) | one transaction per call | **~46 ops/s** |
| `insertMany` (batch 100) | single transaction per batch | **~3.9k docs/s** |
| `insertMany` (batch 1,000) | single transaction per batch | **~20k docs/s** |
| `insertMany` (batch 5,000) | single transaction per batch | **~39k docs/s** |
| `updateOne` (by `_id`) | point update, `$set` one field | **~46 ops/s** |
| `deleteOne` (by `_id`) | point delete | **~46 ops/s** |

Single-document writes are unchanged by 0.10.2 and will stay that way: they are bounded by the per-commit `fsync`, not by anything the engine does. Batched writes improved (bulk ingest 33.6k → **39.2k docs/s**, +17%) because index maintenance now resolves each storage table once per batch instead of once per document per index.

::: tip Batch your writes
Every individual write is a full ACID transaction — the ~46 ops/s ceiling for single-document calls is the cost of a durable `fsync` to disk, not of TalaDB's write path. The same machine sustains **39k docs/s** when writes share a transaction. If you are inserting more than a handful of documents, use `insertMany`.
:::

## Node.js — query latency at 100,000 documents

| Operation | Detail | Result | 0.10.1 |
|---|---|---|---|
| `findOne` by `_id` | primary-key point get | **25 µs** | 24 µs |
| `find`, indexed equality | secondary index, ~10 matches | **174 µs** | 194 µs |
| `find`, indexed range (`$gte`) | newest ~100 docs by `publishedAt` | **1.39 ms** | 1.59 ms |
| `find`, two-sided range (`$gte`+`$lt`) | ~100-doc `publishedAt` window | **1.40 ms** | 1.60 ms |
| `find`, unindexed field | full scan of 100k docs | **317 ms** | 452 ms |
| `count`, unindexed equality | scan, 12.5k matches | **324 ms** | 477 ms |

Point gets and indexed lookups stay in the microsecond range at 100k documents — the B-tree index layout gives `O(log n)` lookups regardless of collection size. Since v0.9.0 a **two-sided range is a single bounded index scan** (the ~100-doc window costs the same as the one-sided form, down from ~463 ms in v0.8.x). Unindexed queries fall back to a full scan; if a field appears in your filters regularly, `createIndex` turns a 317 ms scan into a 174 µs lookup — a **~1,800×** difference.

**What 0.10.2 changed here.** Scans are ~30% faster because a range walk now hands the query engine borrowed keys and values straight out of the page cache instead of copying every entry into a freshly allocated pair of `Vec`s and materialising the whole range before the first row is examined. Indexed lookups improved because fetching *N* documents opens the storage table once rather than *N* times — an overhead that had grown to dominate multi-result lookups. Two query shapes changed complexity rather than constant factor, so they are not in the table above:

- **`findOne` and bounded `find` stop at the page they need.** Previously both materialised the entire result set and then discarded all but the first rows. An unindexed `findOne` over 100k documents used to cost a full scan; it now costs one match. On a 20k-document collection this measured 93 ms → 125 µs, and on an indexed equality 109 ms → 28 µs.
- **`$and` over two indexed equalities intersects the indexes.** The planner previously chose the first indexed conjunct and post-filtered the rest, so `status = "active" AND city = "x"` scanned every active row. It now intersects id sets and decodes only documents satisfying both — 109 ms → 4.9 ms on a 20k collection where the first conjunct matched everything.

## Reading these numbers

- **Scaling** — exact vector search is linear in collection size; document point lookups are logarithmic. Both behave predictably as your data grows.
- **Latency floor, not ceiling** — the test machine is a 2018 dual-fan ultrabook. Treat these as conservative.
- **Durability is per-commit on both platforms by default** — Node.js and the browser OPFS engine both `fsync` every commit (see the note above); opt into batched commits with `durability: { flush_every_write: false }` + `db.flush()` for throughput. The 500 ms debounce is only the browser's auxiliary IndexedDB snapshot, not the OPFS store.
- **Browser vs native** — these were at near parity for flat vector search when both were last measured together (84.6 ms browser vs 93 ms native at 50k). That is no longer a fair comparison: 0.10.2 took native flat search at 50k to 33 ms, and the browser suite has not been re-run since v0.9.4. Since the change is in the shared Rust core, the browser should see a comparable gain — but until the browser suite is re-run, the two columns are measuring different engine versions and should not be compared.
- **React Native — not yet benchmarked.** RN runs the same Rust core via JSI with a file-backed database (like Node.js), so expect broadly Node-like numbers scaled to the device CPU — but these are an *expectation, not a measurement*. A device-driven suite (running inside an app on a simulator/emulator) is planned; until it lands, there are deliberately no RN figures here.
- **Methodology** — deterministic seeded data, warmup before measurement, medians reported, one process at a time on an otherwise idle machine. Read [`scripts/bench-web.mjs`](https://github.com/taladb/taladb/blob/main/scripts/bench-web.mjs) + [`scripts/bench-web/bench.browser.js`](https://github.com/taladb/taladb/blob/main/scripts/bench-web/bench.browser.js) (browser) and [`scripts/bench.mjs`](https://github.com/taladb/taladb/blob/main/scripts/bench.mjs) (Node) for the exact workloads.
