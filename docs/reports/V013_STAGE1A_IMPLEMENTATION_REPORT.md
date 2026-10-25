# v0.13.0 Stage 1A — two-sided indexed ranges: implementation report

Date: 2026-10-25. Branch: `feat/v013-index-range-bounds`, from `dev` at `b905cd1`. Performance baseline: v0.12.1, `a2ef46602127919c60a0fe6333d676b1b1ab391e`.
Companions: [V013_STAGE1A_BENCHMARK_RESULTS.json](V013_STAGE1A_BENCHMARK_RESULTS.json), [V013_STAGE1A_MERGE_READINESS.md](V013_STAGE1A_MERGE_READINESS.md); roadmap item 1 in [V013_OPTIMIZATION_ROADMAP.md](V013_OPTIMIZATION_ROADMAP.md).

| Commit | Content |
|---|---|
| `cd48959` | The executor change, unit tests and `tests/two_sided_range.rs` |
| `4b3bb02` | `examples/range_memory_profile.rs` (heap measurement) |
| `b52a8d0` | `docs/api/collection.md` and the changelog: unsorted order |
| this commit | Stage 1A reports |

## 1. Summary

`find({ n: { $gte: x, $lt: y } })` on an indexed field no longer builds the ID sets of both open-ended halves of the collection. On a field whose index has never held an array, the executor walks only the key ranges both bounds share, in key order, and stops at a limit.

- **Results are unchanged.** Every combination of 15 lower and 15 upper bounds, inclusive and exclusive, returns the same ID set as a full scan. That holds across 12 document shapes, sorted pages, unsorted limits, counts, updates, deletes, encrypted fields and concurrent writes. All 780 existing workspace tests pass unchanged, and removing the safety check fails 6 of the new tests.
- **Memory.** For DOC-11, eight 100-document windows: bytes allocated fall 81% at 10k (11.3 → 2.1 MB) and 97% at 100k (82.2 → 2.1 MB). Peak live heap falls from 0.48 to 0.14 MB at 10k and from 3.98 to 0.14 MB at 100k. The query's memory no longer depends on the collection size.
- **Speed: DOC-11 at 100k is 54–59× faster on x86_64 and 39× on arm64; at 10k it is 7.3–7.5× on x86_64 and 5.8× on arm64.** That is measured on the CI runners against both `dev` and v0.12.1, and no other benchmark regressed. **The 10× target at 10k, and the 50× target at 100k on arm64, are not met.** The optimised query runs at 1.03–1.20× of the cost of fetching and decoding the documents it returns (§4.2). On this fixture, no plan that returns those documents can be faster than 6.2–8.2× at 10k or 41× at 100k on arm64. On the audit's own probe, where documents are small, the speedup is 12.6× at 10k and 130× at 100k.
- **No public API, ABI or storage-format change.** `plan_full`, `QueryPlan` and every public type are untouched. Only `executor.rs` and `index_filter.rs` change, plus a comment in `planner.rs`.

## 2. What changed

### 2.1 Cause

`planner::bounded_range_from_and` plans a lower and an upper bound on one indexed field as `IndexAnd { [lower range(s), upper range(s)] }`. The executor collected the ID set of each sub-plan, each covering about half the collection, and intersected them. That is O(N) time and two O(N) sets for an O(result) answer. The intersection exists for arrays: an array document has one key per element, and different elements can satisfy each bound (`[1, 20]` matches `≥ 5 AND < 10`).

### 2.2 Change

`index_filter::scalar_range_plan(plan, txn, collection)` returns a narrowed plan only when all of the following hold:

1. The plan is an `IndexAnd` of exactly two sub-plans.
2. Both sub-plans consist only of key ranges (`IndexRange`, or `IndexOr` of them) on the same field. Equality intersections, compound plans and mixed fields are excluded.
3. The field's array counter (`META_INDEX_ARRAYS_TABLE`), read **in the query's own read transaction**, is `Some(0)`. A field that has held an array, or one without a counter (written before counters existed), keeps the ID intersection.

