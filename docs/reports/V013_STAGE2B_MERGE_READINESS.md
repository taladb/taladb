# v0.13.0 Stage 2B — merge readiness

Engine change: PR #116 (`feat/v013-ann-retained-rescore`: `6f90eeb` engine and tests, `f4efebc` harness). Reports and changelog: the follow-up PR on `docs/v013-stage2b-reports`. Details: [V013_STAGE2B_IMPLEMENTATION_REPORT.md](V013_STAGE2B_IMPLEMENTATION_REPORT.md). Data: [V013_STAGE2B_BENCHMARK_RESULTS.json](V013_STAGE2B_BENCHMARK_RESULTS.json).

## 1. State

**#116 is already merged.** It was merged into `dev` as `0d16416` at 13:20 UTC on 2026-10-26, six minutes after it was opened. At that point CI had passed, but both performance runs were still going. Every check has since finished:

| Check | Run | Result |
|---|---|---|
| CI (pull request): fmt, clippy, workspace tests, wasm32, MSRV, Tier 0 (fingerprints, VEC-6, MEM-2), audit, Miri, fuzz | 40328872773 | success |
| Vector performance (pull request, PR size) | 40328873104 | success |
| Vector performance (dispatch: release size, vector + Criterion + browser + Tier 0, against `dev` 2c0c170 and v0.12.1 a2ef466) | 40328873601 | **failure**: 15 of 17 jobs pass. Both x86 Criterion jobs fail on `doc/count_indexed_eq` (§1.1). |
| CI on `dev` after the merge (`0d16416`) | 40329800897 | success |

**Not every performance gate passed.** The one failure is on a code path this change does not touch, with identical executed instructions (§1.1). I found no correctness problem, and I do not recommend a revert. The failure stays open until it is either confirmed as code placement or explained some other way. Stage 2C has not been started.

## 1.1 Open item: x86 Criterion `doc/count_indexed_eq`

| Case (AMD EPYC 7763) | vs `dev` 2c0c170 | vs v0.12.1 |
|---|---|---|
| `doc/count_indexed_eq/10000` | +23.2%, 6/6 pairs over T/2 (T 12%) | +22.3%, 6/6 |
| `doc/count_indexed_eq/100000` | +32.4%, 6/6 (T 14%) | +34.1%, 6/6 |

arm64 is neutral on this case. Evidence that it is code placement, not work:

- **Path untouched.** The case runs `count_plan_entries`, a redb range scan with a counting closure, which this change does not modify.
- **Same work.** Under callgrind the same count executes the same number of instructions on both builds: 431,016,920 vs 431,016,166 in total; 36,358,894 in `Range::next` on both.
- **Same code, moved.** The hot functions are identical in size, shifted uniformly, and keep the same 64-byte alignment.
- **No change on Intel.** On the local i5-9400F both builds are equal within 5% (single rounds), with and without the JCC mitigation.
- **Base inside history.** The baseline's times match this case's x86 history from Stage 2A. Only the candidate is out of range.

What would confirm or refute it: one dispatch of `suites=criterion` comparing `dev` 2c0c170 against 2c0c170 plus a padding-only change (an unused but retained function in the same module). If `count_indexed_eq` moves the same way, the cause is placement. I did not push that branch, because pushed branches have been merged within minutes before. It needs your go-ahead, or you can run it yourself.

## 2. Checklist

