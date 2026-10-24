# v0.13.0 benchmark plan

Plan date: 2026-10-24. Baseline: **v0.12.1**, commit `a2ef46602127919c60a0fe6333d676b1b1ab391e`.
Companion documents: [V013_BASELINE_AUDIT.md](V013_BASELINE_AUDIT.md) (what exists, what was measured) and [V013_OPTIMIZATION_ROADMAP.md](V013_OPTIMIZATION_ROADMAP.md) (what to change, in which order).

This plan defines the workloads, measurement protocol and regression gates that every v0.13.0 engine change must pass. It extends the harnesses that already exist — the Criterion `engine` bench, the `examples/*_profile.rs` harnesses, `scripts/vector-bench-ci.py`, the browser vector suite and `performance.yml` — rather than adding a parallel framework. Nothing here changes a public API, the C ABI, or a storage format; every harness listed compiles against the v0.12.1 API, which is what lets it run on the baseline checkout unchanged.

## 1. Rules that apply to every measurement

1. **The baseline is a commit, not a number.** v0.12.1 is compared by building `a2ef466` in a detached worktree on the same machine, in the same job, with the candidate's harness — the rule `vector-bench-ci.py` already follows. Recorded numbers in the audit are reference points for humans; gates never compare against a stored number from another machine.
2. **Same harness on both sides.** The candidate's `benches/` and `examples/` files are copied into the baseline worktree before building, as `vector-bench-ci.py` does for `hnsw_profile.rs`. A harness may only use API present at v0.12.1. Because v0.13.0 adds no public API, this holds automatically — but a harness that calls a newer method must be rejected in review, since it breaks the baseline build (this is what failed run 37031417321 on 2026-10-02).
3. **Separate target directories** for baseline and candidate. A shared one can reuse a library across checkouts (already noted in `vector-bench-ci.py`).
4. **Interleave, never batch.** Run baseline and candidate alternately (B C B C B C), so drift in host load or thermal state lands on both.
5. **Deterministic fixtures.** Fixed ULIDs, fixed PRNG seeds, fixed query sets. The stabilization fixture already supplies deterministic IDs; new fixtures must do the same. A fixture that reseeds per run cannot be compared to its own baseline.
6. **Correctness before timing.** Every timed workload also emits a result fingerprint (IDs and score bits, or counts). A fingerprint mismatch is a hard failure that no timing result can override, and the timing for that case is not compared.
7. **Report what the number is.** Criterion reports sample means of batched iterations; percentiles of those are *not* request latency. Per-request percentiles come only from harnesses that time individual requests (`hnsw_profile`, `vector_filter_profile`, `vector_memory_profile`, the browser suite, `scripts/bench.mjs`). Reports must keep the two apart, as the stabilization reports did.
8. **Record the environment** with every result: commit, `rustc -vV`, `Cargo.lock` SHA-256, binary SHA-256, CPU model, governor, core pinning, load average, runner image.

## 2. Workload catalogue

Status column: **exists** = runs today with no change; **extend** = small addition to an existing harness (a bench function, a flag, a fixture field); **new** = new example or bench file inside the existing harness layout. Tier is the gate tier from §4.

Sizes: S = 1,000 documents, M = 10,000, L = 100,000. L runs only in Tier 2. Unless stated, document fixtures use the stabilization fixture (`benches/support/stabilization.rs`): integer `n` and `bucket`, a short text `body`, a 1 KiB `payload`, an 8-d vector `v`, deterministic IDs.

### 2.1 Document queries (DOC)

| ID | Workload | Sizes | Harness | Status | Metric | Oracle | Tier |
|---|---|---|---|---|---|---|---|
| DOC-1 | `find_one` by `_id` | S M L | `stabilization` | extend | time/op | returned ID | 1 |
| DOC-2 | Indexed equality, ~1% selectivity (`bucket = 42`, indexed) | S M L | `stabilization` | extend | time/op | ID set hash | 1 |
| DOC-3 | Indexed range, bounded page (`n >= 0`, limit 10) | S M L | `stabilization/indexed_page` | exists | time/op | ID list | 1 |
| DOC-4 | Unindexed selective scan (`counter = -1`, no match) | S M L | `stabilization/unindexed_selective` | exists | time/op | count | 1 |
| DOC-5 | Unindexed range scan returning 50% | M | `query/range_scan_unindexed` | exists | time/op | count | 1 |
| DOC-6 | Sorted page via index (`sort n desc, skip 20, limit 10`) | S M L | `stabilization/sorted_page` | exists | time/op | ID list | 1 |
| DOC-7 | Compound-index equality + range (`bucket = 42 AND n >= x`) | M L | `stabilization` | extend | time/op | ID list | 1 |
| DOC-8 | `count` with indexed and unindexed filters | M L | `stabilization` | extend | time/op | count | 1 |
| DOC-9 | Aggregation `$match` → `$group` (`bucket`, sum/avg) | M L | `stabilization` | extend | time/op | result hash | 1 |
| DOC-10 | Projection: `exclude: ["payload", "v"]` vs full documents | M | `stabilization` | extend | time/op, bytes returned | ID list | 1 |
| DOC-11 | Two-sided indexed range window of 100 docs (`n >= x AND n < x + 100`), varying `x` | S M L | `stabilization` | extend | time/op | ID set | 1 |
| DOC-12 | Ascending sorted page (`sort n asc, skip 20, limit 10`) | S M L | `stabilization` | extend | time/op | ID list | 1 |
| DOC-13 | Selective unindexed `count` (`counter = -1`) | M L | `stabilization` | extend | time/op | count | 1 |

