# v0.13.0 Stage 0 — measurement foundation: implementation report

Report date: 2026-10-24. Baseline: **v0.12.1**, commit `a2ef46602127919c60a0fe6333d676b1b1ab391e`.
Branch: `feat/v013-stage0-benchmarks` at `565d94a` (pushed, not merged; `dev` and `main` untouched). Commits: `9d96682` foundation, `d931950` absolute evidence paths and `--pin`, `32e51ed` calibrated Criterion tolerances and Tier 0 references, `ab06759` calibrated vector floors, `f28238a` per-case Criterion floors, `37508b4` keep comparing after a failure, `2965eaa` run provenance in references, `925160a` baseline manifest and references, `565d94a` evidence asset in the manifest.
Companions: [V013_STAGE0_GATE_RESULTS.json](V013_STAGE0_GATE_RESULTS.json) (machine-readable results), [V013_STAGE0_MERGE_READINESS.md](V013_STAGE0_MERGE_READINESS.md), the updated [V013_BENCHMARK_PLAN.md](V013_BENCHMARK_PLAN.md) (§7 has the calibration results). Raw evidence: [`artifacts/v013-stage0/`](artifacts/v013-stage0/), and the v0.12.1 GitHub release asset `taladb-v0.12.1-baseline-evidence.tar.gz` (10.4 MB, SHA-256 `0a14198979f17a36585c43bacbad0b26c4a5a49424c20a8e2f8ca7f505857c64`; the copy of this report inside it predates this line).

## 1. Summary

Stage 0 is implemented and its gates were exercised against v0.12.1 on both CI runner types:

- **They accept unchanged performance.** 6 Criterion A/A jobs (3 per runner type), 3 vector + browser A/A runs and a local A/A were used for calibration. Calibration was then checked on data it had not seen: a release-size run comparing the candidate with itself *and* with a2ef466, a fresh PR-size A/A, and a local A/A. Every one passes: 0 failures across 88 Criterion cases × 2 runners × 2 gates, 32 vector and browser comparisons at release size (28 + 4) and 16 at PR size (14 + 2).
- **They detect meaningful regressions.** Five positive controls each fail the gate they target, and no other: a sorted-page slowdown (+14% at 10k, +75–100% at 1k), a cosine-kernel slowdown, a BM25 parameter change, two extra allocations per updated document, and the 2026-09-20 fused kernel. The timing controls failed on both runners and under both the PR and the cumulative gate, after confirmation.
- **They preserve deterministic correctness.** Result fingerprints of all 77 Criterion workloads are bit-identical across a local x86_64 machine, CI x86_64 and CI arm64, in dev and release builds, and between a2ef466 and the candidate. A changed result fails before any timing is compared.
- **They produce reproducible evidence against v0.12.1.** `benchmarks/baselines/v0.12.1/` holds the manifest (commit, lightweight-tag status, `Cargo.lock` SHA-256, toolchain, harness commit and trees), compact reference results built from a2ef466 itself on both runners, the Tier 0 references and `SHA256SUMS`. Every reference names its CI run, attempt and artifact. Full raw evidence is attached to the v0.12.1 release.

The positive controls found **two gates that could not fire**, and both were fixed before validation. First, the per-request exact-search gate at PR size: a fixed 0.25 ms floor against a 0.26 ms search. Second, every `vector/score_one` kernel: a fixed 2 µs floor against 16–500 ns measurements. Without the controls, both would have shipped as green gates that could never fail.

No engine code changed: `git diff a2ef466 -- packages/core/src packages/bindings packages/core/Cargo.toml Cargo.toml Cargo.lock` is empty on the branch.

## 2. What was implemented

### 2.1 A — stabilised CI gates