| Requirement | Status | Evidence |
|---|---|---|
| Use only a block whose collection, field and revision match the snapshot | Done | `peek_vectors(key, rev)` takes an exact generation; key is `collection::field`; mutants M1/M2 killed |
| Reuse the existing `Arc<RetainedVectors>` loan | Done | Same `vector_loans` mechanism as exact search; M7 killed |
| Ordered-ID lookup; no second collection-sized map; no new block for ANN | Done | `binary_search` on `block.ids`; no allocation added |
| Stored norms reused with identical score bits | Done | `score_with_norms`; bit-identical parity test; M6 killed |
| Point-read path when no block; never drop a candidate absent from a block | Done | Per-candidate fallback; `a_candidate_absent_from_the_block_falls_back_to_its_point_read`; M3/M5 killed |
| No change to traversal, candidate order, `efSearch`, retries, recall, ranking | Done | Distances, recall and fingerprints identical on 128 sweeps (`dev`, candidate, v0.12.1) and in CI |
| Borrowed blocks valid until rescoring completes; no lock in the scoring loop; reservations respected | Done | One short lock; `a_block_evicted_mid_rescoring_stays_valid_and_accounted` (exact byte delta) |
| Eviction and pressure behaviour at 0/1/8/16/64 MiB; defaults unchanged | Done | `peek_vectors` does not touch recency (M8 killed); budget test; no default changed |
| Bit-identical scores and IDs across metrics, dims, quantization, hit/miss, options, filters, retries, writes, rebuilds, concurrency | Done | 11 integration tests + 1 unit test (report §4) |
| Write committed between graph traversal and rescoring | Done | Three deterministic interleavings plus a stress test with a body-vs-score oracle |
| `vector_score_and_body_share_a_snapshot_even_when_cache_is_warm` preserved | Done | Unchanged, passes |
| Mutation tests: generation, stale reuse, lookup, omission, norms, accounting | Done | 8 of 9 killed; M9 equivalent |
| Workspace tests, fmt, clippy, MSRV, wasm32 builds and tests | Done | Local gates and CI |
| Tier 0 fingerprints and SIMD codegen, MEM-2 | Done | CI Tier 0 (ci.yml and the dispatch), both architectures |
| Native vector CI at 8 and 64 MiB, browser, cumulative v0.12.1 | Done | Dispatch 40328873601, all pass (implementation report §6.6) |
| Criterion A/B | **Open** | arm64 pass; x86 fails on `doc/count_indexed_eq` (§1.1) |
| No public API, C ABI, format, cache-default, graph-construction or selection changes | Done | `peek_vectors` is `pub` inside the crate-private `search_cache` module; diff limited to `vector_api.rs`, `search_cache.rs`, tests, examples, the bench script and one report-only workflow step |

## 3. For the maintainer to decide

1. **The x86 Criterion failure (§1.1).** Accept it as code placement on the strength of the local evidence, or have the padding-only control run first. I recommend the control run before the v0.13.0 release gate, since the release runs this same Criterion comparison against v0.12.1 and may keep failing on it while this layout persists (any later change can move it again).
2. **Keep it, knowing hits are rare.** By default a block and a ready graph almost never coexist: only an unfiltered exact search builds a block, Auto mode then chooses ANN, and building or rebuilding the graph evicts the block. The change costs one mutex lock per walk when there is no block and has no measurable effect then. When a block is present, which happens with explicit `mode: Exact` alongside ANN (including the `recall` command) and after `memoryBudget` fallbacks, it saves 10–28% on unquantized graphs (13–21% on arm64 CI at 10k × 384). I recommend keeping it. Reverting is a two-file revert if you prefer less code for a rare case.
3. **The four-thread target needs a different change.** CPU per request at four threads is +45% to +122% over one thread (local and CI) and is not caused by rescoring. `CacheLease::take` lends the graph's node cache to one query at a time, so overlapping walks start cold and read roughly 80–95 graph nodes per request from storage, even at 64 MiB with every node cacheable (implementation report §6.4). Fixing that means letting concurrent walks share a read-only node cache, which changes the cache's ownership and accounting. It would need its own roadmap item, design and Risk M review. I did not attempt it.
4. **At 8 MiB the block can crowd out the graph.** At 10k × 128 a retained block leaves the graph too little room, and ANN is 1.4–3.7× slower until the block is evicted. This happens on `dev` and v0.12.1 alike. Changing the admission policy (for example, not retaining a block that would displace a graph in use) is a behaviour decision about cache policy, which is why I left it alone. Stage 2C (partial retention) touches the same admission code and should take this into account.
5. **CI changes.** Non-staged HNSW runs now add a report-only `--rescore` section, and CONC-2 has a second report-only run with `--retain`. Together they add a few minutes per vector job. `vector-bench-ci.py` now *fails* when the same candidates produce different result IDs or score bits (fingerprints keyed by insertion position, verified equal against v0.12.1), and *notes* a fingerprint change that comes with a candidate change, as recall changes are noted today. Remove either if you do not want them in the gate.

## 4. Limitations

- **x86 timings in CI are inconclusive.** The vs-`dev` x86 job ran on an EPYC 9V45 whose pair ratios spread from 0.66 to 1.51, so the arm64 numbers (tight pairs) and local runs carry the conclusions. Cumulative x86 on an EPYC 7763 shows consistent block wins at `efSearch` 400.
- **No call-graph profiles.** Local profiles are flat because this sandbox forbids call-graph sampling. ARM64 has no profiles, only CI timings and counts.
- **Unmeasured platforms.** macOS, Windows, iOS, Android and browser ANN with a retained block are not measured. The browser suite runs its default workloads, which never hold a block.
