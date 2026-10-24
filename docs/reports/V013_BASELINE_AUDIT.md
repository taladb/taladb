# v0.13.0 baseline audit

Audit date: 2026-10-24. Subject: **TalaDB v0.12.1**, commit `a2ef46602127919c60a0fe6333d676b1b1ab391e`.
Companion documents: [V013_BENCHMARK_PLAN.md](V013_BENCHMARK_PLAN.md) and [V013_OPTIMIZATION_ROADMAP.md](V013_OPTIMIZATION_ROADMAP.md). Raw outputs are in [`artifacts/v013-baseline/`](artifacts/v013-baseline/).

This audit inventories the performance, memory, correctness and compatibility evidence that exists for v0.12.1, measures v0.12.1 where evidence was missing, and records what remains unverified. It changes no engine code. Every measurement below was taken on a detached worktree of the tagged commit; nothing in the user's checkout was built, modified or committed.

## 1. Baseline identity

| Item | Value |
|---|---|
| Tag | `v0.12.1` — a **lightweight** tag; `dev`, `main` and the tag all point at `a2ef466` as of this audit |
| Commit | `a2ef46602127919c60a0fe6333d676b1b1ab391e` (2026-10-22, "chore: changelogs for 0.12.1") |
| `Cargo.lock` SHA-256 | `67ab983eb53ba4aa9c7f0533e322a76bd8cf28b47c39f8300be83dc884e1541d` |
| Toolchain pin | `rust-toolchain.toml`: 1.98.0; MSRV 1.90 |
| Release profile | `opt-level 3`, `lto = true`, `codegen-units = 1`, `overflow-checks = true`; `bench` adds `debug = true` |
| Published artifacts | crates.io `taladb` 0.12.1, five npm packages, GitHub release (release run 39798655385, attempt 2) |
| Native wrappers | `taladb-kotlin` and `taladb-swift` `main` (on GitHub) still pin engine **v0.12.0**; the bump PRs "Build against TalaDB engine v0.12.1" (taladb-kotlin #7, taladb-swift #6) are open. The wrappers have therefore never shipped against the baseline engine |

**Immutability.** A lightweight tag can be moved or deleted without a trace. Gates and reports must name the SHA `a2ef466…`, and the cumulative-drift job should check out by SHA. If an annotated anchor is wanted, create `bench-baseline/v0.13` on the same SHA; this audit did not create one.

**Where this evidence lives.** `docs/reports/` is listed in the repository's `.gitignore`, so these reports and their artifacts are local files. CI benchmark artifacts expire after 90 days; the v0.12.1 release run's JSON (artifact IDs 12434448218–12436070045) expires around 2027-01-20. Both are reasons the plan asks for baseline JSON to be attached to the GitHub release.

## 2. Existing infrastructure

### 2.1 Rust benchmarks and profiling harnesses (`packages/core`)

| Harness | Workloads | Backend | Runs in CI | Notes |
|---|---|---|---|---|
| `benches/engine.rs` (Criterion) | `vector/score_one` (3 metrics × 4 dims), `vector/find_nearest` 1k/10k at 384-d, `query/*` unindexed scans on 10k, `write/insert_many_1k`, `write/export_snapshot_10k` | in-memory | **No** — compiled by `clippy --all-targets`, never executed | `query/equality` is an *unindexed* scan; no index is created in `bench_query` |
| `benches/support/stabilization.rs` | 1k/10k (100k with `TALADB_BENCH_LARGE`): indexed page, sorted page, unindexed selective, BM25, BM25 + filter, `update_many`, index cycle, FTS cycle, `delete_many`, exact vector, hybrid, HNSW (8-d) | in-memory | No | Deterministic IDs; `TALADB_BENCH_SAMPLES` and `TALADB_BENCH_FLAT` controls from the stabilization review |
| `examples/hnsw_profile.rs` | Graph build (sync and resumable), exact mean, ANN sweep `efSearch` 50–400 with recall@10 | in-memory | Yes (via `vector-bench-ci.py`) | 384-d clustered synthetic vectors, seedable |
| `examples/vector_filter_profile.rs` | Exact and ANN filtered search, sparse/moderate/dense/range, cold/warm | in-memory | Yes | Emits fingerprints and distance counts |
| `examples/vector_memory_profile.rs` | 1/2/4 concurrent ANN searches at 1 MiB / 8 MiB cache budgets | in-memory | Yes (report only) | 20 queries per thread; asserts accounting invariants |
| `examples/stabilization_memory.rs` | 10k × 8 KiB docs: index / update / delete, RSS checkpoints per phase | in-memory | No | Used with `/usr/bin/time -v`, Valgrind, Massif in the stabilization reports |
| `examples/open_table_cost.rs` | redb `open_table` cost on a write transaction | file | No | One-off investigation |

### 2.2 Scripts and CI

| Item | What it does | Gate |
|---|---|---|
| `scripts/vector-bench-ci.py` | Builds baseline and candidate worktrees with separate target dirs, runs the **candidate's** harness on both, compares | Build +30% and +500 ms; exact +30% and +0.25 ms; ANN/filtered p50 +50% and +0.25 ms; recall −3 pp |
| `.github/workflows/performance.yml` `compare` | `ubuntu-latest` and `ubuntu-24.04-arm` × quantization `none`/`binary`: HNSW sync, resumable, filtered (none only), memory profile | PR: 2k vectors × 3 repeats. **Push, schedule, release: 10k × 1 repeat** |
| `performance.yml` `browser` | wasm-pack builds of both sides, headless Chromium, worker + OPFS vector suite at 1 MiB and 8 MiB budgets | Build +30%/+500 ms, warm p50 +50%/+0.5 ms, recall −3 pp, cache accounting |
| `release.yml` | Calls `performance.yml` as `vector-performance`; every publish job depends on it | Same as above |
| `scripts/bench.mjs` | Node binding, **file-backed**: single/batched inserts, `updateOne`/`deleteOne`, 100k-doc queries, flat vector search to 100k, HNSW at 50k | Manual only |
| `scripts/bench-web.mjs` + `bench-web/bench.browser.js` | Same document workloads through the browser worker and OPFS | Manual only (vector mode in CI) |
| `packages/bindings/web/e2e` | Real Chromium, nine phases; phase 5 prints performance with loose ceilings | CI runs phases **1, 2, 3, 6** only; 4, 5, 7, 8, 9 are manual |

### 2.3 Correctness and compatibility checks

| Check | Scope | CI |
|---|---|---|
| `cargo test --workspace --all-features` | 47 core integration test files (≈ 500 `#[test]`s) plus ≈ 150 unit tests in `src/`, bindings, CLI | Yes |
| Stabilization regressions | `stabilization.rs`, `fts_stats_integrity.rs`, `snapshot.rs`, `concurrent_consistency.rs`, `parser_parity.rs` + shared fixture | Yes |
| WASM tests | `cargo test -p taladb-web --target wasm32-unknown-unknown` under Node | Yes |
| Miri | `taladb-ffi` `miri_safe` subset, Stacked and Tree Borrows | Yes |
| Fuzz | Five targets (filter, filter parser, update parser, pipeline, snapshot), time-boxed smoke | Yes (smoke only) |
| MSRV, `cargo audit --deny warnings` | 1.90; RustSec | Yes |
| Format interchange | Baseline ↔ candidate open/restore, plain and encrypted, 14 cases | Manual (`artifacts/STABILIZATION_RELEASE_COMPAT.rs`), not in CI |
| Kotlin wrapper | 52 host JNI tests, AAR checks, lint; instrumentation on a device | Kotlin repo CI; device manual |
| Swift wrapper | 43 XCTests on Linux; macOS/iOS job in Swift repo CI | Swift repo CI |

### 2.4 Native bindings and apps

Neither `taladb-kotlin` nor `taladb-swift` has a benchmark. Their only performance-sensitive tests are the live-query regressions (`aLiveQueryNeverHoldsTheCollectorsThread`, `aNewLiveQueryDoesNotWaitBehindRunningOnes`). Device-level evidence comes from Notewell's `diagnostics/Probe.kt` and manual Kepta runs on the user's Android device — never the Play build. There is no iOS device measurement of any kind.

Concurrency differs by binding, which matters for which concurrent workloads are representative:

| Binding | Engine calls |
|---|---|
| Kotlin | Under a `ReentrantReadWriteLock` read lock → concurrent reads |
| Swift | `DispatchQueue(attributes: .concurrent)`, barrier for close → concurrent reads |
| Node | 29 `AsyncTask`s on the libuv pool → concurrent |
| Browser | One worker → serialised; live queries poll `find` every 300 ms |
| React Native | Single native queue → serialised |

## 3. Coverage: verified versus unverified

**Measured and gated** means a CI job compares a candidate against a baseline and can fail. **Measured, not gated** means a harness exists and has numbers, but nothing fails on a regression.

| Area | Native x86_64 | Native arm64 | WASM / browser | Node | Android / iOS |
|---|---|---|---|---|---|
| Vector scoring kernels | measured, not gated | not measured (codegen checked manually once) | not measured | — | — |
| Exact vector search | gated (HNSW harness exact mean) | gated | gated | measured, manual | not measured |
| Filtered vector search | gated | gated | gated | measured, manual | not measured |
| HNSW build / search / recall | gated | gated | gated (browser, 5k) | measured, manual (50k) | not measured |
| HNSW incremental maintenance | **not measured** | not measured | not measured | not measured | not measured |
| Document queries (point, indexed, range, sort) | measured, not gated | not measured | measured, manual | measured, manual | not measured |
| Compound-index queries, aggregation | **not measured** in a bench | not measured | e2e phase 5 (manual, loose ceilings) | not measured | not measured |
| FTS / BM25 | measured, not gated | not measured | e2e only | not measured | not measured |
| Hybrid search | measured, not gated | not measured | e2e only | not measured | not measured |
| Bulk update/delete | measured, not gated | not measured | e2e only | not measured | not measured |
| Writes with durable commit (file backend) | **not measured in Rust** | not measured | e2e only | measured, manual | not measured |
| Index create/drop, FTS rebuild | measured, not gated | not measured | e2e only | not measured | not measured |
| Concurrent reads | vector only, report only | vector only, report only | n/a (serialised) | not measured | not measured |
| Readers under a writer, watch fan-out | **not measured** | not measured | e2e phase 3 (functional) | not measured | Kotlin/Swift regression tests (functional) |
| Open / reopen / first query | **not measured natively** | not measured | report only (browser `reopenMs`) | not measured | not measured |
| Crash recovery (kill mid-commit) | **not tested** | not tested | e2e write/close race (functional) | not tested | not tested |
| Peak memory / allocations | measured manually (stabilization reports) | not measured | origin memory, report only | not measured | not measured |
| Physical file growth | **not measured** | not measured | not measured | not measured | not measured |

In short, the vector side is the only part of the engine with a regression gate. The workloads that v0.12.1 stabilised — sorted pages, BM25, bulk mutations, index backfills — have Criterion numbers but no gate. A regression in them would ship unless someone ran the bench by hand.

## 4. Measurement environment and method

| Item | Value |
|---|---|
| Machine | Intel Core i5-9400F (6 cores, no SMT), 16 GB RAM, Fedora kernel 7.2.10, inside the VS Code Flatpak sandbox |
| CPU governor | `powersave`, not changeable without root — absolute numbers are conservative; ratios on the same machine are valid |
| Pinning | Single-threaded runs `taskset -c 5`; concurrency runs `taskset -c 2-5` |
| Toolchain | rustc 1.98.0 (pinned), Node 22.20.0, perf 7.2.10 |
| Source | Detached worktree of `a2ef466`, separate `CARGO_TARGET_DIR` |
| Binaries | `engine` bench `e53001c6…`, `hnsw_profile` `1fcaf5d3…`, `vector_filter_profile` `b1f9aa8a…`, `vector_memory_profile` `fe0e3dbd…`, `stabilization_memory` `9799a2e2…`, Node addon `e3817893…` (full hashes in `artifacts/v013-baseline/local/`) |
| CI evidence | v0.12.1 release run 39798655385: candidate side = `a2ef466`, baseline side = v0.12.0 (`f981aff`), Azure `ubuntu-latest` (x86_64) and `ubuntu-24.04-arm` |

Two out-of-tree probes were compiled against the same worktree source to answer questions the existing harnesses could not: a steady-state concurrency probe (per-thread CPU time, 400–20,000 requests per thread, warmed threads) and a two-sided range probe. Both are preserved in `artifacts/v013-baseline/harness/`; neither touched the repository.

The Node benchmark's "file-backed" database lives under `os.tmpdir()`, which is **tmpfs** on this host, so its write numbers exclude real fsync cost. Keepsake measured ~10 ms per durable single-document commit on a physical disk; here a single insert costs ~50 µs. Durable-commit latency on real storage is therefore still unmeasured.

## 5. v0.12.1 baseline measurements

### 5.1 Criterion, engine level (in-memory)

Default Criterion settings (linear sampling, 100 samples where not overridden), saved as Criterion baseline `v0121-full`; and the plan's gate protocol (flat sampling, 30 samples, 1 s warm-up, 3 s measurement), median of three repetitions `v0121-aa1..3`. Times are per operation.

| Workload | Default protocol, median [95% CI] | Gate protocol, median of 3 |
|---|---:|---:|
| `vector/score_one` Cosine / Dot / Euclidean, 384-d | 96.5 / 48.6 / 66.8 ns | — |
| `vector/score_one` Cosine / Dot / Euclidean, 1536-d | 395.9 / 198.1 / 252.0 ns | — |
| `vector/find_nearest/1000` (384-d cosine) | 0.183 ms [0.180–0.184] | — |
| `vector/find_nearest/10000` | 1.700 ms [1.605–1.725] | — |
| `query/range_scan_unindexed` (10k, 50% match) | 3.051 ms | — |
| `query/equality` (10k, **unindexed**) | 2.521 ms | — |
| `write/insert_many_1k` | 1.213 ms | — |
| `write/export_snapshot_10k` | 0.630 ms | — |
| `stabilization/indexed_page` 1k / 10k | 14.9 / 15.5 µs | 14.9 / 15.8 µs |
| `stabilization/sorted_page` 1k / 10k | 0.138 / 0.994 ms | 0.135 / 0.925 ms |
| `stabilization/unindexed_selective` 1k / 10k | 0.646 / 7.115 ms | 0.641 / 6.981 ms |
| `stabilization/bm25` 1k / 10k | 0.576 / 6.196 ms | 0.567 / 6.001 ms |
| `stabilization/bm25_selective` 1k / 10k | 0.277 / 2.667 ms | 0.277 / 2.657 ms |
| `stabilization/update_many` 1k / 10k | 4.980 / 54.27 ms | 5.017 / 51.57 ms |
| `stabilization/index_cycle` 1k / 10k | 5.228 / 53.26 ms | 5.145 / 51.41 ms |
| `stabilization/fts_index_cycle` 1k / 10k | 8.448 / 88.68 ms | 8.442 / 87.91 ms |
| `stabilization/delete_many` 1k / 10k | 17.32 / 181.1 ms | 16.78 / 174.6 ms |
| `stabilization/exact_vector` 1k / 10k (8-d) | 35.5 / 179 µs | 34.7 / 176 µs |
| `stabilization/hybrid` 1k / 10k | 0.305 / 3.016 ms | 0.307 / 2.819 ms |
| `stabilization/hnsw` 1k / 10k (8-d) | 26.7 / 30.0 µs | 26.7 / 29.5 µs |

These reproduce the stabilization review's "reviewed" column to within a few percent (for example `update_many/10000` 51.6 vs 52.0 ms, `delete_many/10000` 174.6 vs 175.4 ms, `bm25/10000` 6.00 vs 5.99 ms), so this machine and harness are consistent with the evidence v0.12.1 shipped on.

**The two protocols disagree by up to 6.9%** on the same binary (`sorted_page/10000`, `hybrid/10000`). Numbers from different protocols must never be compared; the gate uses one protocol on both sides.

### 5.2 Noise

**Local A/A** (identical binary, gate protocol, three repetitions): the maximum deviation of a repetition median from the median of three is 0.6–5.9% per case; 1k cases are 2–3× noisier than 10k cases (`update_many/1000` 5.9%, `bm25_selective/1000` 5.6%, `indexed_page/1000` 4.5%). Within a run, p95 of sample means sits only 1–6% above the median, so run-to-run drift — not sampling — dominates. The per-case table and derived tolerances are in plan §4.4.

**CI quasi-A/A.** The release run compared v0.12.0 with v0.12.1; the stabilization changes did not touch vector algorithms, and recall was identical at every `efSearch` on both architectures, so its vector ratios approximate run-to-run noise on GitHub runners at `repeats=1`:

| Measurement (single run, 10k) | Candidate / baseline ratio range observed |
|---|---|
| Warm per-request p50 (HNSW sweep, filtered warm) | 0.87 – 1.30 |
| Cold per-request p50 (filtered cold) | 0.95 – **1.67** |
| Graph build time | 0.86 – 1.06 |
| Exact mean (x64, resumable step) | 0.80 |

The same commit re-run on arm64 changed `exact moderate cold` from 1.515 ms to 0.916 ms.

**Gate history** (`performance.yml`, 22 most recent runs): 6 failures.

| Run | Event | Failure | Real regression? |
|---|---|---|---|
| 39798655385 attempt 1 | v0.12.1 release | arm64 `exact moderate cold` +67% (0.905 → 1.515 ms), one run | No — 0.916 ms on re-run of the same commit; blocked every publish job until re-run |
| 39612454345 | push to `main` | browser `all/ann/100` 0.875 → 2.010 ms and `dense-95pct/ann/100`, one run | No — the same change passed the three-run PR comparison |
| 37122119401 | PR | browser harness correctness error on baseline (`exact filter eligibility mismatch`) | Harness/baseline compatibility |
| 37031417321 | PR | baseline failed to compile the candidate's harness (`set_vector_cache_budget` missing) | Harness used newer API than the baseline |
| 36985008747, 36981144051 | PR | macOS ANN `efSearch` 400 latency; macOS exact +30% (median of 3 runs, 2k) | Undetermined; macOS was then replaced by `ubuntu-24.04-arm` (93be403) |

Both timing failures on the current Linux runners came from **single-run** comparisons, neither was a real regression, and one of them blocked a release. The two earlier macOS failures used three runs; their cause was never established.

### 5.3 Vector search — HNSW and exact (`hnsw_profile`, 384-d, clustered, spread 0.6, M=16, efConstruction=200)

| Configuration | Build | Exact | `ef` 50 | `ef` 100 | `ef` 200 | `ef` 400 |
|---|---:|---:|---:|---:|---:|---:|
| 2k, none, 8 MiB (local, median of 3) | 3.91 s | 0.32 ms | 0.28 ms / 80.5% | 0.40 ms / 95.4% | 0.57 ms / 99.6% | 0.84 ms / 100% |
| 10k, none, 8 MiB (local) | 74.2 s | 3.84 ms | 3.63 ms / 43.5% | 6.60 ms / 63.7% | 11.96 ms / 84.3% | 19.45 ms / 96.5% |
| 10k, none, **64 MiB** (local) | **28.8 s** | 1.64 ms | 0.53 ms / 43.5% | **0.89 ms** / 63.7% | 1.51 ms / 84.3% | 2.49 ms / 96.5% |
| 10k, binary, 8 MiB (local) | 10.0 s | 4.35 ms | 1.30 ms / 27.7% | 2.12 ms / 47.0% | 3.50 ms / 68.6% | 5.09 ms / 85.4% |
| 10k, none, 8 MiB (CI x64) | 68.6 s | 2.77 ms | 3.36 ms | 6.05 ms | 11.06 ms | 18.04 ms |
| 10k, none, 8 MiB (CI arm64, re-run) | 75.6 s | 2.85 ms | 3.80 ms | 6.29 ms | 11.22 ms | 18.80 ms |

Cells are p50 / recall@10. Recall is bit-for-bit deterministic: identical locally and on both CI architectures, for both v0.12.0 and v0.12.1.

Exact search over 10k × 384-d at different vector cache budgets (binary graph present, `exact_mean_ms`): **4.07 ms at 8 MiB, 1.59 ms at 16 MiB, 1.60 ms at 64 MiB**. The decoded block is 15.4 MB, so it is retained at 16 MiB and re-decoded on every query at 8 MiB — the WASM, Android and iOS default.

### 5.4 Filtered vector search (`vector_filter_profile`, 10k × 128-d, binary graph, 8 KiB bodies, median of 3, local)

| Filter | Exact cold / warm | ANN cold / warm | Distances (exact / ANN warm, 20 queries) |
|---|---:|---:|---:|
| sparse | 0.130 / 0.130 ms | 2.59 / **4.74** ms | 200 / **200,955** |
| moderate | 0.851 / 0.396 ms | 8.66 / 2.46 ms | 20,000 / 140,981 |
| dense | 2.396 / 1.810 ms | 3.36 / 1.89 ms | 190,000 / 30,537 |
| range | 2.498 / 1.515 ms | 4.18 / 1.85 ms | 150,000 / 41,053 |

With a sparse filter, warm ANN computes ~10,000 distances per query — the whole collection — where exact computes 10. `Auto` mode already picks exact for filtered queries; only explicit `mode: Ann` hits this. The browser shows the same shape (CI, 5k vectors, 1 MiB budget: `bucket-1pct` ANN 16.0 ms, exact 0.68 ms; 8 MiB: 6.2 ms vs 0.81 ms).

### 5.5 Concurrent search

**Existing harness** (`vector_memory_profile`, 2k × 128-d, binary graph, 20 requests per thread after a cache reset, median of 3 local runs; CI agrees within ~15%):

| Budget | 1 thread | 2 threads | 4 threads |
|---|---|---|---|
| 1 MiB | 0.48 ms, 1,739 req/s | 1.24 ms, 1,388 req/s | 1.32 ms, 2,542 req/s, ~19 fallbacks |
| 8 MiB | 0.49 ms, 1,738 req/s | 1.21 ms, 1,462 req/s | 1.42 ms, 2,443 req/s |

**Steady-state probe** (same fixture, threads warmed, ≥ 400 requests per thread, per-thread CPU time):

| Mode, budget | Throughput 1 → 2 → 4 threads | CPU per request 1 → 4 threads | Cores busy at 4 |
|---|---|---|---|
| Exact, 8 MiB | 7,868 → 15,365 → 30,822 req/s | 0.126 → 0.129 ms | 3.97 |
| ANN, 8 MiB | 7,653 → 14,521 → 15,826 req/s | 0.130 → 0.229 ms | 3.62 |
| ANN, 64 MiB | 7,507 → 11,792 → 14,948 req/s | 0.132 → 0.239 ms | 3.57 |
| ANN, 8 MiB, 10% filter | 2,155 → 3,656 → 5,244 req/s | 0.462 → 0.708 ms | 3.71 |

Two conclusions. The existing harness's 2-thread slowdown is mostly an artefact of two threads warming a freshly reset cache simultaneously with 20 requests each; at steady state, two threads nearly double ANN throughput. But ANN does **not** scale to four threads: CPU per request rises ~80% while cores stay busy, at both budgets — extra work per request, not lock waiting and not budget eviction. Exact search scales linearly. Profile attribution is in §6.

### 5.6 Two-sided indexed range (probe, in-memory, integer field, 100 matching documents)

| Collection | `$gte` newest 100 | `$lt` oldest 100 | `$gte` + `$lt` window of 100 |
|---|---:|---:|---:|
| 10k | 0.081 ms | 0.084 ms | **1.16 ms** |
| 100k | 0.087 ms | 0.093 ms | **12.53 ms** |

The Node benchmark measures the same thing through N-API (12.55 ms at 100k), so the cost is in the engine.

### 5.7 Memory (`stabilization_memory`, 10k × 8 KiB, in-memory backend, VmHWM)

| Workload | After load | After mutation | Peak incl. export | Snapshot | Elapsed (mutation) |
|---|---:|---:|---:|---:|---:|
| index (create + drop scalar, compound, FTS) | 422 MiB | 423 MiB | 501 MiB | 83.2 MB | 136 ms |
| update (`$inc` all) | 422 MiB | 842 MiB | 922 MiB | 84.8 MB | 414 ms |
| delete (all) | 422 MiB | 423 MiB | 423 MiB | 487 B | 276 ms |

Identical to the stabilization review's peaks (501 / 922 / 423 MiB). Loading 82 MB of documents already reaches 422 MiB: the in-memory backend keeps its storage vector and redb keeps cached pages of the same data. No TalaDB backend sets a redb cache size, so the file backend and the browser's OPFS backend also run with redb 4.2's 1 GiB default (`redb-4.2.0/src/db.rs:1435`). Browser and device memory remain unmeasured.

### 5.8 Node binding (`scripts/bench.mjs --skip-hnsw`, file on tmpfs)

| Workload | Result |
|---|---:|
| Single insert / `insertMany` 100 / 1,000 | 19.9k ops/s / 140.5k docs/s / 154.3k docs/s |
| `updateOne` / `deleteOne` by `_id` | 16.8k / 17.4k ops/s |
| 100k: `findOne` by `_id` / indexed equality (~10) | 11 µs / 69 µs |
| 100k: unindexed find / unindexed count | 58.8 ms / 53.0 ms |
| 100k: indexed `$gte` (~100) / `$gte` + `$lt` window (~100) | 0.60 ms / **12.55 ms** |
| Flat 384-d search: 1k / 10k / 50k / 100k | 0.83 / 2.22 / **22.6** / 45.7 ms |
| 100k flat search + 10% filter | 14.8 ms |

Flat search grows 10× from 10k to 50k vectors: at 50k × 384-d the decoded block (77 MB) exceeds the 64 MiB native budget and is re-decoded per query — the same cliff as §5.3, one budget up. At 1k, the Node call costs 0.83 ms against 0.18 ms in Rust. The likely cause is returned documents carrying their 384-float embeddings through N-API, but the binding was not profiled.

### 5.9 Browser (CI, release run, Chromium, worker + OPFS, 5k × 128-d, binary graph)

At 8 MiB: build 1.98 s, insert 0.51 s; unfiltered exact 0.84 ms, ANN `efSearch` 100 1.47 ms (recall 99%); reopen ~43 ms and first query 11–39 ms after reopen. At 1 MiB: build 3.30 s; unfiltered ANN `efSearch` 100 1.82 ms; filtered ANN up to 16 ms. Origin memory: unavailable on the CI runner. The local sandbox has a Playwright Chromium but no WASM packaging (`wasm-pack`), so browser numbers here are CI numbers only.

## 6. Profiles — where the time goes

Flat self-time profiles with `perf` at 1,999 Hz on the `bench` profile binary (debug symbols), each 20 s, reported over the 35–80% window to exclude the stabilization fixture's setup and the HNSW rebuild that follows each 10k group. Top symbols, grouped:

| Workload (10k) | Dominant costs |
|---|---|
| `vector/find_nearest` (384-d cosine) | `score_with_query_norm` **89.7%** — the stored-vector norm is recomputed per comparison |
| `exact_vector` (8-d) | scoring 44.8%, `Ranker::offer` 21.0%, scan closure 17.3% |
| `unindexed_selective` | full document decode ~70%: postcard `Value` 20.8%, `from_utf8` 16.5%, field vectors 12.0%, drops/allocator ~15% |
| `sorted_page` | redb range iteration + guard drops + scan 45.6%; allocator ~22% (per-entry `Vec` in `index_ordered_entries`) |
| `bm25` | redb range iteration/guards/lookups ~45%; SipHash on ULID keys 9.5%; scoring closure 9.5% |
| `hybrid` | redb range iteration + guard drops + scan 58.3%; SipHash 10.6% |
| `update_many` | glibc allocator ~24%, `memmove` 11.3%, redb ~15%, `from_utf8` 4.3%, document clone/drop ~5% |
| `delete_many`, `index_cycle`, `fts_index_cycle` | redb B-tree mutation (`LeafMutator` value/key end updates) 17–28%, page access, allocator |
| `insert_many_1k` | redb insert path and allocator; no single TalaDB hotspot |
| `hnsw` (8-d) | redb point reads ~22%, result document decode ~14% |
| ANN probe, 1 thread (2k × 128-d) | redb point reads ~26% (≈ `efSearch` rescoring reads per query), graph code decode 16%, `Value` decode 13%, traversal ~12%, scoring ~8% |
| ANN probe, 4 threads | `get_page` 8.4 → 12.0%, glibc allocator ~1.3 → ~6.8% |
| HNSW build (2k × 384-d) | distance work ~78%: `dot_similarity` 44.5%, neighbour selection `select` 24.8%, `distance` 8.2% |

Profiles are in `artifacts/v013-baseline/profiles/`.

## 7. Findings

Each finding is backed by §5–6. Ranking and proposed changes are in the roadmap.

1. **Only vector search is gated.** Document queries, FTS, hybrid, bulk mutations and index maintenance — the paths v0.12.1 stabilised — have benchmarks but no CI gate, and the Criterion bench never runs in CI.
2. **The gate is noisiest where it matters most.** Push, schedule and release use one run at the larger size; both timing failures from single runs were noise, and one blocked the v0.12.1 publish.
3. **CI measures HNSW only at the 8 MiB phone budget.** At the 64 MiB native default the same graph builds 2.6× faster and searches 7.4× faster; that configuration is not gated, and the internal roadmap's "graph does not pay for itself" applies to the phone budget only.
4. **Budget cliffs for exact search.** A vector block that does not fit the cache budget is re-decoded per query: 2.55× slower at 10k × 384-d on 8 MiB (WASM/mobile) and 10× slower going from 10k to 50k on native 64 MiB.
5. **Two-sided indexed ranges cost O(N).** 145× slower than a one-sided range at 100k, and unsorted results come back in per-process random order. The vector-filter path already merges such bounds for scalar fields; `find` does not.
6. **Exact cosine recomputes stored norms.** 89.7% of 384-d exact search is the kernel, half of which is the redundant norm pass; the graph path already caches norms.
7. **Sorted pages read and allocate the whole index.** 60× an indexed page of the same size at 10k, linear in N.
8. **Selective unindexed scans decode whole documents.** ~70% of `unindexed_selective` is decoding fields the filter never reads.
9. **ANN does not scale past two threads.** CPU per request +80% at four threads, from shared page-cache and allocator traffic in per-candidate rescoring reads; exact search scales linearly.
10. **Sparse filters make explicit ANN walk the whole graph** — 36× slower than exact over the allowed IDs (opt-in path only).
11. **Memory**: 422 MiB to hold 82 MB in memory; 922 MiB peak for a 10k update; redb's 1 GiB default cache on every backend including OPFS; no browser or device memory data.
12. **Recovery is untested.** No test kills a process mid-commit; open, reopen and repair times are unmeasured natively; physical file growth is unmeasured.
13. **No mobile performance evidence for the engine.** Kotlin and Swift have no benchmarks; Android/iOS numbers exist only from manual app probes, never for the engine in isolation.

## 8. Stale or misleading documentation found

| Location | Statement | v0.12.1 reality |
|---|---|---|
| `ROADMAP-INTERNAL.md` §2 "node cache has no ceiling across graphs" | open | `search_cache.rs` now enforces one shared, evicting budget across all graphs and vector blocks |
| `ROADMAP-INTERNAL.md` §2 "filtered vector search … materialising every matching document" | open | Filters resolve to bounded ID sets (`bounded_vector_ids_in`, `visit_vector_ids_in`) |
| `ROADMAP-INTERNAL.md` §2 "Where things stand" table | exact 1.72 ms, `ef` 100 1.80 ms / 65% at 10k | At 8 MiB: 3.84 / 6.60 ms; at 64 MiB: 1.64 / 0.89 ms; recall 63.7%. The table states no budget |
| `scripts/bench.mjs` comment on the two-sided range: "the planner emits a single bounded index scan for both bounds" | — | The planner emits `IndexAnd` of two half-open scans (`planner.rs:452`) |
| `benches/engine.rs` `query/equality` | reads as an equality lookup | It is an unindexed scan; no index is created |
| `rust-toolchain.toml` comment: "`rust-version = "1.88"`" | — | `Cargo.toml` declares 1.90 |

## 9. What this audit did not do

- No engine code was changed, committed or pushed; no tag was created.
- No Android, iOS, macOS or Windows measurement. No physical-disk fsync measurement (tmpfs). No local browser run; browser data is from CI only.
- No crash-consistency or power-loss test; no fuzz campaign beyond CI smoke.
- No arm64 run locally; arm64 data is CI only.
- Allocation counts were not re-measured (the stabilization review's Valgrind totals stand).
- Profiles are flat (self time). Call-graph attribution — needed before roadmap item 7 — was not collected.