| Requirement | Implementation |
|---|---|
| ≥ 3 independent, interleaved repetitions on every event | `vector-bench-ci.py` builds both sides first, then runs B C B C B C. `performance.yml` uses `--repeats 3` for PR, push, schedule and release (was 1 for push/schedule/release). The browser loop runs 3 pairs on every event |
| No gating of noisy cold-query cases on single runs | Cold filtered cases are report-only below 5 pairs. The paired rule (⌈2n/3⌉ pair ratios above 1 + T/2) means no case can fail on one run |
| Targeted confirmation | A gated suspect is re-measured with 3 more interleaved pairs, for the suspect cases only (Criterion) or the failing step (examples), and fails only if all 6 pairs still meet the rule. The browser step does the same per budget |
| HNSW at 8 MiB and 64 MiB | `--cache-bytes` pass-through; separate 8 MiB and 64 MiB steps; floors calibrated per budget |
| Preserve every attempt and its metadata | Result JSON schema 2: both sides' commit, binary SHA-256, `Cargo.lock` SHA-256 and build time; `rustc -vV`, CPU model, governor, load before/after, runner image and run attempt; the protocol; every per-run value of every attempt; fingerprints. An `-evidence/` directory holds the raw per-run output (Criterion `sample.json`, example JSON). Artifact names include the base kind and run attempt |

Also: a comparison now continues after an earlier one fails, so a PR sees every regressed configuration at once.

### 2.2 B — benchmark coverage

All inside the existing harnesses (Criterion `benches/engine.rs` and `examples/*_profile.rs`); no new framework.

| Family | New workloads (plan IDs) | Notes |
|---|---|---|
| DOC | `doc/find_by_id` (DOC-1), `indexed_eq` (2), `compound_eq_range` (7), `count_indexed_eq`/`count_indexed_range` (8), `aggregate_group` (9), `page_full`/`page_projected` (10), `range_window` (11), `sorted_page_asc` (12), `count_unindexed` (13) at 1k/10k, 100k in release-size runs | Stabilization fixture plus a `(bucket, n)` compound index |
| FTS | `fts/bm25_zipf/10000` (FTS-3), `fts/insert_indexed/10000` (FTS-5) | Deterministic Zipf vocabulary, ~5,000 terms, s = 1.1 |
| HYB | `hyb/hybrid_unfiltered` (HYB-2) | |
| BULK | `bulk/insert_many_indexed` (BULK-2), `update_one_by_id`/`delete_one_by_id` (BULK-5) | |
| IDX | `idx/insert_amplification/{none,scalar1,scalar3,scalar3_fts,scalar3_fts_vector}` (IDX-2), release-size runs | IDX-1 is the existing `stabilization/index_cycle` |
| Fingerprints | Every Criterion case, through one declaration (`benches/support/suite.rs`) | IDs + order, or ID sets where order is undefined, plus `f32` score bits; FNV-1a over a canonical encoding |
| Concurrency | `vector_memory_profile --steady N --mode ann\|exact` (CONC-2): warmed threads, per-thread CPU time from `/proc/thread-self/schedstat`, throughput, CPU ms per request, cores busy; A/B in CI, report only | |
| Memory | Counting global allocator in `stabilization_memory` (MEM-2), gated at +2% in Tier 0; `hnsw_profile` counts storage point reads per query (node-cache miss proxy) | |
| Harness vs v0.12.1 | Every comparison builds the baseline with the candidate's harness; the cumulative jobs build a2ef466 itself | Passed on both runners (V1) |

Existing case IDs and fixtures are unchanged, so the audit's v0.12.1 numbers still map 1:1. `TALADB_BENCH_GROUPS` and `TALADB_BENCH_SIZES` select families and sizes, and fixtures are seeded only for selected families.

### 2.3 C — calibration

- Criterion: per case and runner type, `T = max(5%, 3N)` and `A = max(3 × the largest A/A |median(C) − median(B)|, 2% of B)`. Cases are report-only above T = 25%, or for 1k cases above 10%. Data: 3 A/A jobs per runner type × (1k/10k + 100k).
- Per-request vector gates: the historical relative limits stay. The A/A data support them everywhere except unquantized 10k ANN at 64 MiB, which depends on confirmation. Absolute floors are calibrated per step configuration (rule, size, quantization, sync/staged, budget).
- Results, by runner: plan §7 and the JSON.

### 2.4 D — cumulative regressions

- `performance.yml` resolves its baselines in a `plan` job: the PR base (or parent), plus **a2ef466 by SHA** (`V0121_SHA`; the v0.12.1 tag is lightweight) on the weekly schedule, on every release tag and on request. When the parent already is a2ef466, one comparison serves as both.
- Release-relevant cases are gated: `release.yml` calls the whole workflow, including Criterion and the cumulative comparison, before any publish job.
- Exceptions: `benchmarks/gates/exceptions.json`. Each entry needs `reason`, `approved_by` and `approved_in`, covers one case up to a `max_ratio`, and applies to `pr`, `cumulative` or `any`. Applied exceptions are listed in the step summary and the result JSON. The approval itself is a review of that file. This needs a code-owner rule, which is a repository setting I did not change (see merge readiness).