`query/equality` in `engine.rs` is an **unindexed** scan — `bench_query` creates no index. Keep it as DOC-5's sibling, but do not read it as indexed-equality evidence; DOC-2 fills that gap.

### 2.2 Exact vector search (VEC)

| ID | Workload | Sizes | Harness | Status | Metric | Oracle | Tier |
|---|---|---|---|---|---|---|---|
| VEC-1 | Score kernel per metric × {128, 384, 768, 1536} dims | — | `vector/score_one` | exists | ns/score | — | 1 |
| VEC-2 | Exact top-10, 384-d cosine, unfiltered | S M | `vector/find_nearest` | exists | time/op | IDs + score bits | 1 |
| VEC-3 | Exact top-10, 384-d, dot and euclidean | M | `vector/find_nearest` | extend (metric param) | time/op | IDs + score bits | 1 |
| VEC-4 | Exact filtered: sparse / moderate / dense / range, cold and warm | M | `vector_filter_profile` via `vector-bench-ci.py --workload filtered` | exists, CI | p50/p95 per request | fingerprint, distance count | 1 |
| VEC-5 | Exact search at phone cache budgets (1 MiB, 8 MiB) | 2k | `vector_memory_profile` | exists, CI | p50/p95, fallbacks | exact-ranking assert | 1 |
| VEC-6 | Vectorisation of score kernels per target | — | `rustc --emit=asm` instruction count | new (script) | vector-instruction count | — | 0 |

VEC-6 makes the procedure already documented in `ROADMAP-INTERNAL.md` ("Gotchas") a gate. It is deterministic: no timing, no noise. Count `v128.`/`f32x4.` instructions in `wasm32-unknown-unknown` (`+simd128`), `.4s` operations for `aarch64-unknown-linux-gnu` and `aarch64-apple-ios`, and `%xmm` operations for `x86_64`, in `dot_similarity`, `norm_sq`, `dot_and_norm_sq` and `euclidean_similarity`. Build through `-p taladb-web` for wasm (`-p taladb` fails in `getrandom` on wasm32), and do not set `RUSTFLAGS` on the command line (it drops `getrandom_backend`). The 2026-09-20 regression — 114 vector instructions on x86, 2 on aarch64, 0 on wasm — is the failure this catches and no x86 timing gate can.

### 2.3 Approximate search (HNSW)

| ID | Workload | Sizes | Harness | Status | Metric | Oracle | Tier |
|---|---|---|---|---|---|---|---|
| HNSW-1 | Graph build, 384-d, none and binary quantization | 2k (PR), 10k (T2) | `hnsw_profile` via `vector-bench-ci.py` | exists, CI | build ms | recall | 1/2 |
| HNSW-2 | Resumable build, batch 128, step p95 | 2k, 10k | same, `--batch-size 128` | exists, CI | build ms, step p95 | recall | 1/2 |
| HNSW-3 | Search sweep, `efSearch` 50/100/200/400 | 2k, 10k | `hnsw_profile` | exists, CI | p50/p95, recall@10 | recall equality | 1 |
| HNSW-4 | Incremental maintenance: insert / update / delete 1,000 docs into a collection with a ready graph | 10k | `hnsw_profile` | extend (`--maintain N`) | ms per write, graph still `ready` | recall after | 2 |
| HNSW-5 | ANN vs exact crossover: 10k, 20k, 50k at 384-d | 10k–50k | `hnsw_profile` | exists (sizes only) | p50 ANN vs exact at recall ≥ 0.95 | recall | 3 |
| HNSW-6 | Filtered ANN (10% tenant, 1% bucket) | 2k, 5k | `vector_filter_profile`, browser suite | exists, CI | p50 vs exact | fingerprint | 1 |
| HNSW-7 | Cold first query after reopen (graph load) | 10k | `hnsw_profile` | extend (`--reopen`, file-backed) | first-query ms | recall | 2 |

**Every HNSW workload runs at two cache budgets: 8 MiB (the WASM/Android/iOS default) and 64 MiB (the native default).** `hnsw_profile` defaults to 8 MiB, so today's CI gate only measures the phone budget. The audit found the budget decides the result at 10k × 384-d: build 74.2 s vs 28.8 s, ANN p50 at `efSearch` 100 6.60 ms vs 0.89 ms, for the identical graph. A change that helps one budget can hurt the other; both must be gated. To explain cache behaviour without adding public API, HNSW-1/3 also count storage point reads by wrapping `RedbBackend` in a counting `StorageBackend` inside the example (the pattern `tests/concurrent_consistency.rs` already uses), giving a node-cache miss proxy per query.

Recall is deterministic for a fixed seed and fixed IDs: v0.12.0 and v0.12.1 produced identical recall at every `efSearch` on both x64 and arm64 in the release run. A recall change is therefore a behaviour change, not noise, and is held to a hard rule (§4.2).

### 2.4 Full-text search (FTS)

