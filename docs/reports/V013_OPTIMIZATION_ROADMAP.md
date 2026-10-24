# v0.13.0 optimization roadmap

Roadmap date: 2026-10-24. Baseline: **v0.12.1**, commit `a2ef46602127919c60a0fe6333d676b1b1ab391e`.
Evidence: [V013_BASELINE_AUDIT.md](V013_BASELINE_AUDIT.md). Gates and workload IDs: [V013_BENCHMARK_PLAN.md](V013_BENCHMARK_PLAN.md).

This roadmap ranks candidate engine changes for v0.13.0. Every item cites a measurement on v0.12.1, a profile, or a code path that explains a measurement. Nothing here was implemented. Each item states what it must not change; the global constraints are: no new features, no public API or C ABI change, no storage-format change, no change to v0.12.1's correctness and compatibility guarantees, and no optimization that is not justified by a measurement.

## How items are ranked

| Column | Meaning |
|---|---|
| **Benefit** | Measured cost the change targets, and the expected improvement on the named workload. "Expected" figures are bounded by the profile share of the code being removed; they are targets for the acceptance gate, not claims. |
| **Risk** | Engineering risk to correctness: **L** = local change with an existing exact oracle; **M** = touches shared machinery (cache accounting, snapshot consistency, planner) or needs new oracles; **H** = changes algorithm output. |
| **Compat** | **None** = results, ordering guarantees, scores and formats identical. **Observable** = something a caller can see changes, though no documented contract does (for example the order of unsorted results that is already nondeterministic). **Decision** = needs an explicit product decision before work starts. |

Priority = benefit ÷ risk, with any item whose compatibility impact needs a decision placed after all items that do not.

## Summary

| Rank | Item | Workload | Baseline | Target | Risk | Compat | Stage |
|---|---|---|---|---|---|---|---|
| — | Measurement foundation | all | — | gates live | L | None | 0 |
| 1 | Two-sided indexed ranges as one bounded scan | DOC-11 | 12.5 ms @100k, 1.16 ms @10k | ≥ 50× / ≥ 10× faster | L | Observable (order of unsorted results) | 1 |
| 2 | Retain stored-vector norms for exact cosine | VEC-2 | 1.70 ms @10k × 384-d | ≥ 35% faster | L | None | 1 |
| 3 | Sorted pages without materialising the index | DOC-6, DOC-12 | 0.93 ms @10k | asc ≥ 20×, desc ≥ 20% faster | L–M | None | 1 |
| 4 | Filter on a projected decode for unindexed scans and counts | DOC-4, DOC-13 | 7.0 ms @10k | ≥ 40% faster; DOC-5 ≤ +T | M | None | 2 |
| 5 | Rescore ANN candidates from the retained vector block | HNSW-3, CONC-2 | ~26% of ANN CPU in point reads | ≥ 15% faster; 4-thread CPU/request ≤ +40% | M | None | 2 |
| 6 | Partial retention of the exact-search vector block | VEC-2 at 8 MiB | 4.07 ms vs 1.59 ms cached | ≥ 25% faster at 8 MiB | M | None | 2 |
| 7 | Bulk-update allocation and clone churn | BULK-3 | 51.6 ms @10k | ≥ 10% faster | M | None | 3 (after a targeted profile) |
| 8 | Sparse-filter ANN walks the whole graph | HNSW-6 | 4.74 ms vs 0.13 ms exact | ≥ 10× | L | Decision | 3 |
| 9 | Hashing of ULID-keyed sets in BM25/hybrid/intersections | FTS-1, HYB-1 | ~10% of BM25 CPU | ≥ 7% faster | M | Decision (hash-flooding) | 3 |
| R1 | HNSW at the 8 MiB budget; build cost | HNSW-1/3 | 74 s build, 6.6 ms ANN | measure first | H | Decision | Research |
| R2 | Memory envelope: redb cache, in-memory backend, browser | MEM-1, REC-5 | 922 MiB peak, browser unmeasured | measure first | — | Decision | Research |

## Stage 0 — measurement foundation (blocks every other stage)

No engine change may merge before these are in place, because without them a v0.13.0 improvement cannot be told apart from noise and a regression in an ungated area cannot be seen.

