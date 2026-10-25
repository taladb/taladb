# v0.13.0 Stage 1B — cached stored-vector norms for exact cosine search: implementation report

Date: 2026-10-25. Branch: `feat/v013-exact-cosine-norms`, from `dev` at `576f7dd` (contains merged PR #103). Performance baseline: v0.12.1, `a2ef46602127919c60a0fe6333d676b1b1ab391e`.
Companions: [V013_STAGE1B_BENCHMARK_RESULTS.json](V013_STAGE1B_BENCHMARK_RESULTS.json), [V013_STAGE1B_MERGE_READINESS.md](V013_STAGE1B_MERGE_READINESS.md); roadmap item 2 in [V013_OPTIMIZATION_ROADMAP.md](V013_OPTIMIZATION_ROADMAP.md).

| Commit | Content |
|---|---|
| `da7b300` | The change: norms in the retained block, cached scoring, accounting; unit tests and `tests/vector_cosine_norms.rs` |
| `7194732` | A cosine block whose norms do not fit the budget keeps its vectors without norms (no regression at the fit boundary) |
| `7e86c0f`, `10c5422` | `examples/exact_vector_profile.rs`: exact search per request by dimension, metric and budget, with heap measurement |
| `5e8a8c9` | Changelog |
| this commit | Stage 1B reports |

## 1. Summary

Exact cosine search no longer recomputes each stored vector's squared norm on every query when the decoded vector block is retained in the cache. The norm is computed once, when the block is decoded, and the cached scan scores with `score_with_norms`, as the HNSW graph path already does.

- **Scores are bit-for-bit identical.** Unit and integration tests compare `f32::to_bits()` between cached, cold and streamed scans and the pre-change kernel. They cover 1–40 dimensions plus 128/383/384/385/768/1536, zero, subnormal, underflowing and overflowing magnitudes, ties, filters, writes, reopening, metric changes and concurrent writes. Every Criterion, Tier 0 and browser fingerprint is equal against `dev` and v0.12.1, on x86_64 and arm64, and HNSW recall@10 is identical in every configuration.
- **VEC-2 (`vector/find_nearest/10000`, 384-d cosine, retained block), CI:**
  - x86_64: **−39.6%** time against `dev` (1.66×) and **−41.2%** against v0.12.1 (1.70×).
  - arm64: **−34.9%** against `dev` (1.54×) and **−36.0%** against v0.12.1 (1.56×).
  - The roadmap target of at least 35% less time is met on x86_64 for both gates, and on arm64 against v0.12.1. It is **missed by 0.1 point on arm64 against `dev`**.
  - On the local i5-9400F, the gain is 19–27%. That machine is memory-bound on this workload (§4.3).
- **Now exactly as cheap as dot product.** Cached cosine search costs what a dot-product search over the same block costs: within 0–5%, with the same instruction count (24.9 vs 24.7 billion over the whole profile run). What remains is one pass over the vectors.
- **No regressions on the CI runners.** Dot, Euclidean, the streaming path, HNSW build/search/recall, hybrid and filtered search, the browser suite and MEM-2 allocations are all neutral, with 0 gate failures across the run's 17 jobs (8 Criterion results, 8 vector and 2 browser jobs, 2 Tier 0). One local-only artefact, a 31–46% slower `score_one/Euclidean` on the i5-9400F, is caused by Intel's JCC erratum and code placement, not by the change. Building both sides with the erratum mitigation makes it disappear (§4.5).
- **Memory.** Cached norms add 4 bytes per vector: +0.26% at 384-d, +0.76% at 128-d, +8.3% at 8-d. Only cosine blocks carry them. Every norm byte is charged in the reservation, the retained total, pinned loans, eviction and the peak. The retained block's real heap matches the accounted figure to the byte, apart from a constant 113 B of entry overhead that was never counted (89 B before).
- **No public API, C ABI, storage-format, default or planner change.** `VectorBlock` and `CachedVectors`, both public, are untouched; the norms live in a new crate-private type.

## 2. What changed

### 2.1 Cause

`Collection::find_nearest_in` scored every stored vector with `score_with_query_norm`. For cosine, that computes `dot_and_norm_sq`: a dot product and the stored vector's squared norm, two reductions per vector per query. The norm depends only on the stored vector. The audit measured it at 89.7% of `vector/find_nearest/10000`. `score_with_norms` and `norm_sq` already exist for the graph path and are documented as bit-equal to the fused computation.

### 2.2 Change

**Retained block.** The search cache retains a crate-private `RetainedVectors { block: VectorBlock, norms: Vec<f32> }` (`vector.rs`) instead of a bare `VectorBlock`. `norms` holds `norm_sq` of each vector in block order for a cosine index, and is empty otherwise. `RetainedVectors::bytes()` is `block.bytes() + norms.capacity() × 4`. `search_cache.rs` stores and lends `Arc<RetainedVectors>` through a private `CachedBlock`, replacing its use of the public `CachedVectors`. Every accounting figure goes through `bytes()`: retained total, LRU eviction, pinned loans, admission and peak.

**Why not a field on `VectorBlock`.** `VectorBlock` is a public struct with public fields in the published `taladb` crate. Adding any field breaks struct literals in downstream code, which would be a public API change. The roadmap's wording ("add `norms` alongside `values` in `VectorBlock`") is met in substance: the retained in-memory block carries the norms.

**Scoring.** The `score` closure takes `stored_norm_sq: Option<f32>`:

- `Some` scores with `score_with_norms`.
- `None` scores with `score_with_query_norm`, unchanged.

The three cached-block sites pass `retained.norm_sq(i)`:

- unfiltered;
- dense ID set;
- filter visitor over the block.

The streamed sites (sparse point reads, storage scans, non-retaining cold scans) pass `None`. For Dot and Euclidean, `score_with_norms` delegates to `score_with_query_norm`, so they compute exactly as before.

**Decode.** The cold scan reserves `count × (4·dims + 16 + 4)` bytes for a cosine index, against `count × (4·dims + 16)` before. When it retains, it computes `norm_sq` once per vector, scores the first query with that norm and pushes it into the block. A scan that does not retain is the existing streaming path, untouched.

**Fit boundary (`7194732`).** If the reservation with norms fails, the scan tries the vectors alone, which is the reservation the code made before this change. That happens when the budget lies between the two sizes, or under concurrent loans. A block retained this way has no norms, so it scores by recomputing them, as before. Without this fallback, a 384-d cosine collection of 5,392–5,405 vectors at the 8 MiB phone/WASM default would have switched from a retained block to streaming. That is about 2.5× slower per query (4.1 ms vs 1.6 ms at 10k). The band is 0.26% of the budget at 384-d and 8.3% at 8-d. Only norms that are actually retained are charged.

### 2.3 Properties preserved

| Property | How |
|---|---|
| Score bits, ranking, ties | `norm_sq` has the lane count and summation order of the norm loop in `dot_and_norm_sq`; `dot_similarity` matches its dot loop; the final `dot / (query_norm · sqrt(norm_sq))` and the zero checks are the same expression. Tested with `to_bits()` (§3) |
| Dimension validation | Unchanged: the query is checked against the index; the scan skips stored vectors whose length differs; a block only holds vectors of the index's dimension |
| Generation invalidation | Unchanged: the cache key, revision check and epoch apply to `CachedBlock` exactly as to `CachedVectors`. Norms are built with the vectors, in one scan, and never outlive them |
| Streaming path | Unchanged code; the probe shows 0.97–1.01× at 1 and 8 MiB |
| Dot and Euclidean | Retain no norms (byte counts unchanged, tested) and call the same out-of-line `score_with_query_norm`, whose code is unchanged on every target (§6) |
| A metric change on the same field | A dropped index evicts its block. If a block without norms ever met a cosine index, `norm_sq(i)` is `None` and the norm is recomputed. A block with norms under another metric is scored by that metric, ignoring them. Tested by recreating the index Cosine → Dot → Cosine → Euclidean → Cosine |
| HNSW, ANN rescoring, FTS, planner, Stage 1A | Not touched. ANN candidates are still rescored from point reads with `score_with_query_norm`; reusing the block there is roadmap item 5 (Stage 2) |

## 3. Correctness

`packages/core/tests/vector_cosine_norms.rs` compares, with `f32::to_bits()`, the exact-search results of three runs and an oracle:

- streaming (retention disabled);
- cold (the query that builds the block);
- warm (the block reused);
- the oracle: every stored vector scored with `score_with_query_norm`, non-finite scores dropped, best score first, then lowest ID.

The corpus mixes random vectors with zero and −0 vectors, ×1e-20 (squares underflow), ×1e-41 (subnormal components), ×1e18, ×1e30 (squared norm overflows to infinity), and identical and opposite vectors (exact ties). Queries include tiny, huge and zero vectors and a stored vector (score 1.0).

| Test | Coverage |
|---|---|
| `cosine_scores_on_a_retained_block_match_streaming_and_the_kernel_bit_for_bit` | 1, 7, 8, 9, 13, 128, 383, 384, 385, 768, 1536 dimensions × 5 queries × top-k 1/10/all; retained bytes = n × (20 + 4·dims) |
| `dot_and_euclidean_retain_no_norms_and_score_as_before` | Dot and Euclidean at 8, 9, 384 dimensions; retained bytes = n × (16 + 4·dims) |
| `filtered_searches_over_a_retained_block_match_streaming` | The three filtered paths over a retained block (10, 200 and 500 matches: point reads, ID set, visitor) at 9 and 384 dimensions |
| `a_block_whose_norms_do_not_fit_keeps_its_vectors_alone` | Budgets 47,999 / 48,000 / 51,999 / 52,000 / 52,001 B for 1,000 × 8-d (stream / vectors only / vectors only / with norms / with norms); 1 MiB vs 1,050,300 B for 675 × 384-d; Dot boundary unchanged; peak ≤ budget, active 0 |
| `blocks_of_several_indexes_are_charged_and_evicted_with_their_norms` | Two cosine blocks exactly at the budget, then one byte short: LRU eviction with norm bytes counted |
| `inserts_updates_and_deletes_rebuild_the_norms` | Insert, update through a second handle, embedding set to `null`, delete, `delete_many`, at 13 and 384 dimensions; each followed by searches on both handles against the oracle and an exact retained-byte check |
| `reopening_a_database_rebuilds_identical_norms` | File-backed, 385-d: warm before closing, cold and warm after reopening, a write after reopening |
| `recreating_the_index_with_another_metric_never_reuses_its_norms` | Cosine → Dot → Cosine → Euclidean → Cosine on the same field |
| `concurrent_searches_score_every_hit_from_its_own_vector_during_writes` | 4 readers × 40 searches during 60 updates/deletes/inserts at 384-d: every hit's score bits equal `score_with_query_norm` of the vector in the returned document (same snapshot), order non-increasing; final state against the oracle; accounting settles (active 0, retained ≤ budget, peak ≤ allowance) |

Unit tests:

- `vector::tests::cached_norm_scores_match_recomputed_norm_scores_bit_for_bit`: 51 lengths × 36 magnitude combinations plus zero, −0, constant, negated and identical vectors, for all three metrics.
- `search_cache::tests::cached_norms_are_charged_to_retained_pinned_and_peak_bytes`: admission, retained, peak and pinned bytes include the norms. The existing cache unit tests now run with norm-carrying blocks.

**Mutation checks** (each applied, run, reverted):

| Mutation | Tests failing |
|---|---|
| Norm computed with a sequential sum instead of `norm_sq` (same value up to rounding) | 6 of 9 integration tests |
| Warm scan uses the neighbour's norm | 7 of 9 |
| Norm bytes not charged in `RetainedVectors::bytes` | 4 of 9, and the unit test |
| Fit-boundary fallback removed | 2 of 9 |

**Results** at the final head `5e8a8c9`:

- **Workspace:** 57 suites, **791 passed**, 0 failed, 3 ignored (780 before, plus 11 new). No existing assertion changed. The existing cache unit tests' helper now builds blocks with norms.
- **Lints:** rustfmt clean; clippy clean for the workspace (all targets, all features) and for `taladb-web` on wasm32 with and without default features.
- **WASM:** `taladb-web` builds for `wasm32-unknown-unknown` (release); its 22 tests pass under Node with `wasm-bindgen-test-runner` 0.2.114, the version the lockfile pins. The browser suite (Chromium, worker + OPFS) passed in CI against both baselines.

## 4. Performance

### 4.1 CI — run 40177478711 (release size; suites vector, criterion, browser, tier0; PR base and cumulative)

Each comparison ran on one runner with 3 interleaved pairs. Times are Criterion medians.

| Case | x86_64 vs `dev` (EPYC 7763) | x86_64 vs v0.12.1 (EPYC 9V45) | arm64 vs `dev` | arm64 vs v0.12.1 |
|---|---:|---:|---:|---:|
| **VEC-2** `vector/find_nearest/10000` | 1145 → 691 µs (**−39.6%**) | 602 → 354 µs (**−41.2%**) | 952 → 620 µs (**−34.9%**) | 949 → 607 µs (**−36.0%**) |
| `vector/find_nearest/1000` | 203 → 158 µs (−21.8%) | 103 → 81 µs (−21.3%) | 157 → 121 µs (−22.8%) | 156 → 120 µs (−23.1%) |
| `stabilization/exact_vector/10000` (8-d) | −19.9% | −11.4% | −16.0% | −16.2% |
| `vector/score_one/*` (12 cases) | 0.99–1.01 | 0.94–1.01 | 0.99–1.01 | 1.00–1.00 |
| `stabilization/hnsw`, `hybrid`, `hyb/*` | 0.99–1.02 | 0.97–0.99 | 0.99–1.02 | 0.99–1.01 |

- **Verdicts:** 0 regressions and 0 failures across all 8 Criterion results (77 cases at 1k/10k, plus 11 at 100k).
- **Fingerprints:** all equal against both baselines.
- **Vector suite:** all 8 jobs passed (HNSW at 64 MiB and 8 MiB, resumable builds, filtered, concurrency; none and binary quantization). Recall@10 is identical in every configuration. Exact search improves 0.64–0.71× at 64 MiB, where the block is retained. It is neutral at 8 MiB, where the 10k × 384-d block streams. ANN p50 and build time are neutral.
- **Browser:** both jobs passed. The PR job at 8 MiB needed a confirmation round on HNSW build time, which differs by up to 2.9× within the baseline's own runs (1,556–4,500 ms); the cumulative job measured 1,962 vs 1,962 ms. Unfiltered exact search in the browser (5k × 128-d, retained at 8 MiB) went from 0.65 to 0.59 ms (0.90×).
- **Tier 0:** passed on both runners: 77 fingerprints equal the v0.12.1 references, MEM-2 allocation counts pass, VEC-6 passes with no notes.

### 4.2 Dimensions, budgets and metrics — local probe

`exact_vector_profile`, i5-9400F pinned to one core, 7 interleaved rounds of v0.12.1, `dev` and candidate builds. The table gives the median of per-round p50s. Top-10 fingerprints (IDs and score bits) are identical across all three builds in every row.

| Configuration | Path | `dev` | Candidate | Change |
|---|---|---:|---:|---:|
| 10k × 8-d cosine, 64 MiB | retained | 174.4 µs | 158.5 µs | −9.1% |
| 10k × 128-d cosine, 64 MiB | retained | 523.8 µs | 395.2 µs | −24.5% |
| 10k × 384-d cosine, 64 MiB | retained | 1556.5 µs | 1147.4 µs | **−26.3%** |
| 10k × 768-d cosine, 64 MiB | retained | 3105.2 µs | 2116.9 µs | −31.8% |
| 10k × 1536-d cosine, 64 MiB | retained | 6125.2 µs | 4340.5 µs | −29.1% |
| 10k × 384-d cosine, 16 MiB | retained | 1572.0 µs | 1139.0 µs | −27.5% |
| 10k × 384-d cosine, 8 MiB | streaming | 4141.2 µs | 4161.8 µs | +0.5% |
| 10k × 384-d cosine, 1 MiB | streaming | 4214.1 µs | 4150.2 µs | −1.5% |
| 1k × 384-d cosine, 64 MiB | retained | 176.9 µs | 135.5 µs | −23.4% |
| 5,390 × 384-d cosine, 8 MiB (fits with norms) | retained | 817.4 µs | 582.1 µs | −28.8% |
| 5,400 × 384-d cosine, 8 MiB (fits without norms only) | retained, no norms | 826.6 µs | 834.4 µs | +0.9% |
| 675 × 384-d cosine, 1 MiB (fits without norms only) | retained, no norms | 144.4 µs | 141.5 µs | −2.0% |
| 10k × 384-d dot, 64 MiB | retained | 1097.3 µs | 1112.6 µs | +1.4% |
| 10k × 384-d Euclidean, 64 MiB | retained | 1284.4 µs | 1285.6 µs | +0.1% |
| 10k × 384-d dot, 8 MiB | streaming | 3780.4 µs | 3673.8 µs | −2.8% |
| 10k × 384-d Euclidean, 8 MiB | streaming | 3844.0 µs | 4124.2 µs | +7.3% (9-round rerun: +2.7%) |

- **Cold queries.** The query that builds the block computes the norm once per vector, which replaces the norm it already computed inside `score_with_query_norm`. Cold p50 changes by −3% to +7% (384-d: 5.26 → 5.41 ms, +2.9%), within this machine's run-to-run spread.
- **Euclidean streaming row.** These binaries carry identical erratum hits in `score_with_query_norm` on both sides (§4.5). v0.12.1 has none, and runs Euclidean 3–7% faster than `dev` across these runs with byte-identical vector code. Placement effects of that size bound what this method can resolve on this CPU.
- **Against v0.12.1.** The candidate's ratios are the same within 1–2 points: −26.8% at 384-d.

### 4.3 Why the local gain is smaller: the measured bottleneck

`perf` on the probe (10k × 384-d, 64 MiB, 3,000 warm queries, user space):

| Build, metric | Cycles | Instructions | Cache misses | Top symbol | Cycles per vector in the kernel |
|---|---:|---:|---:|---|---:|
| `dev`, cosine | 19.5 G | 37.8 G | 830 M | `score_with_query_norm` 84.4% | ≈ 549 |
| candidate, cosine | 15.9 G | 24.9 G | 822 M | `dot_similarity` 76.7% | ≈ 406 |
| candidate, dot | 16.1 G | 24.7 G | 827 M | `dot_similarity` 78.2% | ≈ 418 |

- **Compute is halved, as designed.** The candidate executes 34% fewer instructions, the same number as the dot metric. Memory traffic is unchanged: the same cache misses on all three rows.
- **Kernel cost moves from 2.0× to 1.0× dot only where the data is cache-resident.** In L1 (`vector/score_one`), cosine already costs 2.0× dot, and cached cosine scoring costs 1.0×. The 10k × 384-d block is 15.5 MB, larger than the i5-9400F's 9 MB L3, so every query streams it from DRAM.
- **Why only part of the saving shows.** The removed norm pass ran on a vector the dot pass had just brought into L1, overlapping with the DRAM latency of the next vector. Removing it saves about 140 of its cycles per vector, not the ~190 it costs in isolation.
- **The remaining cost is the single pass over the vectors**, which is what dot product pays too. Going further requires reading fewer bytes per vector. That means a storage or representation change (out of scope) or the roadmap's research items.
- **The CI runners confirm the explanation.** The EPYC runners have 32 MB of L3 per CCD, holding the whole block, and reach −40% to −41%. arm64 reaches −35% to −36%. At 1k vectors (1.5 MB, cache-resident), the local time falls by about 35% once a fixed ≈58 µs per query (mostly loading and decoding the 10 result documents, 5% of the 10k profile) is subtracted.

### 4.4 Regression controls

- **HNSW.** Build time, ANN p50 at `efSearch` 50–400, storage reads per query and recall@10 are unchanged at both budgets, both quantizations and both gates (§4.1). ANN does not use the changed code.
- **Dot and Euclidean.** Neutral in CI (`score_one` 0.94–1.01) and in the probe's retained rows.
- **Filtered exact search.** The CI filtered workload and the browser's filtered exact cases are neutral. The new integration test covers each filtered path's results.
- **Concurrency (CONC-2).** Report-only workload, ran on both runners without failure.

### 4.5 A local-only artefact: `vector/score_one/Euclidean` on the i5-9400F

The local Criterion A/B reported `score_one/Euclidean` 31–46% slower against `dev` (10–19% against v0.12.1), confirmed over 6 pairs. It is neutral on every CI runner. Findings:

1. **The code is identical.** `euclidean_similarity` is byte-identical in both benchmark binaries; only its address moved, by 0xF0 bytes, because unrelated code before it grew.
2. **Erratum hit.** In the candidate binary, the loop's backward `jne` spans 0x3865df–0x3865e0 and crosses a 32-byte boundary. On Skylake-derived cores (the i5-9400F is Coffee Lake), Intel's JCC-erratum microcode stops such a loop from running out of the decoded-µop cache. In the `dev` binary the jump ends at offset 17 and is unaffected.
3. **Controls.** v0.12.1 and `dev` differ by 23% on the same case with byte-identical source (0.258 vs 0.210 µs at 1536-d). With both sides built with `-C llvm-args=-x86-branches-within-32B-boundaries`, no kernel loop crosses a boundary in either binary, and `score_one/Euclidean` is neutral at every dimension (0.972–1.007×). The CI runners (AMD EPYC, arm64) are not affected by the erratum.

The same placement lottery appears in the probe binaries (§4.2). It is a property of the toolchain and this CPU, not of the change. It does mean local x86 A/B results on this machine can move ±7–46% for kernels the change does not touch.

## 5. Memory and cache accounting

| | Per vector, before → after | Change |
|---|---:|---:|
| 8-d cosine | 48 → 52 B | +8.33% |
| 128-d cosine | 528 → 532 B | +0.76% |
| 384-d cosine | 1,552 → 1,556 B | +0.26% |
| 768-d cosine | 3,088 → 3,092 B | +0.13% |
| 1536-d cosine | 6,160 → 6,164 B | +0.06% |
| Dot, Euclidean (any dimension) | unchanged | 0 |

**Accounted vs real heap.** The probe's counting allocator measures the heap that the cold query leaves allocated, i.e. the retained block. Across all 16 configurations that measurement equals the cache's `retained_bytes` plus a constant: 113 B for the candidate, 89 B for `dev`. The constant is the entry's `Arc` and key, which `bytes()` has never counted; the extra 24 B is the norms vector's header. For 10k × 384-d that is 15,560,113 B of heap for 15,560,000 B accounted. Peak bytes never exceed the memory budget, and active bytes return to 0 after every query, in the probe, the integration tests and the CI memory profile.

**Where blocks stop fitting** (largest 384-d cosine collection retained with norms / vectors only):

| Budget | With norms | Vectors only | Between the two |
|---|---:|---:|---|
| 1 MiB | 673 | 675 | retained without norms (as before) |
| 8 MiB | 5,391 | 5,405 | retained without norms (as before) |
| 16 MiB | 10,782 | 10,810 | retained without norms (as before) |
| 64 MiB | 43,129 | 43,240 | retained without norms (as before) |

Under LRU between several indexes, a cosine block takes 4 B per vector more of the shared budget. A workload whose blocks fit the budget exactly before can now evict one of them, as the integration test demonstrates.

## 6. Code generation

- **VEC-6** (Tier 0 tool, non-LTO rlib) is unchanged on every target and kernel against `dev` and v0.12.1:
  - x86_64: `dot_similarity` 41, `euclidean_similarity` 48, `score_with_query_norm` 115 vector instructions;
  - aarch64 Linux and aarch64 iOS: 10 / 12 / 20;
  - wasm32: 34 / 40 / 74.
- **Inside `find_nearest_in`,** on all four targets:
  - The cached cosine path calls the same out-of-line, vectorised `dot_similarity`.
  - Streamed vectors and Dot/Euclidean call the same out-of-line `score_with_query_norm` as before.
  - The decode-time `norm_sq` is inlined with packed f32 arithmetic: multiply/add 10/10 on x86_64 and wasm32, 6/5 on aarch64 Linux and iOS (0/0, 2/0, 2/0 and 0/0 in `dev`, which inlined no kernel).
  - No scalar copy of a kernel was introduced.

Evidence: `local/codegen-*.json`, `local/codegen-find-nearest-in.txt`.

## 7. Deviations, missed targets and unverified items

| Item | Status |
|---|---|
| ≥ 35% on VEC-2 | **Met** on x86_64 (−39.6% / −41.2%) and arm64 vs v0.12.1 (−36.0%). **Missed on arm64 vs `dev` by 0.1 point** (−34.9%). Locally 19–27% on an i5-9400F; bottleneck in §4.3 |
| "Extend `VectorBlock`" | **Deviation:** the norms live in crate-private `RetainedVectors` beside `VectorBlock`, because adding a field to the public struct is a public API change |
| Fit-boundary fallback | **Addition beyond the literal brief:** a cosine block whose norms do not fit keeps its vectors without norms, instead of streaming. It prevents a ~2.5× regression at the boundary and changes no default |
| Local `score_one/Euclidean` failure | Not a regression of this change: JCC-erratum placement on the i5-9400F, proven by the mitigation control; neutral on all CI runners |
| ARM64 | Measured on the GitHub `ubuntu-24.04-arm` runner only. No Apple silicon, Android or iOS device timings. iOS codegen checked (VEC-6, `find_nearest_in`) |
| macOS, Windows, Safari, Firefox | Not measured. The browser suite is Chromium only |
| Memory measurement on arm64 | The heap-vs-accounting probe ran on x86_64 only. The accounting code has no architecture-specific path. The CI memory profile (ANN mode) passed its accounting asserts on arm64 |
| ANN rescoring from the block | Not done, by design: roadmap item 5 (Stage 2) |
| Probe's first sweep | Superseded: random document IDs made its fingerprints incomparable. The probe now uses fixed IDs; the first sweep is kept in the evidence, marked superseded |
| Evidence | CI run 40177478711 artifacts (90-day retention) and `~/.cache/taladb-stage0/evidence/v013-stage1b/` (local runs, probe JSONL, perf, codegen, JCC check, results generator). Not attached to a release |
| Stage 1C / Stage 2 | Not started |