| ID | Workload | Sizes | Harness | Status | Metric | Oracle | Tier |
|---|---|---|---|---|---|---|---|
| FTS-1 | BM25 top-10, two terms | S M L | `stabilization/bm25` | exists | time/op | IDs + score bits | 1 |
| FTS-2 | BM25 with selective filter | S M | `stabilization/bm25_selective` | exists | time/op | IDs + score bits | 1 |
| FTS-3 | BM25 on a Zipf-distributed vocabulary (realistic posting lengths) | M L | `stabilization` | extend (fixture) | time/op | IDs + score bits | 1 |
| FTS-4 | FTS index drop + rebuild | S M L | `stabilization/fts_index_cycle` | exists | time/op | stats equality | 1 |
| FTS-5 | Incremental FTS cost: `insert_many` 1,000 docs into an FTS-indexed collection | M | `stabilization` | extend | time/op | stats equality | 1 |

The current FTS fixture gives every document the same three words plus one of 100 tokens (`"rust database storage tokenN"`), so the BM25 query matches the whole corpus with near-uniform postings. FTS-3 adds a deterministic Zipf vocabulary (fixed seed, ~5,000 terms, s ≈ 1.1, 8–40 tokens per document) so posting-list skew resembles real text. No external dataset is needed.

### 2.5 Hybrid search (HYB)

| ID | Workload | Sizes | Harness | Status | Metric | Oracle | Tier |
|---|---|---|---|---|---|---|---|
| HYB-1 | BM25 + exact vector, filtered (`bucket = 42`), RRF | S M | `stabilization/hybrid` | exists | time/op | IDs + score bits | 1 |
| HYB-2 | Same, unfiltered | M | `stabilization` | extend | time/op | IDs + score bits | 1 |
| HYB-3 | Same at 384-d on the FTS-3 corpus | M | `stabilization` | extend | time/op | IDs + score bits | 2 |

### 2.6 Bulk mutations and writes (BULK)

| ID | Workload | Sizes | Harness | Status | Metric | Oracle | Tier |
|---|---|---|---|---|---|---|---|
| BULK-1 | `insert_many` 1,000 small docs, no indexes, in-memory | — | `write/insert_many_1k` | exists | time/op | count | 1 |
| BULK-2 | `insert_many` 1,000 docs into a fully indexed collection (scalar, compound, FTS, vector) | M | `stabilization` | extend | time/op | count + index check | 1 |
| BULK-3 | `update_many` `$inc` over all documents | S M L | `stabilization/update_many` | exists | time/op | count | 1 |
| BULK-4 | `delete_many` all documents (snapshot restore outside timing) | S M | `stabilization/delete_many` | exists | time/op | count | 1 |
| BULK-5 | Single-document `update_one` / `delete_one` by `_id` | M | `stabilization` | extend | time/op | count | 1 |
| BULK-6 | File-backed variants of BULK-1/3/5, `Immediate` and eventual durability | M | `stabilization` (tempdir) | extend | time/op | count after reopen | 2 |
| BULK-7 | Node binding writes: single insert, `insertMany` 100/1,000, `updateOne`, `deleteOne`, file-backed | — | `scripts/bench.mjs` | exists, manual | ops/s, median | — | 2 |

All Criterion workloads today run on the in-memory backend. Commit durability, page I/O and file growth are therefore unmeasured in Rust; only the Node script is file-backed. BULK-6 closes that with the same harness on a temporary file. Durability defaults are not to be changed by v0.13.0 work; BULK-6 measures them so that a change elsewhere cannot hide a commit-path regression.

### 2.7 Index maintenance (IDX)

| ID | Workload | Sizes | Harness | Status | Metric | Oracle | Tier |
|---|---|---|---|---|---|---|---|
| IDX-1 | Create + drop scalar and compound index | S M L | `stabilization/index_cycle` | exists | time/op | — | 1 |
| IDX-2 | Write amplification: `insert_many` 1,000 docs with 0, 1, 3 scalar indexes, + FTS, + vector | M | `stabilization` | extend | time/op per configuration | count | 2 |
| IDX-3 | Vector index create (exact) and graph backfill | M | `hnsw_profile` | exists | build ms | recall | 1 |
| IDX-4 | Physical file size after create/drop and update cycles, before/after `compact()` | M | `stabilization_memory` | extend (file-backed mode) | bytes | — | 2 |

### 2.8 Concurrent access (CONC)

Engine reads are concurrent in real deployments: the Kotlin wrapper runs calls under a read lock, the Swift wrapper uses a concurrent queue, and the Node binding runs async tasks on the libuv pool. The browser worker and React Native serialise calls, so CONC results apply to native and Node only.

| ID | Workload | Threads | Harness | Status | Metric | Oracle | Tier |
|---|---|---|---|---|---|---|---|
| CONC-1 | Concurrent ANN search, 2k × 128-d, binary graph, 1 MiB / 8 MiB budgets | 1, 2, 4 | `vector_memory_profile` | exists, CI | p50/p95, throughput, fallbacks | exact-ranking assert | 1 |
| CONC-2 | Same, steady state: ≥ 400 requests per thread, per-thread CPU time | 1, 2, 4 | `vector_memory_profile` | extend (`--steady N`) | throughput, CPU ms/request, CPU cores busy | as CONC-1 | 2 |
| CONC-3 | Concurrent document reads (DOC-2, DOC-3, FTS-1 mix) | 1, 2, 4 | new example `concurrent_reads.rs` | new | throughput, p50/p95 | fingerprint | 2 |
| CONC-4 | Readers under one writer: DOC-2 p95 while another thread runs `insert_many` batches of 128 | 1 writer + 1/2/4 readers | `concurrent_reads.rs` | new | reader p95, writer throughput | fingerprint | 2 |
| CONC-5 | Live-query fan-out: 1, 8, 32 `watch` handles on one collection, 1,000 single inserts | — | `concurrent_reads.rs` | new | write p50/p95 with N watchers, notification delay p95 | events received | 2 |