| Step | Work | Acceptance |
|---|---|---|
| 0.1 | `performance.yml`: `repeats=3` on every event; cold cases report-only below 5 runs; HNSW step at 64 MiB in addition to 8 MiB (`--cache-bytes` pass-through in `vector-bench-ci.py`) | The two failures on record (v0.12.1 release attempt 1, 2026-10-21 push) would not have fired under the new rule when replayed against their stored JSON |
| 0.2 | Criterion A/B mode in `vector-bench-ci.py` (plan §3.1, §4.2), confirmation re-run (§4.3) | CI A/A calibration on `ubuntu-latest` and `ubuntu-24.04-arm` passes with identical binaries; a positive control (black-box loop in `Collection::find_with_options`) fails `sorted_page` |
| 0.3 | Tier 0 in `ci.yml`: workload fingerprints, VEC-6 codegen counts, MEM-2 allocation counts | < 2 min; a deliberately fused kernel (the 2026-09-20 shape) fails VEC-6 on wasm32 and aarch64 |
| 0.4 | Harness extensions needed by stages 1–2: DOC-1/2/7/8/9/11/12/13, BULK-2/5, FTS-3/5, HYB-2, CONC-2, storage-read counter for HNSW | Each compiles and runs unmodified on the v0.12.1 worktree and produces matching fingerprints on v0.12.1 and `dev` |
| 0.5 | Cumulative comparison against `a2ef466` on schedule and release | Runs on the weekly schedule; first run reports zero cases beyond *T* |

Risk L, Compat None: only `benches/`, `examples/`, `scripts/` and workflows change.

## Stage 1 — local, low-risk changes with exact oracles

### 1. Two-sided indexed ranges as one bounded scan

**Evidence.** `find({ n: { $gte: x, $lt: x + 100 } })` on an indexed integer field returns 100 documents in 12.53 ms at 100k documents and 1.16 ms at 10k, while a one-sided range returning the same 100 documents takes 0.087 ms and 0.081 ms (engine-level probe, `artifacts/v013-baseline/local/range-probe.jsonl`; the Node benchmark shows the same 12.55 ms). Cost grows linearly with the collection, not the result.

**Cause.** `bounded_range_from_and` (`query/planner.rs:452`) plans `IndexAnd` of two half-open scans — `[x, ∞)` and `(−∞, x+100)` — so that a document whose *different array elements* satisfy each bound is still found. The executor (`query/executor.rs`, `QueryPlan::IndexAnd`) materialises a `HashSet` of every ULID in each half (≈ N/2 each) and intersects them. That intersection also returns documents in `HashSet` iteration order, which, with `RandomState`, differs between processes.

**Change.** When the field's array counter in `META_INDEX_ARRAYS_TABLE` is zero, plan the intersection of the two bounds as range scans instead of an ID intersection. The engine already does exactly this for vector-filter resolution: `query/index_filter.rs:195` checks the same counter and calls `intersect_ranges`, which keeps the Int/Float twin blocks that `lower_bound_plan`/`upper_bound_plan` emit. Reuse that function rather than writing a second merge. Keep `IndexAnd` for fields with arrays and for encrypted fields.

**Benefit.** Target ≥ 10× at M and ≥ 50× at L on DOC-11; bounded below by the one-sided cost (~0.09 ms). Also removes two O(N) ID sets per query (memory).

**Must not change.** The set of matching documents for any filter; behaviour on array, mixed-number, NaN, −0.0 and encrypted fields; results of sorted queries.

**Compat: Observable.** Unsorted two-sided range results come back in index order instead of the current per-process random order. No documented contract covers unsorted order, and the current order is not reproducible across runs, but a caller relying on `limit` without `sort` will see a different (now stable) subset. Record it in the changelog.

**Acceptance.**
- DOC-11 meets the target under plan §4.2; DOC-3, DOC-6, DOC-7 within *T*.
- New parity test: for each of {scalar ints, scalar floats, mixed int/float, arrays, missing/null, NaN, ±0.0, embedded-NUL strings, encrypted field}, two-sided ranges with all four bound combinations return the same ID *set* as a `FullScan` of the same filter; planner tests assert `IndexRange` only in the eligible cases.
- Existing `query_engine.rs`, `secondary_indexes.rs`, `index_array_scaling.rs`, `stabilization.rs` pass unchanged.

### 2. Retain stored-vector norms for exact cosine search

**Evidence.** `vector/find_nearest/10000` (384-d cosine, decoded block cached) spends 89.7% of its time in `score_with_query_norm`; the cosine kernel costs 96.5 ns against 48.6 ns for dot at 384-d — exactly two reductions instead of one, at every dimension measured (`vector/score_one`).

