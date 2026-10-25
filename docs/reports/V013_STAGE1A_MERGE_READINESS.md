# v0.13.0 Stage 1A — merge readiness

Date: 2026-10-25. Branch: `feat/v013-index-range-bounds` → `dev` (PR opened; **not merged**). Details: [V013_STAGE1A_IMPLEMENTATION_REPORT.md](V013_STAGE1A_IMPLEMENTATION_REPORT.md), [V013_STAGE1A_BENCHMARK_RESULTS.json](V013_STAGE1A_BENCHMARK_RESULTS.json).

## 1. Verdict

**Correct and safe to merge; two of the performance targets are not met, for a measured reason.** You need to decide whether to accept the change with the targets as measured (§3).

| Requirement | Result |
|---|---|
| Result ID sets equal the full-scan oracle | Yes: 900 bound combinations × 12 document shapes (scalars and arrays), plus sorting, paging, counts, updates, deletes, encryption and concurrent writes. The mutation check proves the tests catch the array hazard |
| Sorted order preserved exactly | Yes: identical ID order for sorted pages with skip/limit |
| Unsorted order change documented | Yes: `docs/api/collection.md`, changelog `Unreleased` |
| Existing tests and benchmarks unchanged and passing | Yes: 780/780 workspace tests; no assertion or benchmark edited |
| No public API, ABI or storage-format change | Yes: only `query/executor.rs` and `query/index_filter.rs` (crate-private) change, plus a `planner.rs` comment |
| No change to ANN, FTS hashing, vector caching; no new features | Yes |
| DOC-11 ≥ 10× at 10k | **No**: 7.3–7.5× (x86_64), 5.8× (arm64) |
| DOC-11 ≥ 50× at 100k | **x86_64 yes** (53.6× vs `dev`, 58.7× vs v0.12.1); **arm64 no** (39×) |
| No significant regression on adjacent workloads | Yes: every gated case is neutral on both runners against `dev` and v0.12.1 |
| Allocations and peak memory | −81% / −97% bytes allocated and −71% / −96% peak live heap at 10k / 100k |
| PR-base and cumulative comparisons | Run 40160253524: both gates, both runners, all suites green |

## 2. Why the targets are missed

The optimised query costs 1.03–1.20× what fetching and decoding its 800 result documents costs (8 × `doc/page_full` on the same fixture). On that fixture, no plan returning the documents can exceed 6.2× (arm64) to 8.2× (x86_64) at 10k, or 41× at 100k on arm64.

The targets came from the audit's probe, whose documents are small. There the change gives **12.6× at 10k and 130× at 100k**, with a two-sided window costing the same as a one-sided one. I did not try to close the gap: faster document decoding is roadmap item 4 (Stage 2), and the instructions ruled out speculative changes.

## 3. Decision for you

1. **Accept as is** (recommended). The optimisation reaches the floor of its workload. Record DOC-11's acceptance as "at the result-decoding floor", with the measured factors per runner.
2. **Or** redefine the DOC-11 target against the floor, e.g. "within 1.25× of 8 × `doc/page_full`", which every runner meets (1.03–1.20). Add a small-document variant to the Criterion `doc/` group that reproduces the audit probe's fixture. That changes the benchmark harness and needs recalibrated tolerances for the new case, so it belongs in its own small change, not in this PR.

## 4. Before merging

- The PR's own CI (PR-size gates against `dev`) must be green, including `ci-pass`.
- Read the behaviour note: unsorted two-sided ranges on scalar fields now return index-key order, so an unsorted `limit` selects a different, stable subset. Apps that rely on unsorted order should add a sort. They never had a guarantee, and the old order differed between runs.
- Release notes must not claim mobile or browser gains for this change: measured on Linux x86_64 and arm64 runners and one local x86_64 machine only.

## 5. Not started

Stage 1B (roadmap items 2 and 3) waits for your review and approval of this stage.