`throughput = requests / wall time` and `CPU cores busy = Σ thread CPU time / wall time`. Together they distinguish serialisation (busy ≈ 1 core, throughput flat) from extra per-request work (busy ≈ N cores, CPU per request rising). The audit's concurrency probe uses exactly this measurement.

### 2.9 Recovery and persistence (REC)

| ID | Workload | Sizes | Harness | Status | Metric | Oracle | Tier |
|---|---|---|---|---|---|---|---|
| REC-1 | Open a file-backed database (clean shutdown) and run the first query | M L | new example `reopen_profile.rs` | new | open ms, first-query ms | fingerprint | 2 |
| REC-2 | Snapshot export and restore | M | `write/export_snapshot_10k`; restore extend | exists / extend | time/op, bytes | byte equality of re-export | 1 |
| REC-3 | Crash consistency: child process inserts/updates in a loop with `Immediate` durability, parent sends SIGKILL at a seeded random time; reopen and verify | M | new example `crash_reopen.rs` + driver | new | open/repair ms after kill | every acknowledged commit present; index scan == full scan; FTS stats == rebuild; vector count == document count | 2 |
| REC-4 | Open of a redb v2 file (legacy migration) | M | `backend_migration.rs` fixture, timed | extend | ms | document count | 3 |
| REC-5 | Browser reopen and first query after OPFS reload | 2k, 5k | browser vector suite (`reopenMs`, `firstQueryMs`) | exists, CI (report only) | ms | fingerprint | 2 |

REC-3 is primarily a correctness gate: no engine test today kills a process mid-commit. It measures recovery time as a side effect. It must never run against a user's database or a device build.

### 2.10 Memory (MEM)

| ID | Workload | Harness | Status | Metric | Tier |
|---|---|---|---|---|---|
| MEM-1 | Peak RSS by phase (load, index, mutate, export, drop), 10k × 8 KiB docs: index / update / delete | `stabilization_memory` + `/usr/bin/time -v` | exists | VmHWM per phase | 2 |
| MEM-2 | Allocation count and bytes for MEM-1 at 1k | `stabilization_memory` under a counting global allocator (feature-gated in the example) | extend | allocations, bytes | 1 |
| MEM-3 | Vector cache accounting: retained ≤ budget, peak ≤ allowance, active = 0 after | `vector_memory_profile`, browser suite | exists, CI | bytes | 0 |
| MEM-4 | 1k workloads under a 192 MiB address-space limit | `stabilization_memory` under `ulimit -v` | exists, manual | pass/fail | 2 |

Allocation counts (MEM-2) are deterministic for a fixed fixture, so they gate tightly where RSS cannot. Valgrind was used for this in the stabilization review; a counting allocator inside the example gives the same numbers in seconds and runs in CI.

### 2.11 Platform coverage

| Platform | Engine path exercised | Runs where | Gate |
|---|---|---|---|
| Linux x86_64, native | Criterion, all examples | CI `ubuntu-latest`, local i5-9400F | Tier 0–2 |
| Linux aarch64, native | vector examples (and Criterion once added) | CI `ubuntu-24.04-arm` | Tier 0–2 |
| macOS arm64, native | none today (93be403 replaced `macos-latest` with `ubuntu-24.04-arm` for arm64 coverage; both macOS gate runs on record, 36981144051 and 36985008747, had failed on latency) | — | Tier 3 |
| Chromium, WASM worker + OPFS | browser vector suite | CI `ubuntu-latest` | Tier 1 (vector) |
| Node | `scripts/bench.mjs` | manual | Tier 2 (manual) |
| Android device | Notewell `Probe`, Kepta (manual) | user's device only | Tier 3, report only |
| iOS / Safari | browser suite over HTTPS (manual) | — | Tier 3, report only |
| Kotlin / Swift wrappers | none (functional tests only) | — | Tier 3 |

Native Android and iOS performance cannot be gated in CI with current infrastructure. Release notes must not claim mobile performance changes that were measured only on x86_64 or arm64 Linux.

## 3. Measurement protocol

### 3.1 Criterion workloads (Tier 1)

Run the existing `engine` bench with the controls the stabilization review introduced:

```sh
TALADB_BENCH_FLAT=1 TALADB_BENCH_SAMPLES=30 \
  cargo bench -p taladb --bench engine --locked -- '<filter>' \
  --warm-up-time 1 --measurement-time 3 --sample-size 30 --noplot \
  --save-baseline <side>-<rep>
```

- Three repetitions per side, interleaved B C B C B C, each saved under its own Criterion baseline name; the per-repetition statistic is the median of the 30 flat sample means (`target/criterion/<id>/<baseline>/sample.json`).
- Case statistic: **median of the three per-repetition medians**. Paired ratios `C_i / B_i` are kept for the consistency rule in §4.
- Local runs: pin to one core (`taskset -c N`) and record the governor. CI runners cannot pin reliably; interleaving carries the load there.
- The baseline worktree receives the candidate's `benches/engine.rs` and `benches/support/` before building.

