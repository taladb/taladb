# Benchmark gates

TalaDB's performance gates compare a candidate commit with a baseline commit **on the same runner, in the same job**, using the candidate's harness on both sides. Stored numbers are evidence for people. A gate never compares against a number measured on a different machine.

| Path | What it is |
|---|---|
| `gates/criterion-tolerances.json` | Per-case tolerances for the Criterion gate, per CI runner type, derived from A/A runs |
| `gates/exceptions.json` | Approved, bounded exceptions to a gate (empty unless a regression was accepted) |
| `baselines/v0.12.1/` | The v0.13 baseline: manifest (commit, toolchain, environment), compact reference results, Tier 0 references, checksums |

The harnesses themselves live where they always have: `packages/core/benches/` (Criterion), `packages/core/examples/*_profile.rs`, and `scripts/bench-web*` (browser). The drivers are `scripts/vector-bench-ci.py` (A/B timing) and `scripts/bench-tier0.py` (deterministic checks).

## Tiers

| Tier | Runs | Checks | Blocks |
|---|---|---|---|
| 0 | `ci.yml` → `benchmark-tier0.yml`, x86_64 and arm64, every PR touching code | Result fingerprints of every Criterion workload; MEM-2 allocation counts (+2%); VEC-6 vector instructions per scoring kernel on x86_64, aarch64 Linux/iOS and wasm32 (must not fall) | Yes (`ci-pass`) |
| 1 | `performance.yml`, PRs touching the engine | Criterion A/B at 1k and 10k; HNSW at 8 MiB and 64 MiB budgets; resumable builds; filtered search; browser worker + OPFS, all at PR sizes | Yes |
| 2 | `performance.yml` on push to `main`, the weekly schedule and every release | Tier 1 at release sizes (10k vectors, 5k browser, 100k documents for read-only families) plus IDX write amplification and CONC-2 concurrency (report only). On schedule and release, also the **cumulative comparison against v0.12.1** | Release: yes. Schedule: a red run |

Tier 0 compares against `baselines/v0.12.1/`. Tiers 1 and 2 build the baseline commit next to the candidate.

## Workloads

Workload IDs come from `docs/reports/V013_BENCHMARK_PLAN.md`, which is local to the maintainers. Criterion ids are what the gate reports.

| Family | Criterion ids | Plan IDs |
|---|---|---|
| `vector/` | `score_one/{Cosine,Dot,Euclidean}/{128,384,768,1536}`, `find_nearest/{1000,10000}` | VEC-1, VEC-2 |
| `query/` | `range_scan_unindexed`, `range_scan_mixed_numeric`, `equality` (unindexed) | DOC-5 |
| `write/` | `insert_many_1k`, `export_snapshot_10k` | BULK-1, REC-2 |
| `stabilization/` | `indexed_page`, `sorted_page`, `unindexed_selective`, `bm25`, `bm25_selective`, `update_many`, `index_cycle`, `fts_index_cycle`, `delete_many`, `exact_vector`, `hybrid`, `hnsw` at 1k and 10k | DOC-3/4/6, FTS-1/2/4, BULK-3/4, IDX-1, HYB-1 |
| `doc/` | `find_by_id`, `indexed_eq`, `compound_eq_range`, `count_indexed_eq`, `count_indexed_range`, `aggregate_group`, `page_full`, `page_projected`, `range_window`, `sorted_page_asc`, `count_unindexed` at 1k, 10k (100k in Tier 2) | DOC-1/2/7/8/9/10/11/12/13 |
| `fts/` | `bm25_zipf/10000` (Zipf vocabulary), `insert_indexed/10000` | FTS-3, FTS-5 |
| `hyb/` | `hybrid_unfiltered/{1000,10000}` | HYB-2 |
| `bulk/` | `insert_many_indexed`, `update_one_by_id`, `delete_one_by_id` at 10k | BULK-2, BULK-5 |
| `idx/` | `insert_amplification/{none,scalar1,scalar3,scalar3_fts,scalar3_fts_vector}` (Tier 2) | IDX-2 |

