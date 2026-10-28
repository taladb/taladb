# v0.13.0 Stage 2C — merge readiness

Engine change: PR #119 (`feat/v013-partial-vector-retention`: `cdfced2` engine and tests, `ebdee16` harness). Reports and changelog: the follow-up PR on `docs/v013-stage2c-reports`. Details: [V013_STAGE2C_IMPLEMENTATION_REPORT.md](V013_STAGE2C_IMPLEMENTATION_REPORT.md). Data: [V013_STAGE2C_BENCHMARK_RESULTS.json](V013_STAGE2C_BENCHMARK_RESULTS.json).

## 1. State

**#119 is already merged.** It was merged into `dev` as `dcf1fa6` at 06:10 UTC on 2026-10-28, 44 seconds after it was opened, once CI had passed and while both performance runs were still going. All checks have since finished:

| Check | Run | Result |
|---|---|---|
| CI (pull request): fmt, clippy, workspace tests, wasm32, MSRV, Tier 0 (fingerprints, VEC-6, MEM-2), audit, Miri, fuzz | 40642203151 | success |
| Vector performance (pull request, PR size) | 40642203156 | success |
| Vector performance (dispatch: release size, vector + Criterion + browser + Tier 0, against `dev` 92b402c and v0.12.1 a2ef466) | 40642210555 | **success, 17 of 17 jobs**, no failures and no warnings |
| x86 Criterion against v0.12.1, re-run until it ran on an EPYC 7763 (§1.1) | 40642210555, attempt 2 | success; ran on an EPYC 7763; `count_indexed_eq` neutral (0.986 / 0.988) |

Every gate passed. I found no correctness problem. Stage 3 has not been started.

### 1.1 x86 Criterion: `doc/count_indexed_eq`

| Comparison | CPU | `/10000` | `/100000` |
|---|---|---|---|
| Candidate vs `dev` (PR base) | **AMD EPYC 7763** (Zen 3) | 0.987, neutral | 1.014, neutral |
| Candidate vs v0.12.1 | AMD EPYC 9V45 | 0.991, neutral | 0.989, neutral |
| Candidate vs v0.12.1 | **AMD EPYC 7763** | **0.986, neutral** | **0.988, neutral** |
| Candidate vs `dev`, arm64 | | 0.997 | 1.004 |
| Candidate vs v0.12.1, arm64 | | 1.010 | 1.008 |
| Executed instructions (callgrind, 2,000 counts) | local | `dev` 431,016,278 · candidate 431,016,861 · v0.12.1 430,398,676 | |

This change does not touch the count path and executes the same instructions as `dev`. The closure that Stage 2B's controls identified (`count_plan_entries::{closure#0}`) moved by +0x1450 in the CI-equivalent bench build, from 16 to 32 mod 64. That is a layout change only. On the EPYC 7763 the case is neutral against both `dev` and v0.12.1, so Stage 2B's Zen 3 regression against v0.12.1 does not appear at this layout. The sensitivity Stage 2B confirmed is unchanged, and later changes can move the closure again, in either direction.

As the brief requires, I added no padding and changed no alignment, left the Criterion tolerances alone, proposed no exception in this PR, and left Stage 2B's root-cause evidence and suggested exception text as they are. The release-exception decision belongs to the frozen v0.13.0 candidate.

## 2. Checklist