### 2.5 E — evidence

- `benchmarks/baselines/v0.12.1/`: `manifest.json`, 20 compact reference files (Criterion at 1k/10k and 100k, HNSW in 6 configurations, filtered, concurrency; × 2 runners), Tier 0 references, `SHA256SUMS`.
- `release.yml` attaches every benchmark artifact of the release run (all attempts) as `taladb-<tag>-benchmark-evidence.tar.gz` plus `.sha256`.
- `scripts/bench-baseline.py` regenerates a baseline directory from gate results. It refuses an uncommitted harness, and accepts a side built from another commit only when its engine sources are identical (checked with git).

### 2.6 Tier 0 (deterministic, no baseline build)

`scripts/bench-tier0.py`, called from `ci.yml` through the reusable `benchmark-tier0.yml` on x86_64 and arm64, and required by `ci-pass`. It runs when code changes, or when only the Tier 0 script or `benchmarks/` change.

| Check | Rule | v0.12.1 reference |
|---|---|---|
| Fingerprints | equal for every case | 77 cases, identical on both runners |
| MEM-2 allocations | ≤ +2% per workload metric | index/update/delete at 1k docs; run-to-run variation ≤ 0.03% |
| VEC-6 codegen | vector instructions per kernel must not fall | x86_64 41/48/115, aarch64 (Linux, iOS) 10/12/20, wasm32 34/40/74 for `dot_similarity`/`euclidean_similarity`/`score_with_query_norm` |

VEC-6 is a check on vectorisation, not on speed. The timed `vector/score_one` cases on both runners measure kernel performance; codegen counts do not substitute for them.

## 3. Positive controls

Every control is a single commit on a throwaway `bench/stage0-control-*` branch on top of the calibrated commit, never merged.

| Control | Change | Gate | Result |
|---|---|---|---|
| Sorted page | 400k `black_box` iterations per call in `index_ordered_entries` (~100 µs) | Criterion, PR **and** cumulative, both runners (run 40104297780) | **Fails exactly** `stabilization/sorted_page/{1000,10000}` and `doc/sorted_page_asc/{1000,10000}` on arm64; the same on x86_64 except `sorted_page/1000`, report-only there by calibration. 10k: +12–16% against T = 5–10%. 6/6 pairs on every failure; the other 68 of the 72 PR-size cases neutral on all four jobs |
| Cosine kernel | two redundant `norm_sq` passes in `score_with_query_norm` (scores bit-identical) | Vector (C2b, run 40106202243) | **Fails** `exact_mean_ms` +51–58% on all 4 jobs, both runners, after confirmation. ANN unaffected, as expected: the graph path scores with `score_with_norms` |
| | | Criterion (C2, run 40104306699, before per-case floors) | Failed `exact_vector`, `find_nearest`. **Missed `score_one/Cosine` at 1.86–1.92×**: blind spot 2, fixed |
| | | Criterion (C2c, run 40108078013, per-case floors) | **Fails** every gated cosine-dependent case on both runners, nothing else (§3.1) |
| BM25 parameters | `k1` 1.2 → 1.25 | Tier 0 fingerprints, both runners (run 40104315455) | **Fails** `fts/bm25_zipf/10000` only |
| Allocations | two extra allocations per updated document | Tier 0 MEM-2, both runners (run 40104324270) | **Fails** `update mutation_allocations` +2.8% |
| Fused kernel | the 2026-09-20 `dot_and_norm_sq` | Tier 0 VEC-6 (run 40104333480) | **Fails** `score_with_query_norm`: aarch64 Linux 20 → 0, iOS 20 → 0, wasm32 74 → 0, x86_64 115 → 95 |

### 3.1 C2c — fresh cosine control under per-case floors

Run 40108078013, cosine control on top of `f28238a` (per-case floors), PR size. The Criterion gate fails, after confirmation (3 + 3 pairs), **exactly the cosine-dependent cases and nothing else**:

| Runner | Failed (gated) | Regressed but report-only by calibration | Everything else |
|---|---|---|---|
| arm64 | `vector/score_one/Cosine/{128,384,768,1536}` (1.86–1.92×), `stabilization/exact_vector/{1000,10000}`, `vector/find_nearest/1000` | `vector/find_nearest/10000` | neutral; `Dot`/`Euclidean` kernels and `hybrid` unchanged; fingerprints equal |
| x86_64 | `vector/score_one/Cosine/{384,768,1536}`, `stabilization/exact_vector/10000`, `vector/find_nearest/10000` | `score_one/Cosine/128`, `exact_vector/1000`, `find_nearest/1000` | neutral; fingerprints equal |

The same run's vector jobs fail `exact_mean_ms` on all four after confirmation. They show only the 8 MiB step because this control branch predates `37508b4`. Locally (i5-9400F), the same control fails `exact_vector` and `find_nearest/10000` with 6/6 pairs. There, `score_one/Cosine` is flagged but unconfirmed, because that run chose its confirmation suspects before the per-case floors existed.

## 4. Defects found while validating, and fixed

| # | Defect | Found by | Fix |
|---|---|---|---|
| 1 | 2k exact-search gate could not fire: floor 0.25 ms against a ~0.26 ms search, so any regression under +96% passed | Cosine control: +56% passed | Floors calibrated per step configuration (`ab06759`) |
| 2 | `vector/score_one` gate could not fire: floor 2 µs against 16–500 ns measurements | Cosine control: 1.9× passed | Per-case `A_ns` from A/A (`f28238a`) |
| 3 | A failing comparison skipped every later comparison in the job | Cosine control showed only its 8 MiB step | `!cancelled()` on later steps (`37508b4`) |
| 4 | Relative `--output` placed fingerprints and `CRITERION_HOME` inside the baseline checkout | First CI Criterion run (40089233914) | Absolute paths (`d931950`) |
| 5 | A target directory reused by another checkout could report "fresh" and return the previous checkout's binary: cargo artifact names ignore the workspace path | Local control built "in 0 s" | Target directory keyed by checkout path (`32e51ed`). CI was not affected, because fresh checkouts have newer mtimes |

## 5. Findings worth keeping

1. **Fingerprints depend on the C library, not the CPU.** On the same CPU, with the same source and toolchain, glibc 2.43 (Fedora) and 2.39 (Ubuntu 24.04) gave a BM25 control's top hit scores one ulp apart (`40bda3da` vs `40bda3db`), because idf uses `ln_1p`. The v0.12.1 fingerprints agree on both. Tier 0 references belong to the CI runner image; re-record them when it changes its C library.
2. **The stabilization FTS corpus cannot detect a BM25 change.** With every term once per equal-length document, `k1` cancels out of the score. Only the new Zipf workload caught the `k1` control. This confirms the plan's rationale for FTS-3.
3. **`$group` timing is bimodal per process.** `doc/aggregate_group/10000` has an A/A spread of T = 59–73% and is report-only on both runners. Its cost depends on the per-process `RandomState` seed of the group `HashMap`, so each run of the same binary draws a different collision pattern.
4. **x86_64 runners are about twice as noisy as arm64 within a job.** Median T is 11.3% vs 5.5%, and 56 vs 81 of 88 cases are gated. Most report-only x86 cases are 1k cases, 128-d kernels and the batched insert cases.
5. **Unquantized 10k ANN at 64 MiB is the noisiest per-request gate.** In one A/A attempt, arm64 hit +54% with 2/3 pairs, which a three-pair gate alone would have failed; pair 2 was slow across every `efSearch`. Confirmation cleared it (6-pair median 1.14). The largest A/A median ratio was 1.36 on x86_64. The 50% limit holds only with confirmation.
6. **Cargo's pre-link bitcode pipeline compiles the kernels scalar.** With `lto = true`, cargo compiles the `taladb` rlib for the linker and runs no vectoriser before LTO. Its assembly therefore shows scalar `fmul s`/`mulss` loops, even though the shipped (post-LTO) and non-LTO code is vectorised; this was checked through an exported wrapper on aarch64, which gave identical counts. VEC-6 counts the non-LTO rlib, which is also what crates.io users without LTO compile.
7. **Replays of the gate failures on record.** The v0.12.1 release attempt 1 (arm64 `exact moderate cold` +67%, one run) becomes a report-only note under Stage 0, so it would not have blocked the publish. The 2026-10-21 push failure (browser `all/ann/100`, one run) still fails as a single pair; Stage 0 never decides on one pair, and the three-pair PR comparison of the same change passes the new rule.