Per-request harnesses: `hnsw_profile` (HNSW-1/2/3, with storage point reads per query as a node-cache miss proxy), `vector_filter_profile` (VEC-4, HNSW-6), `vector_memory_profile` (VEC-5, CONC-1; `--steady` is CONC-2), `stabilization_memory` (MEM-1, MEM-2).

Every Criterion case is declared once (`benches/support/suite.rs`). `TALADB_BENCH_FINGERPRINTS=<file>` runs each case once and writes a fingerprint of its result instead of timing it. The fingerprint covers IDs and order, or the ID set where the engine defines no order, plus `f32` score bits. The A/B gate compares both sides' fingerprints before comparing time. `TALADB_BENCH_GROUPS=doc,fts` limits a run to some families, and `TALADB_BENCH_SIZES=1000` sets the collection sizes.

Every harness must compile against v0.12.1 (`a2ef466`) unchanged, because the cumulative gate builds it there. A harness that calls newer API breaks the baseline build. Reject it in review.

**Tier 0 references belong to the CI runner image.** Score bits pass through the C math library (BM25's idf uses `ln_1p`), and glibc versions differ in the last ulp for some inputs. With the same source, toolchain and CPU, a BM25 control produced identical IDs but a top score one ulp apart under glibc 2.43 (Fedora) and 2.39 (Ubuntu 24.04). The v0.12.1 fingerprints happen to agree on both, and on x86_64 and arm64. Record the references on CI, re-record them when the runner image changes its C library, and expect a local check on another distribution to disagree on some score bits. A/B comparisons are unaffected: both sides run on one machine.

## Decision rules

For each case, with per-run statistics *Bᵢ*, *Cᵢ* from interleaved pairs (B C B C B C), *B*, *C* their medians and *T* the case tolerance:

- **Regression** only when all three hold: *C/B > 1 + T*; *C − B* above the absolute floor (Criterion: max(2 µs, 2% of *B*); per-request vector gates: 0.25 ms native, 0.5 ms browser, 500 ms for builds); and at least ⌈2n/3⌉ paired ratios *Cᵢ/Bᵢ* above *1 + T/2*. One bad run cannot fail a case.
- **Confirmation:** a suspected regression is re-measured with three more interleaved pairs, only for the failing cases. It fails only if the rule still holds over all six pairs. Both attempts are written to the result JSON and its evidence directory.
- **Hard rules, no tolerance:** fingerprints equal; recall@10 not more than 3 percentage points lower (any change is reported); cache accounting asserts; no repeated run of one binary may change its own results.
- **Cold-cache** filtered cases are reported, not gated, below five pairs.
- **Per-run statistic:** Criterion: the median of the sample means (`sample.json`, flat sampling, 30 samples, 1 s warm-up, 3 s measurement). Examples: their own p50/mean per run.

## Tolerances and recalibration

**Per-request vector gates** keep their relative limits (build and exact search +30%, ANN and filtered p50 +50%; the A/A runs stay inside a third of them, except unquantized 10k ANN at 64 MiB, which relies on confirmation). Their absolute floors are calibrated for each step configuration (rule, size, quantization, sync or staged, budget) as 3 × the largest A/A difference between the sides' medians, in `criterion-tolerances.json` under `vector`. One fixed 0.25 ms floor had made the 2k exact-search gate blind: exact search takes ~0.26 ms there, so a +56% regression passed. Recalibrate with `vector-bench-ci.py calibrate --vector <A/A vector-benchmark*.json…> --profile <profile>`. `vector-bench-ci.py reevaluate <result.json>` re-judges a stored result under the current tables without re-measuring it.

**Criterion:**

`T = max(5%, 3N)`, where *N* is the case's A/A noise: the largest deviation of one run's median from its side's median, over both sides of every A/A run. Cases are reported, not gated, when *T* exceeds 25%, or for 1k cases when it exceeds 10%. Uncalibrated cases use the default (10%).

Recalibrate whenever the runner image, toolchain pin, Criterion protocol or a fixture changes:

1. Dispatch **Vector performance** on the branch with `baseline` set to the branch's own commit and `suites` = `criterion`, at least three times. Each run is an A/A comparison: the same commit is built twice and interleaved.
2. Download the `criterion-benchmark-*` artifacts, then run `python3 scripts/vector-bench-ci.py calibrate <A/A JSONs…> --profile ci-linux-x86_64`, and the same with `ci-linux-aarch64` for the arm64 runs. For the vector floors, dispatch with `suites` = `vector` at both sizes and run `calibrate --vector`.
3. Validate the result: one more A/A run must pass, and a positive control must fail (below). Commit the tolerance file together with the A/A evidence summary.

## Positive controls

A gate that never fires cannot be told apart from one that cannot fire. Before trusting a new or recalibrated gate, run it against a deliberately slowed or broken candidate on a throwaway branch, and never merge that branch:

| Control | Change | Must fail |
|---|---|---|
| Sorted pages | fixed `black_box` spin in `index_ordered_entries` | `stabilization/sorted_page/*`, `doc/sorted_page_asc/*` only |
| Cosine kernel | redundant norm pass in `score_with_query_norm` (scores unchanged) | `vector/score_one/Cosine/*`, `vector/find_nearest/*`, HNSW exact |
| Fused kernel | the 2026-09-20 fused `dot_and_norm_sq` loop | VEC-6 on aarch64 and wasm32 |
| BM25 parameters | `k1` 1.2 → 1.25 | fingerprint of `fts/bm25_zipf` (the stabilization corpus cannot show it: with every term once per equal-length document, `k1` cancels out of the score) |
| Extra allocations | two allocations per updated document | MEM-2 `update` |

## Exceptions

A justified regression, for example a correctness fix that costs time, is accepted by adding an entry to `gates/exceptions.json` in the same PR:

```json
{ "case": "stabilization/update_many/10000", "gate": "pr", "platform": "ci-linux-x86_64",
  "max_ratio": 1.12, "reason": "…", "approved_by": "<reviewer>", "approved_in": "<PR URL>" }
```

`gate` is `pr`, `cumulative` (against v0.12.1) or `any`; `platform` is a tolerance profile or `*`. An exception covers the case only up to `max_ratio`, so a larger regression still fails. The gate refuses an entry without a reason or approver, and the step summary lists every exception it applied. The approval itself is a review of this file: protect `benchmarks/gates/` with a code-owner review rule so an author cannot approve their own exception. Every `cumulative` exception must also be named in the release's changelog.

## Evidence

- Every comparison writes a result JSON. It records each side's commit, binary SHA-256 and `Cargo.lock` SHA-256, plus `rustc -vV`, CPU model, governor, load average, runner image, the protocol, every per-run value of every attempt and the fingerprints. Next to it is an `-evidence/` directory with the raw per-run output (Criterion `sample.json` files, example JSON).
- Workflow artifacts keep these for 90 days. A release run also attaches all of them to the GitHub release as `taladb-<tag>-benchmark-evidence.tar.gz` with a `.sha256`, so they outlive the retention window.
- `baselines/v0.12.1/` holds the canonical baseline manifest and compact results in Git, with checksums (`SHA256SUMS`). The full evidence behind it is attached to the v0.12.1 release (`taladb-v0.12.1-baseline-evidence.tar.gz`).

## Running locally

```sh
git worktree add --detach ../taladb-baseline a2ef46602127919c60a0fe6333d676b1b1ab391e
python3 scripts/vector-bench-ci.py ../taladb-baseline . --workload criterion --criterion-groups doc --output out.json
python3 scripts/vector-bench-ci.py ../taladb-baseline . --workload hnsw --count 2000 --cache-bytes 67108864
python3 scripts/bench-tier0.py fingerprints        # and: allocations, codegen
```

Pin to one core (`taskset -c N`) and note the CPU governor. Local tolerances are not CI tolerances; pass `--tolerances-profile` to choose a table.