Then each document has at most one key, so it is in both ID sets exactly when that key lies in both bounds' key ranges. That is the pairwise intersection computed by the existing `intersect_ranges`, which is **reused unchanged**; it already serves vector-filter resolution and covered-ID visits for scalar fields. `union_ranges` (also reused) makes the result disjoint and sorted. The Int/Float twin ranges that `lower_bound_plan`/`upper_bound_plan` emit for mixed numeric types are intersected like any other range, so cross-type matches are kept.

The executor uses the narrowed plan at all three `IndexAnd` sites:

| Site | Serves | Behaviour |
|---|---|---|
| `execute_into` | `find`, `find_one`, `find_with_options`, `count`, `update_*`, `delete_*`, `aggregate` with a leading `$match` | Streams each range through `fetch_index_range` with no dedup set; `limit` stops the walk |
| `collect_ulids` | ID collection inside `$or` and other unions | Collects the narrowed ranges |
| `visit_plan_documents` | Capped ID collection (vector filters) | Visits the narrowed ranges; before, it scanned the whole lower half |

### 2.3 Properties preserved

| Case | How |
|---|---|
| Arrays, including elements satisfying separate bounds | Counter ≠ 0 → ID intersection, as before |
| Encrypted fields | Their index entries are one stored value per document (counter 0), so the key-range equivalence holds; results equal the unindexed collection's (tested) |
| Mixed Int/Float values and bounds | Twin ranges intersected pairwise; up to one Int window and one Float window |
| NaN, ±0.0, ±∞, i64 limits | Key ranges are only narrowed, never widened; the post-filter (`Matcher`) still decides every match, unchanged |
| Missing and null values; strings, bytes, bool, objects | Same post-filter on a subset of the same keys |
| Empty and contradictory ranges | Empty pairwise intersections are dropped; an Int lower bound still admits a bounded Float window, which the post-filter rejects |
| Sorted queries, `skip`, `limit` | Sorting collects every candidate and orders by a total order (`cmp_rows`, ties on `_id`), so candidate order cannot matter |
| Concurrent writes | The counter and the index are read in one snapshot; a field gaining an array in a later commit is seen by later reads only |

### 2.4 Observable change (approved)

Unsorted results of an eligible query now arrive in **index-key order**: by value with integers before floats, then `_id`. Before, they arrived in `HashSet` order, which differed between processes. An unsorted `limit` therefore returns a different, now stable, subset.

`docs/api/collection.md` now states the actual unsorted order. It used to claim ULID insertion order, which only full scans provide. The changelog's `Unreleased` section records the change.

## 3. Correctness

`packages/core/tests/two_sided_range.rs` compares an indexed collection with an unindexed twin (a full scan of the same documents):

| Test | Coverage |
|---|---|
| `every_bound_combination_matches_a_full_scan_on_scalar_fields` | 15 lower × 15 upper bounds × {`Gte`/`Gt`} × {`Lt`/`Lte`} = 900 filters. Bounds: i64 min/max, ints, ±0.0, floats, NaN, ±∞, a string. Documents: −5…25, i64 limits, floats incl. −0.0/0.0/NaN/±∞/1e300, strings incl. NUL, empty string, bool, null, bytes, object, missing. `find` ID set (and no duplicates) and `count` |
| `arrays_whose_elements_satisfy_separate_bounds_are_still_found` | The same 900 filters with 7 array documents added, including `[1, 20]` for `≥ 5, < 10` |
| `bound_order_extra_predicates_and_repeated_bounds_match_a_full_scan` | Reversed bound order, extra indexed/unindexed predicates, repeated bounds, `$ne`, `$exists`, inside `$or` |
| `sorted_pages_are_identical_and_unsorted_limits_are_subsets` | Sorted asc/desc/multi-key with skip/limit: identical ID order; unsorted skip/limit: correct size, no duplicates, subset; `find_one` |
| `unsorted_results_come_back_in_index_key_order_on_scalar_fields` | Pins the new, documented order |
| `update_and_delete_affect_the_same_documents_as_a_full_scan` | `update_many` / `delete_many` counts and resulting state |
| `a_field_that_gains_an_array_falls_back_and_stays_correct` | Fallback once the counter becomes non-zero |
| `encrypted_fields_behave_as_without_the_index` | Field encryption on the indexed field |
| `concurrent_reads_during_writes_stay_consistent` | 3 readers during 600 inserts, including an array halfway: no duplicates, every document matches, final state equals the oracle |

