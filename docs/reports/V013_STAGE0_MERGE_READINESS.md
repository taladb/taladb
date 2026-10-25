# v0.13.0 Stage 0 — final merge readiness

Date: 2026-10-24. Baseline: v0.12.1, `a2ef46602127919c60a0fe6333d676b1b1ab391e`.
Evidence: [V013_STAGE0_IMPLEMENTATION_REPORT.md](V013_STAGE0_IMPLEMENTATION_REPORT.md) (§8 covers this round), [V013_STAGE0_GATE_RESULTS.json](V013_STAGE0_GATE_RESULTS.json), plan §7.

## 1. Status

**Stage 0 is merged into `dev`**: PR #100, merge commit `3f57bb7`, merged by the repository owner. `dev` is at `20ed5df`, and its post-merge CI is green. `main` is unchanged at `a2ef466`.

| Criterion | Evidence | Met |
|---|---|---|
| Accepts unchanged performance | Every A/A and validation run passes, on both runners, at PR size against `dev` (PR #100) and at release size against itself and against `a2ef466` (V1, V3) | Yes |
| Detects meaningful regressions | Sorted-page and cosine controls fail exactly their target cases, after confirmation, under the PR and cumulative gates. The BM25, allocation and fused-kernel controls fail Tier 0. The fused kernel fails only where it compiles scalar | Yes |
| Preserves deterministic correctness | 77 fingerprints identical across runners, profiles and `a2ef466` vs candidate | Yes |
| Reproducible evidence against v0.12.1 | `benchmarks/baselines/v0.12.1/` provenance verified; raw evidence on the v0.12.1 release | Yes |
| No engine, storage-format, API or ABI change | Empty source and manifest diff against `a2ef466`; `Cargo.lock` changed only by the separate security PR #101 | Yes |

## 2. Approved decisions, as implemented

1. **PR Criterion at 10k.** 47 cases on PRs (every 10k case plus kernels and fixed-size cases) instead of 77. The Criterion jobs take 25.8 min (x86_64) and 27.5 min (arm64), down from ~40.5. Performance workflow wall time is 33 min, and runner-minutes fall from ~113 to ~84. The 25 1k cases run on push to `main`, the weekly schedule and releases: 7 gated on x86_64, 25 on arm64.
2. **64 MiB unquantized ANN gate** kept at its calibrated threshold, with confirmation.
3. **Full matrix** on schedule and release, unchanged and validated (V3).
4. **Correctness and cumulative gates** unchanged.
5. **VEC-6** fails only on a lost vectorisation, meaning no packed f32 multiply and add on a target where the kernel is expected to vectorise. Count changes are recorded. Re-run of the fused-kernel control: fails aarch64 Linux, iOS and wasm32; x86_64's −20 instructions are recorded, not failed.

## 3. Repository settings still required

These cannot be changed from the repository, and I did not change them. Until they are set, CODEOWNERS only *requests* the owner's review.

| Branch | Setting | Current |
|---|---|---|
| `dev` | Protect: require a PR, 1 approval, **Require review from Code Owners**, required status check `CI — all checks passed`, dismiss stale approvals | Not protected |
| `main` | Add **Require review from Code Owners**, required check `CI — all checks passed`, dismiss stale approvals | PR + 1 approval + signed commits only |
| both | Do **not** require the `Vector performance` jobs: path-filtered, so other PRs would wait forever; job names depend on the baseline. Releases already depend on it | — |

Not enforceable with one maintainer: review of the owner's own changes to `benchmarks/` by a second person. The author can't approve their own PR, and admin bypass is on. A second maintainer with write access is needed for that.

## 4. Before the next `dev` → `main` merge

- `main` requires signed commits, and the Stage 0 and `ctutils` commits are unsigned, like most commits in this repository. Only GitHub's own merge commits are signed. So far unsigned commits have reached `main` through admin bypass (`enforce_admins` is off): `a2ef466` was pushed directly, and #98 carried unsigned commits. A squash merge produces a single GitHub-signed commit. Otherwise sign the commits, or rely on admin bypass knowingly.
- The first push to `main` with Stage 0 runs the new performance workflow. Expect ~50 min wall time (Criterion with the 100k step) and two baselines if the previous commit is not `a2ef466`.
- `ctutils` 0.4.5 (#101) fixes a timing side channel in the shipped `encryption` feature. Consider a v0.12.2 patch release; that is a product decision.

## 5. Known limitations

| Limitation | Impact |
|---|---|
| x86_64 runners gate 56/88 Criterion cases (39/47 on PRs); arm64 81/88 (45/47) | Some regressions are caught on arm64 only, or reported |
| 1k cases on PRs not run | Caught on push to `main` / schedule / release instead of before merge |
| `doc/aggregate_group/*` report-only (per-process `RandomState`) | `$group` regressions are reported only |
| Tier 0 2.8–3.7 min on x86_64 (plan target 2 min) | No wall-time cost (parallel to `rust-check`) |
| Tier 0 references are tied to the CI runner's glibc | Re-record when the runner image changes its C library |
| Not measured: macOS, Windows, devices, Safari/Firefox, browser on arm64, WASM kernel timing, physical-disk durability; HNSW-4/5/7, HYB-3, VEC-3, BULK-6/7, IDX-4, CONC-3/4/5, REC-1/3/4, MEM-4 not implemented | No gate in those areas; release notes must not claim changes there |

## 6. Stage 1

Not started. The gates are ready for it. Roadmap item 1 changes the order of unsorted two-sided range results. DOC-11 fingerprints these as ID sets, so they stay green as long as the matching documents are the same.
