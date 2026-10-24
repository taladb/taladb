# v0.13.0 Stage 0 — merge readiness

Date: 2026-10-24. Branch: `feat/v013-stage0-benchmarks` at `565d94a` (pushed; **no PR opened; `dev` and `main` untouched**). Baseline: v0.12.1, `a2ef46602127919c60a0fe6333d676b1b1ab391e`.
Evidence: [V013_STAGE0_IMPLEMENTATION_REPORT.md](V013_STAGE0_IMPLEMENTATION_REPORT.md), [V013_STAGE0_GATE_RESULTS.json](V013_STAGE0_GATE_RESULTS.json), plan §7.

## 1. Verdict

**Ready to merge after two maintainer decisions (§3) and one repository setting (§4).** Stage 0's success criterion holds on both CI runner types:

| Criterion | Evidence | Met |
|---|---|---|
| Accepts unchanged performance | Fresh A/A validation runs (not used for calibration) pass every gate: Criterion 88 cases × 2 runners × PR + cumulative; 32 vector/browser comparisons at release size, 16 at PR size; Tier 0 on both runners; local A/A | Yes |
| Reliably detects meaningful regressions | Five positive controls fail exactly their target gates, on both runners and under both PR and cumulative gates where timed, after confirmation. Detection reaches +12–16% at 10k on sorted pages (T 5–10%) | Yes, within the calibrated tolerances. x86_64 gates 56 of 88 Criterion cases, arm64 81 of 88 |
| Preserves deterministic correctness | 77 fingerprints identical across machines, architectures, build profiles and a2ef466 vs candidate; a BM25 parameter change fails Tier 0 and the A/B gate | Yes |
| Reproducible evidence against v0.12.1 | `benchmarks/baselines/v0.12.1/` (manifest, 20 references built from a2ef466 itself, Tier 0 references, `SHA256SUMS`); raw evidence on the v0.12.1 release | Yes |
| No engine, API, ABI or format change | Empty diff against a2ef466 for `packages/core/src`, bindings, manifests and `Cargo.lock` | Yes |

## 2. What changes for contributors once merged

- PRs touching the engine get a **Criterion** job per runner (~40 min) next to the existing vector and browser jobs. `ci-pass` gains **Benchmark Tier 0**, which runs fingerprints, allocations and codegen.
- A harness change during v0.13 must still compile against a2ef466; the cumulative jobs fail otherwise.
- An intended change to results, allocations or kernel codegen means re-recording Tier 0 references in the same PR (`suites=tier0`, `record_tier0=true`), reviewed like code.
- An accepted regression needs an entry in `benchmarks/gates/exceptions.json`.

## 3. Decisions needed

| # | Decision | Options | Recommendation |
|---|---|---|---|
| 1 | **PR runtime.** Criterion makes engine PRs ~40 min wall and ~113 runner-minutes, up from ~9 min and ~25 runner-minutes | (a) accept; (b) PR Criterion at 10k only (`--criterion-sizes 10000`; about −25% of the run, since the 1k cases are ~95 s of a 370 s repetition); 19 of 25 1k cases are report-only on x86_64 anyway; (c) run Criterion on PRs only with a label, always on push, schedule and release | (b): it keeps every gated 10k case and arm64's 1k coverage on push. Needs no recalibration |
| 2 | **Unquantized 10k ANN at 64 MiB** is the noisiest per-request gate (A/A median ratio up to 1.36; the arm64 first attempt hit +54% and confirmation cleared it) | (a) keep (50% + confirmation); (b) make it report-only on x86_64; (c) raise queries per `efSearch` from 100 to 300 (≈ +2 min per job) and recalibrate | (a) for now, watch the weekly schedule; (c) if it confirms falsely once |

The product decisions you already approved (two-sided range ordering, no ANN→exact substitution, no hashing change, no `ReadTxn` reverse iteration, evidence in Git + release) are untouched by Stage 0. Nothing in Stage 0 depends on them, and Stage 1 work on item 1 will show its ordering change only as an `id-set` fingerprint, which is order-insensitive by design (DOC-11).

## 4. Repository settings (not changed by me)

1. **Code-owner review for `benchmarks/`** (CODEOWNERS plus "Require review from Code Owners" on `dev`/`main`), so an exception or re-recorded reference cannot be self-approved. Without it the exceptions file is reviewed only as an ordinary file.
2. `ci-pass` stays the required check; it now includes Tier 0. `Vector performance` is not a required check today; making it one on `dev` is optional (release already depends on it).

## 5. Merge mechanics

1. `origin/dev` (`c3a616d`) is behind `main`/`a2ef466` by the 0.12.1 release merge and changelog. The branch is based on `a2ef466`, so a PR into `dev` also carries those two commits. Sync `dev` with `main` first, or accept them in the PR.
2. The PR's own performance run compares against `dev`. The plan job resolves its base; `c3a616d` and `a2ef466` have identical engine sources (empty `git diff` for `packages/core/src`, the core manifest and `Cargo.lock`), so the harness builds on both.
3. After merge, dispatch **Vector performance** with `cumulative=true` once on `dev` as a smoke test. The weekly schedule then runs the cumulative comparison against `a2ef466` every Monday.
4. Delete the `bench/stage0-control-*` branches. They are throwaway positive controls, and the CI runs that used them keep their logs.

## 6. Known limitations at merge

| Limitation | Impact | Follow-up |
|---|---|---|
| x86_64 runners gate 56/88 Criterion cases (median T 11%); report-only there: most 1k cases, 128-d kernels, IDX/BULK inserts, `unindexed_selective/10000`, `doc/range_window/100000` | Regressions in those cases are caught on arm64 only (81/88, median T 5.5%) or reported | Recalibrate when runner hardware changes; consider more samples for batched cases |
| `doc/aggregate_group/*` report-only (bimodal by `RandomState`) | `$group` regressions are only reported | A fixed-seed group fixture would need a harness change |
| Tier 0 2.8–3.7 min on x86_64 (target was 2) | None on wall time (parallel to `rust-check`) | Drop iOS from per-PR codegen if needed |
| Tier 0 references are glibc-specific | A runner image moving to a new glibc may need re-recording | Documented in `benchmarks/README.md` |
| Not measured: macOS, Windows, devices, Safari/Firefox, WASM kernel timing, physical-disk fsync, browser on arm64 | Release notes must not claim performance changes there | Plan Tier 3 |
| Workloads not implemented: HNSW-4/5/7, HYB-3, VEC-3, BULK-6/7 (CI), IDX-4, CONC-3/4/5, REC-1/3/4, MEM-4 | Those areas stay ungated | Add as Stage 2/3 needs them |

## 7. Open items

None for Stage 0. The last control run (40108078013, cosine control under per-case Criterion floors) failed exactly its target cases on both runners. `V013_STAGE0_GATE_RESULTS.json` → `success_criterion` evaluates all four parts as true from the stored results.