Unit tests in `query::index_filter::tests` cover the eligibility decision:

- a scalar field gets bounded ranges only;
- mixed bounds keep both type blocks;
- contradictory bounds find nothing;
- array fields keep the ID intersection;
- equality intersections, one-sided ranges and different fields are left alone.

Results:
- **Workspace:** 56 suites, **780 passed**, 0 failed, 3 ignored (the same 3 as before). No existing assertion or benchmark was changed.
- **Lints:** clippy is clean (workspace, all targets, all features, plus wasm32); rustfmt is clean.
- **Mutation check:** removing the array-counter check fails 6 of the 9 integration tests (every one with array data), so the tests can detect the hazard the check prevents.

## 4. Performance

### 4.1 CI — run 40160253524 (release size, all suites)

Both sides of each comparison were built on the same runner. Each comparison used 3 interleaved pairs and needed no confirmation round. Criterion times are per iteration, eight windows of 100 documents.

| DOC-11 `doc/range_window` | x86_64 vs `dev` | x86_64 vs v0.12.1 | arm64 vs `dev` | arm64 vs v0.12.1 |
|---|---:|---:|---:|---:|
| 1k | 1.38 → 0.77 ms (1.8×) | 2.10 → 1.22 ms (1.7×) | 2.12 → 1.33 ms (1.6×) | 2.14 → 1.35 ms (1.6×) |
| 10k | 5.90 → 0.79 ms (**7.5×**) | 9.00 → 1.24 ms (**7.3×**) | 8.15 → 1.40 ms (**5.8×**) | 8.30 → 1.43 ms (**5.8×**) |
| 100k | 47.7 → 0.89 ms (**53.6×**) | 80.8 → 1.38 ms (**58.7×**) | 65.6 → 1.67 ms (**39.2×**) | 67.1 → 1.73 ms (**38.8×**) |

Absolute times differ between the two x86_64 jobs because they ran on different runner CPUs; each ratio is from one runner. On x86_64, `doc/range_window/100000` is report-only by calibration (T = 44%), but its improvement is far outside that noise.

Adjacent workloads: every case was neutral, on both runners, both gates and all sizes. That includes DOC-3 `stabilization/indexed_page`, DOC-6 `stabilization/sorted_page`, DOC-7 `doc/compound_eq_range` and DOC-12 `doc/sorted_page_asc`, all within 0.96–1.03×. **0 failures** across 8 Criterion results (77 + 11 cases each). All fingerprints were equal, including DOC-11's ID sets against v0.12.1. Vector (8 jobs), browser (2) and Tier 0 (2, including MEM-2 allocation counts and VEC-6) all pass.

### 4.2 Why 10× at 10k is not reached on DOC-11: the measured bottleneck

`doc/page_full` is a one-sided indexed page of 100 of the same fixture's documents. Eight of them cost what fetching and decoding the eight windows' 800 documents costs: each carries a 1 KiB payload, a short body and an 8-d vector.

| Runner | n | Candidate | 8 × page_full | Candidate / floor | Best any plan could do |
|---|---:|---:|---:|---:|---:|
| x86_64 (vs `dev`) | 10k | 0.787 ms | 0.721 ms | 1.09 | 8.2× |
| arm64 (vs `dev`) | 10k | 1.399 ms | 1.324 ms | 1.06 | 6.2× |
| x86_64 (vs `dev`) | 100k | 0.890 ms | 0.785 ms | 1.13 | 60.8× |
| arm64 (vs `dev`) | 100k | 1.672 ms | 1.577 ms | 1.06 | 41.6× |

The narrowed query has no headroom left: what remains is returning the documents.