| Requirement | Status | Evidence |
|---|---|---|
| Audit of every `RetainedVectors` consumer and of the stated assumptions | Done | Report §2.1 |
| Complete and partial blocks distinguished internally; `VectorBlock`/`CachedVectors` layout unchanged | Done | `RetainedVectors.partial` and `resume_after()` (crate-private); report §2.2 |
| Reserve before decoding; prefix sized to fit; deterministic prefix in key order; exact continuation boundary | Done | `reserve_partial`, `reserve_exact(capacity)`, boundary = last retained ID; report §3 |
| Existing generation and epoch checks reused; never a zero-vector prefix | Done | `insert_vectors` refuses empty prefixes; mutant M19 killed |
| Score the prefix, then stream the remainder; never double-score or omit; offer order preserved; cached norms; vectors-only fallback | Done | Proof in report §6.1; `distance_computations == count` and exact streamed counts in every test; mutants M1–M6, M8, M9, M21 killed |
| No collection-sized map or full intermediate block | Done | Report §3 |
| Malformed records, skipped entries and the exact boundary verified | Done | `malformed_records_and_foreign_keys_around_the_boundary_are_skipped_once`, `every_prefix_boundary_ranks_the_whole_table_exactly_once` |
| Unfiltered, dense filtered and sparse filtered paths; ANN rescoring | Done | Report §4; ANN race test with candidates on both sides; M20 killed |
| Graph-aware admission; no default budget change; no new cache; all memory accounted; no lock while scoring; no stale reuse; LRU changed only for prefixes (justified); pinned memory accounted; no added graph cold starts | Done | Report §5; mixed and ANN-only traffic equal to `dev` in graph reads, recall, fallbacks, latency (§5.1); unit tests and mutants M10–M17 |
| Full-scan oracle; bit-identical results across metrics, dims 3–1536, edge values, ties, filters, grouping, offsets, thresholds, pagination, boundaries, malformed records, writes, multiple collections and fields, reopen, graph rebuild, concurrency, pressure, budgets 0/1/4/8/16/64 MiB | Done | 17 integration tests (report §6.2) |
| Deterministic interleavings for writes during a partial-cache query | Done | Three interleaving tests (write during a hit, write during a build, eviction during a hit) |
| Mutation that treats a partial cache as complete is detected | Done | M1: 15 of 17 tests fail |
| Stage 1B and 2B tests preserved | Done | Unchanged and passing |
| Workspace tests, fmt, clippy, MSRV 1.90, wasm32 builds and tests | Done | Local gates; CI 40642203151 |
| Native x86_64 and arm64 performance CI, browser, Tier 0, MEM-2, MEM-3, Criterion A/B, cumulative | Done | Dispatch 40642210555, all pass |
| VEC-2 ≥ 25% faster at 8 MiB | **Met** | Criterion −44.5% (x86 EPYC 7763), −38.3% (arm64); gated exact −38% to −53%; local −40.8% |
| Improvement shown to come from reduced decoding | Done | Decodes −53.5%, instructions −36.6% (report §7.4) |
| CPU model recorded per x86 runner; layout vs work separated for `count_indexed_eq` | Done | §1.1 |
| No public API, C ABI, format, encoding, graph construction, ANN selection, score, budget or durability change | Done | Diff limited to `collection.rs`, `search_cache.rs`, `vector.rs`, a comment in `vector_api.rs`, one test file, one bench case and one example |

## 3. For the maintainer to decide

1. **The graph-first policy.** A prefix is never built while any graph is resident, and every graph walk reclaims it. This is what keeps ANN and mixed traffic exactly at `dev`'s behaviour, but it has costs:
   - an exact-only field in a database that also uses HNSW on another field gets no prefix once that graph is resident;
   - ANN rescoring almost never sees a prefix (only in the race tested in §6.2).

   The alternative would let a prefix stay beside a graph within some share of the budget. That is the crowding Stage 2B measured (1.4–3.7× slower ANN), so I did not do it. If you want prefixes for exact-only fields beside graphs, it needs its own measurement on real mixed databases.
2. **The minimum share (1/8 of the table).** At 8 MiB, 10k × 384-d qualifies (54%), but 50k × 384-d does not (5,349 of 50,000 vectors fit, below 6,250), so it streams as before. A lower minimum would help such tables a little (each retained vector saves about 0.4 µs per query). It would also let a small prefix occupy memory that a later, larger one cannot use, because prefixes are not grown in place. I kept 1/8 as a conservative default; it is one constant (`PARTIAL_BLOCK_MIN_SHARE`).
3. **First-query cost.** Building a prefix costs +8% to +21% on that query for prefixes up to 16 MiB, and +80% for a 67 MB prefix (50k × 384-d at 64 MiB), where fresh pages are faulted in. Per byte this is what `dev` already pays to build full blocks; the next one or two queries recover it. Builds happen once per revision, and again after a budget change or eviction.
4. **The Stage 2B exception question.** At this layout the release gate's comparison against v0.12.1 passes on Zen 3 (0.986 at 10k, 0.988 at 100k), so the current `dev` needs no exception for `doc/count_indexed_eq`. The placement sensitivity remains, and later changes can move the closure back. I suggest deciding on the frozen v0.13.0 candidate, as the brief says, using Stage 2B's suggested entry only if the regression reappears there.

## 4. Limitations

- **Local timings are Intel x86_64 only.** arm64, Zen 3 and browser numbers come from CI.
- **Profiles.** This sandbox allows neither call graphs nor hardware counters, so attribution uses callgrind instruction counts and decode counters.
- **Unmeasured platforms.** macOS, Windows, iOS and Android are not measured. The browser suite covers a prefix only at 1 MiB (5k × 128-d).
- **Not done here.** No in-place growth of a prefix, and no shared read-only graph node cache (Stage 2B's four-thread finding); both are outside this stage.