**Cause.** The exact path (`collection.rs:1456`, and ANN rescoring at `vector_api.rs:873`) calls `score_with_query_norm`, which recomputes the stored vector's squared norm on every comparison. The graph path already avoids this with `score_with_norms` and a stored `norm_sq`.

**Change.** Add a `norms: Vec<f32>` alongside `values` in the in-memory `VectorBlock` (filled once when the block is decoded) and score cached blocks with `score_with_norms`. The on-disk vector record is untouched.

**Benefit.** Target ≥ 35% on VEC-2 at 384-d (bounded by half of the 89.7% kernel share ≈ 45%); smaller at low dimensions (8-d `exact_vector`: kernel is 45% of the time). Cached-block path only; the streaming path (block over budget) is unchanged.

**Must not change.** Score bits. `norm_sq` accumulates per lane in the same order as the norm loop inside `dot_and_norm_sq`, and the scan already skips vectors whose length differs from the query, so the scores are bit-identical by construction — prove it with a test over random vectors including non-multiple-of-8 tails, alongside the existing `dot_and_norm_sq_matches_the_chunks_exact_original_bit_for_bit`.

**Memory.** +4 bytes per vector counted against the vector cache budget: +0.26% at 384-d, +3% at 128-d, +12.5% at 8-d. The budget at which a block stops fitting moves by that fraction; report it.

**Compat: None.** **Acceptance:** VEC-2 target met on x86_64 and arm64; fingerprints (IDs + score bits) identical on every vector test and VEC-4; VEC-6 shows no loss of vector instructions in `norm_sq`/`dot_similarity` on wasm32, aarch64 or x86_64.

### 3. Sorted pages without materialising the index

**Evidence.** `sorted_page/10000` (`sort n desc, skip 20, limit 10`) takes 0.93 ms against 15.8 µs for an indexed page of the same size, and ~10 ms at 100k (stabilization report) — linear in N for a 30-row answer. Profile: redb range iteration and guard drops 45%, allocator 22%.

**Cause.** `index_ordered_entries` (`query/executor.rs:855`) scans the entire index unbounded and allocates a `Vec<u8>` per entry for its value prefix, then selects the page.

**Change.**
- *Ascending*: stop the forward scan once `skip + limit` entries plus the complete tie run at the boundary have been seen (ties need the existing ULID tie-break).
- *Descending*: keep only the last `skip + limit` entries plus their tie run in a bounded buffer that reuses its allocations. The scan stays O(N) because the internal read API has only forward iteration; reverse iteration would need a new method on the public `ReadTxn` trait, which is an API change and is **not** proposed for v0.13.0.

**Benefit.** DOC-12 (ascending) ≥ 20× at M; DOC-6 (descending) ≥ 20% at M from removing per-entry allocation. Also caps memory at O(skip + limit) instead of O(N).

**Must not change.** Page contents and order for any sort, including boundary ties, multi-key sorts and the fallback conditions the stabilization work defined (arrays, mixed numbers, NaN, signed zero, encrypted fields).

**Compat: None.** **Acceptance:** the existing page-vs-full-sort regressions in `stabilization.rs` and `aggregate_sorted_index.rs` pass; a new test drives `skip` across tie-run boundaries in both directions; DOC-6/DOC-12 targets met; aggregation `$sort`+`$limit` (which uses the same helper) within *T*.

## Stage 2 — shared machinery, new oracles needed

### 4. Evaluate unindexed filters on a projected decode

**Evidence.** `unindexed_selective/10000` (filter on one integer field, no match) takes 7.0 ms; ~70% of it is full document decoding — postcard `Value` enum decoding 20.8%, UTF-8 validation of strings 16.5% (each document carries a 1 KiB string), field-vector construction 12%, drops and allocator ~15%. Node: unindexed `count` over 100k documents 53 ms, unindexed `find` 59 ms.

**History.** The stabilization work moved projected decoding *out* of the ordinary full-scan path after it caused a small regression for scans where most documents match. This item must not repeat that.

**Change.** For `count` with a filter, evaluate the filter on a projected decode only (no full decode at all). For `find`, evaluate on the projected decode and fully decode only matching documents; keep the direct collector when the filter is `All` or needs whole-document access.

**Benefit.** DOC-13 (count) and DOC-4 (selective find) ≥ 40% at M. **Cost bound:** DOC-5 (`range_scan_unindexed`, 50% match) must stay within *T* — the case the earlier attempt regressed.