### 3.2 Per-request workloads (examples)

`hnsw_profile`, `vector_filter_profile`, `vector_memory_profile` and the browser suite already emit JSON with p50/p95, recall, fingerprints and cache statistics. Keep their schemas. Two protocol changes:

- **Repeats ≥ 3 on every event.** `performance.yml` uses `repeats=3` for pull requests but `repeats=1` (with the larger 10k / 5k sizes) for push, schedule and release — the configuration that blocks a release is the noisiest one. Both single-run failures on record came from that: v0.12.1 attempt 1 (arm64, `exact moderate cold` 0.905 → 1.515 ms, 0.916 ms on rerun of the same commit) and the 2026-10-21 push to `main` (browser `all/ann/100` 0.875 → 2.010 ms, which passed the three-run PR comparison of the same change).
- **Cold-cache cases are reported, not gated, unless measured over ≥ 5 runs.** A cold p50 is effectively one sample per query; the arm64 release failure was a cold case.

### 3.3 Memory workloads

`/usr/bin/time -v` around `stabilization_memory` for VmHWM; the example's own `/proc/self/status` checkpoints for per-phase RSS. Three runs per side, median. RSS depends on the allocator and the host, so only same-machine comparisons count.

### 3.4 Profiling (diagnosis, not gating)

Use the `bench` profile, which keeps debug symbols:

```sh
perf record -F 1999 -o out.data -- \
  target/release/deps/engine-<hash> --bench '^<id>$' --profile-time 20
perf report -i out.data --time 50%-100% --no-children --sort symbol --stdio
```

The stabilization group seeds both fixtures before the filter applies, so report only the second half of the window. The audit's profiles were taken this way.

## 4. Gates

### 4.1 Tiers

| Tier | When | What | Blocking |
|---|---|---|---|
| 0 | Every PR touching `packages/core/**` or bindings | Fingerprints, recall equality, VEC-6 codegen counts, MEM-2 allocation counts, MEM-3 accounting asserts | Yes |
| 1 | Same PRs | Same-runner A/B against the PR base: Criterion subset (§3.1) at S and M; existing vector, filtered, browser comparisons at PR sizes | Yes, after confirmation (§4.3) |
| 2 | Weekly schedule and every release tag | Tier 1 at M and L; HNSW at 10k; MEM-1; CONC-2..5; REC-1/3; BULK-6/7. **Plus a cumulative comparison of the candidate against v0.12.1** | Release: yes, after confirmation; schedule: opens an issue |
| 3 | Manual, before a release that claims a performance change | Android/iOS device runs, Safari, macOS, HNSW-5 crossover, REC-4 | No; results go into the release notes |

### 4.2 Decision rules

For each timed case with baseline statistic *B*, candidate statistic *C* and paired ratios *rᵢ = Cᵢ / Bᵢ*:

**Regression** when all three hold:

1. *C / B > 1 + T*, where *T* is the case's relative tolerance from the calibration table in §4.4.
2. *C − B > A*, the case's absolute floor. ~~*A* = max(2 µs, 2% of *B*)~~ — **superseded in Stage 0 (§7.4):** a fixed 2 µs floor is larger than every `vector/score_one` measurement (16–500 ns), and a positive control showed a 1.9× slower kernel passing. *A* is now calibrated per case: max(3 × the largest A/A |median(C) − median(B)|, 2% of *B*). The per-request vector gates likewise moved from fixed floors (0.25 ms native) to floors calibrated per step configuration; the browser keeps 0.5 ms.
3. At least two of the three paired ratios exceed *1 + T/2*. One bad repetition cannot fail a case.

**Improvement claims** use the mirror rule. A roadmap item's acceptance criterion ("≥ 20% faster") is met only when *B / C − 1* exceeds the target *and* the mirror rule holds; otherwise the change is reported as neutral.

**Hard rules, no tolerance:**

- Result fingerprints equal (IDs, order, score bits) for every deterministic workload, or the PR states which intended behaviour change alters them and the reviewer accepts it.
- Recall@10 at every `efSearch` must not fall. The existing 3-percentage-point allowance in `vector-bench-ci.py` and `vector-web-bench-compare.mjs` stays as the CI rule because graph-build changes may legitimately move individual neighbours; for v0.13.0, any recall change at all must be listed in the PR description.
- MEM-3 accounting asserts hold. MEM-2 allocation count within +2% of baseline.
- VEC-6 vector-instruction count must not fall on any target for any kernel.

### 4.3 Confirmation

A Tier 1 or Tier 2 failure re-measures only the failing cases with three more interleaved pairs, and fails the job only if the case fails again on the combined six pairs. This replaces "re-run failed jobs", which today re-runs everything and loses the first attempt's evidence from the run view. Keep both attempts' JSON in the uploaded artifact.

### 4.4 Tolerance calibration

Tolerances are derived from measured noise, not chosen. *T = max(T_min, 3 × N)*, where *N* is the case's A/A noise: the maximum absolute deviation of a per-repetition median from the median of three, measured on identical binaries with the §3.1 protocol.