The ceiling depends on the CPU: the decode-vs-scan cost ratio differs between machines, and GitHub assigns different x86_64 CPUs to different jobs. PR #103's own run (40163439155, PR size against `dev`) landed on an AMD EPYC 7763. There, DOC-11 at 10k came out at **5.94×**, with the candidate at **1.006×** of the floor and a ceiling of 5.98×. The dispatch run's x86_64 jobs used an AMD EPYC 9V45 (7.50×, ceiling 8.19×) and an Intel Xeon 8370C (7.26×, ceiling 8.11×). On every CPU measured, the candidate sits at the floor. The PR run's other results: 47 cases per runner, 46 neutral, 1 improvement (DOC-11), 0 failures; `ci-pass` green. The roadmap's targets came from the audit probe (`artifacts/v013-baseline/harness/probe-range.rs`), whose documents are an integer and a short title. Re-run unchanged (local i5-9400F, median of 3 p50s):

| Audit probe | `dev` | Candidate | Speedup |
|---|---:|---:|---:|
| two-sided window of 100, 10k | 1.084 ms | 0.086 ms | **12.6×** |
| two-sided window of 100, 100k | 11.96 ms | 0.092 ms | **130×** |
| one-sided, newest 100 (reference) | 0.081 / 0.084 ms | 0.081 / 0.085 ms | 1.0× |

There, a two-sided window costs the same as a one-sided range. Faster decoding is roadmap item 4 (Stage 2) and is out of scope here.

### 4.3 Local A/B (i5-9400F, pinned, `dev` vs candidate)

DOC-11 runs at 2.27 → 1.30 ms (1.75×) at 1k, 9.50 → 1.33 ms (7.1×) at 10k, and 82.8 → 1.53 ms (54.2×) at 100k. DOC-3/6/7/12 range from 0.98× to 1.03×. Two cases the change cannot affect (`aggregate_group/100000`, `indexed_eq/100000`) came out "improvement" at about 1.12×, which is local run-to-run drift.

## 5. Allocations and memory

`examples/range_memory_profile.rs` uses a counting allocator that tracks live bytes. It measures DOC-11's eight windows of 100 documents with a 1 KiB payload, on a local x86_64 machine, 3 runs per build. The figures are identical across runs.

| | v0.12.1 | `dev` | Candidate |
|---|---:|---:|---:|
| 10k: bytes allocated | 11.26 MB | 11.26 MB | **2.11 MB** (−81%) |
| 10k: peak live heap | 478 KB | 478 KB | **140 KB** (−71%) |
| 100k: bytes allocated | 82.19 MB | 82.19 MB | **2.11 MB** (−97%) |
| 100k: peak live heap | 3,979 KB | 3,979 KB | **140 KB** (−96%) |
| 100k: `count()` peak live heap | 3,703 KB | 3,703 KB | **121 KB** |
| Allocation count (8 windows) | 6,504 / 6,557 | 6,504 / 6,557 | 6,520 / 6,520 |

The allocation *count* barely moves: the two ID sets were a few large, growing allocations. Their *size* scaled with the collection, and now nothing does. Stage 0's MEM-2 allocation gate (index/update/delete) also passed on both runners in CI.

## 6. Not done, and unverified

| Item | Status |
|---|---|
| 10× at 10k on DOC-11; 50× at 100k on arm64 | **Not met**; bounded by result decoding (§4.2). Met on the audit probe |
| Memory measurement on arm64 | Not run: the example ran locally on x86_64 only. The allocation logic has no architecture-specific path |
| Unsorted two-sided ranges on fields that hold arrays | Unchanged (ID intersection), by design |
| `IndexAnd` with more than two bounds' worth of sub-plans | The planner emits exactly two for a two-sided range. Other shapes are untouched |
| macOS, Windows, devices, browser query timings | Not measured; the browser suite is vector-only |
| Evidence | CI run 40160253524 artifacts (90-day retention) and a local copy in `~/.cache/taladb-stage0/evidence/v013-stage1a/`. Not attached to a release: the v0.12.1 release holds the baseline and was not modified. Attaching to the v0.13.0 release is the natural home |
| Stage 1B | Not started |