**Risk M.** Filter semantics on nested paths, arrays, missing versus null, regex and `$not` must be identical under projection. Oracle: run every filter in `tests/fixtures/parser_parity.json` and `query_engine.rs` through both paths and compare ID lists.

**Compat: None.**

### 5. Rescore ANN candidates from the retained vector block

**Evidence.** Concurrency probe (2,000 × 128-d, binary graph, `efSearch` 100, warmed threads, `artifacts/…/probe-*.json`): exact search scales linearly to 4 threads (CPU per request 0.126 → 0.129 ms), ANN does not (0.130 → 0.229–0.239 ms at 4 threads, at both 8 MiB and 64 MiB, so not budget eviction). Single-thread ANN profile: redb point lookups ~26% of CPU; at 4 threads `get_page` rises from 8.4% to 12% and glibc allocation from ~1.3% to ~6.8% — shared page-cache and allocator contention, not lock waiting (4 threads keep 3.6 cores busy).

**Cause.** After the graph walk, ANN rescoring reads each of about `efSearch` candidates with its own `txn.get` and `decode_f32_vec` (`vector_api.rs:869–879`), even when the exact path's decoded `VectorBlock` for the same generation is retained.

**Change.** When a block for the current table generation is retained, rescore from it; otherwise keep point reads. Generation equality is what guarantees the "same snapshot" rule documented at that call site.

**Benefit.** ≥ 15% on HNSW-3 at budgets where the block is retained; CONC-2 4-thread CPU per request no more than +40% over 1 thread (from +80%).

**Risk M.** Snapshot consistency (`vector_score_and_body_share_a_snapshot_even_when_cache_is_warm` must keep passing, and a new test must interleave a write between graph walk and rescoring), and cache loans for the block.

**Compat: None** — identical f32 inputs give identical scores and results.

### 6. Partial retention of the exact-search vector block

**Evidence.** Exact search over 10k × 384-d: 4.07 ms at the 8 MiB WASM/mobile default budget, 1.59 ms at 16 MiB (`hnsw-binary-10k-cache*.json`). The 15.4 MB decoded block does not fit 8 MiB, so every query decodes the whole table. On native (64 MiB default) the same cliff sits near 43k vectors at 384-d: Node flat search is 2.22 ms at 10k but 22.6 ms at 50k (10× for 5× the data).

**Change.** When a block exceeds the budget, retain the prefix that fits and stream only the remainder, inside the existing budget accounting. No default budget changes.

**Benefit.** ≥ 25% on VEC-2 at 8 MiB for 10k × 384-d; gradual instead of cliff-like degradation above the budget.

**Risk M.** Generation-keyed invalidation and concurrent loans in `search_cache.rs`; peak accounting must stay ≤ budget (MEM-3).

**Compat: None.** Results identical (same vectors, same order of offers to the ranker must be preserved, or tie-breaks re-verified).

## Stage 3 — lower benefit, or a decision is needed first

### 7. Bulk-update allocation and clone churn

**Evidence.** `update_many/10000`: glibc allocator ~24%, `memmove` 11%, UTF-8 validation 4.3%, whole-document `clone`/`to_vec`/drop ~5% of CPU. redb is ~15%.

**Before any change:** profile with call graphs (`perf record --call-graph dwarf`) to attribute the allocations to decode, clone, index-key construction and encode. Only the clone is clearly avoidable from the flat profile (≤ 5%); the rest may be inherent to decoding into `Value`. Target ≥ 10% on BULK-3 or drop the item. **Compat: None.**

### 8. Sparse-filter ANN walks the whole graph — *decision needed*

**Evidence.** `vector_filter_profile` at 10k: with a sparse filter (~10 eligible documents), warm ANN computes ~10,000 distances per query and takes 4.74 ms; exact search over the same allowed IDs computes 10 and takes 0.13 ms. Browser CI shows the same shape (1% filters: ANN ~6.2 ms, exact ~0.7 ms). `Auto` mode already selects exact for filtered queries, so only callers that request `mode: Ann` are affected.

**Change.** When the bounded allowed-ID set is no larger than `efSearch`, score it exactly even under `mode: Ann`.

**Why a decision.** The caller explicitly asked for ANN; results would become exact (recall can only rise), and `execution.path`/`reason` would report the exact path. That is observable behaviour of an explicit option. Approve or drop.

### 9. Hashing of ULID-keyed sets — *decision needed*