Local calibration, v0.12.1, Intel i5-9400F, `powersave` governor (not changeable here), pinned to one core, three repetitions of the §3.1 protocol on one binary (SHA-256 `e53001c6…`), 2026-10-24. These values are the *procedure demonstrated*, not the CI tolerances: GitHub runners are noisier, and the CI table must be produced the same way on `ubuntu-latest` and `ubuntu-24.04-arm` before Tier 1 is switched on (§6). Until then, use *T* = max(10%, 3*N*_local) on CI.

| Case (`stabilization/…`) | v0.12.1 median | A/A noise *N* | A/A range | *T* = max(5%, 3*N*) | *A* = max(2 µs, 2%) |
|---|---:|---:|---:|---:|---:|
| bm25/1000 | 0.567 ms | 4.4% | 4.4% | 14% | 11.34 µs |
| bm25/10000 | 6.001 ms | 1.5% | 2.8% | 5% | 0.120 ms |
| bm25_selective/1000 | 0.277 ms | 5.6% | 7.5% | 17% | 5.54 µs |
| bm25_selective/10000 | 2.657 ms | 2.9% | 5.7% | 9% | 53.14 µs |
| delete_many/1000 | 16.776 ms | 4.0% | 4.7% | 13% | 0.336 ms |
| delete_many/10000 | 174.619 ms | 1.2% | 1.3% | 5% | 3.492 ms |
| exact_vector/1000 | 34.71 µs | 3.4% | 4.3% | 11% | 2.00 µs |
| exact_vector/10000 | 0.176 ms | 1.9% | 2.1% | 6% | 3.51 µs |
| fts_index_cycle/1000 | 8.442 ms | 2.6% | 3.2% | 8% | 0.169 ms |
| fts_index_cycle/10000 | 87.905 ms | 0.6% | 0.7% | 5% | 1.758 ms |
| hnsw/1000 | 26.66 µs | 3.5% | 6.6% | 11% | 2.00 µs |
| hnsw/10000 | 29.47 µs | 3.8% | 5.5% | 12% | 2.00 µs |
| hybrid/1000 | 0.307 ms | 2.8% | 2.8% | 9% | 6.15 µs |
| hybrid/10000 | 2.819 ms | 0.6% | 1.1% | 5% | 56.38 µs |
| index_cycle/1000 | 5.145 ms | 2.5% | 3.0% | 8% | 0.103 ms |
| index_cycle/10000 | 51.412 ms | 2.1% | 2.7% | 7% | 1.028 ms |
| indexed_page/1000 | 14.90 µs | 4.5% | 6.5% | 14% | 2.00 µs |
| indexed_page/10000 | 15.79 µs | 2.1% | 3.8% | 7% | 2.00 µs |
| sorted_page/1000 | 0.135 ms | 2.1% | 2.6% | 7% | 2.69 µs |
| sorted_page/10000 | 0.925 ms | 1.6% | 3.0% | 5% | 18.50 µs |
| unindexed_selective/1000 | 0.641 ms | 1.4% | 1.5% | 5% | 12.82 µs |
| unindexed_selective/10000 | 6.981 ms | 4.3% | 4.7% | 13% | 0.140 ms |
| update_many/1000 | 5.017 ms | 5.9% | 6.6% | 18% | 0.100 ms |
| update_many/10000 | 51.566 ms | 2.2% | 3.7% | 7% | 1.031 ms |

Observations: S-size (1k) cases are 2–3× noisier than M-size (10k) cases, because their sub-millisecond iterations are more exposed to timer, cache and scheduler effects; M-size cases carry the gate, and S-size cases with *T* above 10% are reported, not gated. Within-run spread is small (p95 of sample means 1–6% above the median), so between-run drift — not sampling — dominates *N*. This is why the protocol repeats whole runs instead of adding samples.

Recalibrate when the runner image, toolchain pin or fixture changes; store the A/A JSON next to the gate's output.

**CI calibration was done in Stage 0; the results are in §7.** The local table above was produced before the Stage 0 harness existed; §7.2 also has a local A/A with the Stage 0 harness.

### 4.5 Cumulative drift against v0.12.1

Per-PR tolerances allow drift: ten changes each 4% slower pass every gate and compound to +48%. Tier 2 therefore also compares the candidate directly against v0.12.1 on the same runner with the same rules, and fails a release if any Tier 1 case is slower than v0.12.1 by more than *T* — unless the changelog names the case and the reason. This comparison is what makes v0.12.1 the comparison baseline for all of v0.13.0 rather than only for the first PR.

## 5. CI integration (reuse, in order)