## 6. CI runtime and maintenance cost

Measured on the GitHub runners in these runs (minutes).

| Workflow, event | Before Stage 0 (v0.12.1 release run, PR #98) | After |
|---|---|---|
| PR: longest job (wall) | 9.1 (browser) | **~40.5** (Criterion, both runners in parallel) |
| PR: runner-minutes | ~24.5 | ~4 × 5 (vector) + 8 (browser) + 2 × 40.5 (Criterion) + 4 (Tier 0) ≈ **113** |
| Release / push / schedule: longest job | 11.1 | **~47** (Criterion incl. 100k step) |
| Release: runner-minutes, one baseline | ~35 | 2 × 7 + 2 × 27 (vector) + 11 (browser) + 2 × 47 (Criterion) ≈ **173** |
| Release with the cumulative comparison as a second baseline | — | ≈ **346** (wall still ~47 when jobs run in parallel) |
| Tier 0 in `ci.yml` (parallel to `rust-check`) | — | 2.8 (x86_64, warm cache; 3.7 cold), 1.2 (arm64) |

Plus up to one confirmation round per failing step (≤ +50% of that step), only when a step fails. The first v0.13.0 release has a2ef466 as its previous tag, so it pays for one baseline, not two. Artifacts are ~35 MB per release-size Criterion run and ~2 MB per vector run.

Cost drivers and options are in [V013_STAGE0_MERGE_READINESS.md](V013_STAGE0_MERGE_READINESS.md) §3. The main lever is dropping 1k sizes from PR Criterion runs: on x86_64, 19 of the 25 1k cases are report-only anyway (on arm64, 2).

Maintenance:

| Task | When | Effort |
|---|---|---|
| Recalibrate Criterion tolerances and vector floors | Runner image, toolchain pin, protocol or fixture change | 3 A/A dispatches per suite (~50 min wall), then `calibrate` / `calibrate --vector`; commit |
| Re-record Tier 0 references | Runner image changes its C library; an intended result or codegen change | One `suites=tier0, record_tier0=true` dispatch; review the diff |
| Keep harnesses v0.12.1-compatible | Every harness change during v0.13 | Review rule; the cumulative job fails to build otherwise |
| Exceptions | When a regression is accepted | One reviewed JSON entry |

## 7. Not done, and unverified

| Item | Status |
|---|---|
| macOS, Windows, iOS devices, Android devices, Safari, Firefox | **Not measured.** Gates run on Linux x86_64 and arm64 runners and Chromium only |
| WASM kernel *timing* | Not gated; only VEC-6 codegen and end-to-end browser search |
| Browser on arm64 | Not run (Chromium job is x86_64 only, as before) |
| Physical-disk durability (BULK-6), Node writes (BULK-7) in CI | Not implemented; Node bench remains manual, tmpfs caveat stands |
| HNSW-4 (maintenance), HNSW-5 (crossover), HNSW-7 (reopen), HYB-3, VEC-3, IDX-4, CONC-3/4/5, REC-1/3/4, MEM-4 | Not implemented (plan Tier 2/3; not in roadmap 0.4) |
| L-size (100k) for `stabilization/*`, BULK, FTS-5 | Not in CI; 100k covers the read-only `doc/` family only |
| CONC-2 | Measured A/B, report only (not gated) |
| Exception approval enforcement | Needs a CODEOWNERS / branch-protection rule; not configured |
| Plan target "Tier 0 < 2 min" | Not met: 2.8–3.7 min on x86_64 (codegen for four targets), 1.2 min on arm64; it runs in parallel with `rust-check` |
| Local timing evidence | Local A/A passed and the local sorted-page control failed its 1k cases. Its 10k cases did not fail: I ran a container build on other cores during that run, and one baseline pair dropped to 0.73. The CI controls are the gating evidence |
| `docs/reports/` | Gitignored; these reports are local files, copied into the release asset |