**Evidence.** SipHash (`Hasher::write` + `hash_one`) is ~9.5% of `bm25/10000` and ~10.6% of `hybrid/10000`.

**Why a decision.** `_id` values can be caller-supplied, so a weaker hasher exposes hash-flooding; a DoS-resistant faster hasher means a new dependency. An alternative that removes hashing from BM25 scoring entirely (merge-joining posting streams that are already ID-ordered) is a larger refactor. Decide whether ≤ 10% on BM25 is worth either.

## Research tracks (measure; do not optimise in v0.13.0 without new evidence)

### R1. HNSW at the 8 MiB budget, and build cost

**Evidence.** At 10k × 384-d (unquantized, M=16, efConstruction=200), the same deterministic graph:

| Budget | Build | Exact p50 | ANN `efSearch` 100 | `efSearch` 400 | Recall@10 at 100 / 400 |
|---|---:|---:|---:|---:|---:|
| 8 MiB (WASM/mobile default; what CI gates) | 74.2 s | 3.84 ms | 6.60 ms | 19.4 ms | 63.7% / 96.5% |
| 64 MiB (native default; not gated) | 28.8 s | 1.64 ms | 0.89 ms | 2.49 ms | 63.7% / 96.5% |

At 2k, recall at `efSearch` 100 is 95.4%; at 10k, 63.7%. The 2k build profile is ~78% distance computation (`dot_similarity` 44.5%, neighbour selection `select` 24.8%, `distance` 8.2%).

**What this means.** The internal roadmap's "the graph does not pay for itself" and "build cost is superlinear" are largely statements about the **8 MiB budget**: at the native default the graph beats exact search at `efSearch` ≤ 200 and builds 2.6× faster. On phones and in the browser it does not. The recall decline with collection size is independent of budget.

**Next measurements.** HNSW-1/3 at both budgets with the storage-read counter (plan §2.3), node-cache hit rate by layer, HNSW-5 crossover at 20k/50k at 64 MiB, and the same at 8 MiB with binary quantization (which fits: 5.4 MB retained, but recall 47% at `efSearch` 100). Any change to graph construction changes stored neighbours and recall — no format change, but a behaviour change that needs a decision.

### R2. Memory envelope

**Evidence.** 10k × 8 KiB documents on the in-memory backend: 422 MiB high-water after loading 82 MB of documents, 501 MiB peak for index create/drop, 922 MiB for `update_many` with export, 423 MiB for `delete_many` (reproduces the stabilization review to within 0.1%). No backend sets a redb cache size, so all of them — file, in-memory and the browser's OPFS backend — use redb 4.2's 1 GiB default (`redb-4.2.0/src/db.rs:1435`). An unshipped 64 MiB cache experiment cut export-inclusive peaks by 19–28% (501→408, 922→668, 423→329 MiB) but raised latency 31–97% (stabilization review).

**Gap.** Browser memory is unmeasured (`measureUserAgentSpecificMemory` returned "unavailable" in CI) and no Android/iOS RSS exists for the engine. Measure MEM-1 equivalents in Chromium with cross-origin isolation and on the user's Android device via Notewell's `Probe` (never the Play build) before proposing any default.

## Not proposed

| Idea | Reason |
|---|---|
| Change durability defaults or batch commits | Changes guarantees; out of scope. BULK-6 measures the commit path so other work cannot hide a regression in it. |
| Persist BM25 document frequencies / change postings layout | Storage-format change. |
| f16 or other compressed cached vectors | Changes scores. |
| Change the redb cache default or vector cache budgets | Behaviour and memory trade-off; needs R2 evidence from real devices. |
| Reverse iteration on `ReadTxn` for descending sorted pages | New public trait method; would make item 3's descending case O(page) — revisit in a release that allows API additions. |
| Native SIMD dispatch (AVX2) | `ROADMAP-INTERNAL.md` lists it; the x86 kernel already vectorises on SSE2, item 2 halves cosine cost first, and the benefit has not been measured. |

## Notes on the internal roadmap

`ROADMAP-INTERNAL.md` §2 predates the shared search-cache and filtered-vector work. Two of its "open" items are stale against v0.12.1 source: the node cache now has a shared, evicting budget across graphs (`search_cache.rs`), and filtered vector search collects bounded ID sets (`bounded_vector_ids_in`) rather than materialising matching documents. Its "Where things stand" table should be re-measured at both budgets (R1) before it is used to justify work.