1. `performance.yml` `compare` job: change `repeats` to 3 for push/schedule/release (keep the larger sizes); stop gating cold cases at one run; add a `--cache-bytes` pass-through to `vector-bench-ci.py` (it forwards none today, so CI always measures `hnsw_profile`'s 8 MiB default) and run a second HNSW step at 67108864 so the native default budget is gated alongside the 8 MiB one (at 64 MiB the 10k build takes ~29 s instead of ~74 s, so the extra step costs less than the existing one). No new job.
2. Add a `--workload criterion` mode to `scripts/vector-bench-ci.py`: copy the candidate's `benches/` into the baseline worktree, run §3.1 interleaved, read Criterion's `sample.json`, apply §4.2, write the same JSON and step-summary table. The script already owns worktree setup, separate target directories, artifact naming and the step summary.
3. Add a Tier 0 step to `ci.yml`'s `rust-check` job: run the deterministic fingerprints, the VEC-6 codegen script and the MEM-2 counter. All three finish in seconds and need no baseline build.
4. Add the v0.12.1 cumulative comparison as a second `BENCH_BASE` in the scheduled and release runs of `performance.yml` (`BENCH_BASE=v0.12.1`).
5. Keep uploading every JSON with 90-day retention, and attach the release run's JSON to the GitHub release so the baseline evidence outlives the retention window.

## 6. Acceptance criteria for this plan

The benchmark infrastructure is ready for v0.13.0 optimisation work when:

Status after Stage 0 (2026-10-24, branch `feat/v013-stage0-benchmarks`; details in §7 and `V013_STAGE0_IMPLEMENTATION_REPORT.md`):

- [x] Every workload marked **exists** runs against both v0.12.1 and the candidate with the candidate's harness, producing fingerprints that match between the two. *(77 Criterion workloads; cumulative jobs build a2ef466 itself.)*
- [x] Every workload implemented as **extend** or **new** compiles against the v0.12.1 API and has a fingerprint oracle. *(Not implemented: HNSW-4/5/7, HYB-3, VEC-3, BULK-6/7 in CI, IDX-4, CONC-3/4/5, REC-1/3/4, MEM-4 — see §7.6.)*
- [x] A/A calibration has been run on `ubuntu-latest` and `ubuntu-24.04-arm` (same commit built twice), and no case fails the rule on the A/A data or on fresh A/A validation runs.
- [x] `performance.yml` uses ≥ 3 interleaved repeats on every event, and confirmation is implemented (Criterion, examples, browser).
- [ ] Tier 0 runs in `ci.yml` in under two minutes. *(Runs in `ci.yml`, in parallel with `rust-check`: 2.8 min x86_64 with a warm cache — four-target codegen is most of it — and 1.2 min arm64. Target missed.)*
- [x] The cumulative v0.12.1 comparison runs on schedule and on release *(and on request; by SHA)*.
- [x] Deliberately slowed candidates fail the corresponding gate and an unmodified candidate passes. *(Five controls; two of them first exposed gates that could not fire, which were fixed — §7.4.)*

## 7. Stage 0 results (2026-10-24)

Branch `feat/v013-stage0-benchmarks`; every run below is on GitHub-hosted `ubuntu-latest` (x86_64, AMD EPYC 7763 in these runs) and `ubuntu-24.04-arm`, and every comparison builds both sides on the same runner in the same job. Run IDs and raw data: `artifacts/v013-stage0/`.

### 7.1 Criterion A/A calibration

Three A/A jobs per runner type (runs 40089969692, 40089974304, 40089979284; the same commit `d931950` built twice and interleaved, 1k/10k, 100k for `doc/`), §3.1 protocol. Rule: *T* = max(5%, 3*N*), *A* = max(3 × largest A/A |median(C) − median(B)|, 2% of *B*); report-only above *T* = 25%, or for 1k cases above 10%.

| Runner | Cases | Gated | Median *N* | Median gated *T* | Gated at *T* = 5% | Report-only 1k cases |
|---|---:|---:|---:|---:|---:|---:|
| `ubuntu-latest` (x86_64) | 88 | **56** | 4.4% | 11.3% | 1 | 19 of 25 |
| `ubuntu-24.04-arm` | 88 | **81** | 2.0% | 5.5% | 37 | 2 of 25 |

Selected cases (*N* / *T* / state):

| Case | x86_64 | | | arm64 | | |
|---|---:|---:|---|---:|---:|---|
| stabilization/sorted_page/10000 | 2.9% | 9% | gated | 3.0% | 9% | gated |
| stabilization/indexed_page/10000 | 4.0% | 12% | gated | 1.3% | 5% | gated |
| stabilization/unindexed_selective/10000 | 9.0% | 27% | report | 2.9% | 9% | gated |
| stabilization/bm25/10000 | 4.5% | 13% | gated | 3.1% | 9% | gated |
| stabilization/update_many/10000 | 6.8% | 20% | gated | 4.1% | 12% | gated |
| stabilization/delete_many/10000 | 3.1% | 9% | gated | 1.6% | 5% | gated |
| stabilization/hybrid/10000 | 2.9% | 9% | gated | 2.9% | 9% | gated |
| doc/range_window/10000 (DOC-11) | 2.7% | 8% | gated | 2.6% | 8% | gated |
| doc/range_window/100000 (DOC-11 L) | 14.8% | 44% | report | 2.9% | 9% | gated |
| doc/sorted_page_asc/10000 (DOC-12) | 1.7% | 5% | gated | 3.2% | 10% | gated |
| doc/count_unindexed/10000 (DOC-13) | 4.4% | 13% | gated | 7.0% | 21% | gated |
| fts/bm25_zipf/10000 (FTS-3) | 5.5% | 16% | gated | 0.9% | 5% | gated |
| vector/find_nearest/10000 (VEC-2) | 4.5% | 13% | gated | 9.5% | 28% | report |
| vector/score_one/Cosine/384 (VEC-1) | 6.3% | 19% | gated | 0.3% | 5% | gated |
| bulk/insert_many_indexed/10000 (BULK-2) | 10.7% | 32% | report | 7.3% | 22% | gated |

Notes. The x86_64 runners are about twice as noisy within a job as the arm64 ones; the §4.4 local table (i5-9400F, single binary) had *N* 0.6–5.9%. `doc/aggregate_group/10000` is report-only on both runners (*T* 73% / 59%): `$group` uses a per-process `RandomState`, so every run of the same binary draws a different collision pattern. One of the 6 A/A jobs flagged a suspect in its first attempt (x86_64 `idx/insert_amplification/scalar3` +16.7%, 2/3 pairs); confirmation cleared it. For Stage 1 claims: every roadmap target is gated on at least one runner; DOC-11 at 100k is gated on arm64 only. Improvement claims of ≥ 10× are far outside any *T*.

### 7.2 Per-request (vector, browser) A/A

Three A/A runs (40089246132 and 40089249863 at release size, 40089254047 at PR size): every comparison passed. Largest A/A median ratios: builds ≤ 1.06; exact search ≤ 1.08 at 10k and ≤ 1.013 at 2k (deviations in either direction up to 10% and 7%); filtered ≤ 1.11 warm and ≤ 1.01 cold (the cold case that blocked the v0.12.1 release is stable when run three times); HNSW ANN ≤ 1.12 at 8 MiB but **1.36 (x86_64) and 1.54 (arm64, first attempt) for unquantized 10k at 64 MiB**. The arm64 case would have failed a three-pair gate; confirmation cleared it (6-pair median 1.14). The 50% ANN limit at 64 MiB therefore holds only with confirmation. Browser warm p50 medians ≤ 1.21 (single pairs up to 1.71).

Relative limits are unchanged (+30% build and exact, +50% ANN and filtered). Absolute floors are calibrated per step configuration as 3 × the largest A/A |median(C) − median(B)|. Examples: exact search at 2k 0.01–0.06 ms (was 0.25 ms); at 10k 8 MiB 0.45–0.75 ms; ANN 10k unquantized 8 MiB 3.7–4.2 ms, 64 MiB 1.5–2.3 ms.

Local A/A (i5-9400F, powersave, measured runs pinned to core 5, Stage 0 harness, 77 cases): passed; the local profile has 62 of 77 cases gated, median *N* 3.7%.

### 7.3 Validation on data not used for calibration

| Run | What | Result |
|---|---|---|
| 40104289158 (V1) | Release size, all suites: candidate vs itself **and** vs a2ef466 (cumulative), Tier 0 | All Criterion jobs pass (88 cases × 2 runners × 2 gates); 28 vector + 4 browser comparisons pass (vector re-judged under the calibrated floors, which postdate the run); Tier 0 passes on both runners |
| 40106192059 (V2) | Fresh PR-size A/A, vector + browser, calibrated floors | 14 + 2 comparisons pass |
| local | A/A, 77 cases | Pass; release-built fingerprints equal the committed (dev-built) references |

### 7.4 Positive controls, and the two gates they found blind

| Control | Gate | Result |
|---|---|---|
| Sorted page, ~100 µs extra per call | Criterion, PR and cumulative, both runners | Fails exactly `stabilization/sorted_page/*` and `doc/sorted_page_asc/*` (x86_64 `sorted_page/1000` report-only); +12–16% at 10k against *T* 5–10%; all 6 pairs; other 68 cases neutral |
| Cosine kernel, two redundant norm passes | Per-request vector | First run: exact +51–58% **passed** — the 0.25 ms floor exceeded the 0.26 ms search. After floors were calibrated: fails on all four jobs after confirmation |
| | Criterion | First run: `score_one/Cosine` 1.86–1.92× **passed** — the 2 µs floor exceeded the measurement. After per-case floors (run 40108078013): fails every gated cosine-dependent case (`score_one/Cosine`, `exact_vector`, `find_nearest`) on both runners after confirmation, nothing else |
| BM25 `k1` 1.2 → 1.25 | Tier 0 fingerprints | Fails `fts/bm25_zipf` only — the uniform stabilization corpus cancels `k1` |
| Two allocations per updated document | Tier 0 MEM-2 | Fails `update` +2.8% |
| 2026-09-20 fused kernel | Tier 0 VEC-6 | aarch64 20 → 0, iOS 20 → 0, wasm32 74 → 0, x86_64 115 → 95 |

### 7.5 Tier 0 references

Fingerprints for 77 cases are bit-identical on CI x86_64, CI arm64 and a local x86_64 machine, and between dev and release builds. They depend on the C library: under glibc 2.43 and 2.39, a BM25 control differed by one ulp, while v0.12.1 itself agrees. MEM-2 counts vary ≤ 0.03% between runs and machines. VEC-6 counts are taken from the non-LTO rlib: with LTO on, cargo emits pre-link bitcode whose assembly is scalar, while after LTO the kernels have no symbol.

### 7.6 Deviations from this plan

- VEC-6 counts `dot_similarity`, `euclidean_similarity` and `score_with_query_norm` (which inlines `dot_and_norm_sq`). `norm_sq` has no symbol and is not counted. aarch64-apple-ios is checked; Android aarch64 is not separately compiled (same NEON baseline as aarch64 Linux).
- Criterion groups that existed before (`vector/`, `query/`, `write/`) keep their own sample sizes and sampling mode, so their numbers stay comparable with §5.1. New groups follow §3.1.
- L size (100k) runs in CI for the read-only `doc/` family only.
- CONC-2 is measured A/B and reported, not gated.
- Not implemented: HNSW-4/5/7, HYB-3, VEC-3, BULK-6/7 in CI, IDX-4, CONC-3/4/5, REC-1/3/4, MEM-4.
