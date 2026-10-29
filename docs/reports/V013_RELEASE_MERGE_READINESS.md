# v0.13.0 release candidate — merge readiness

Candidate: **`bcaa6d1d5ac42e3f0a08736fe4945fe07d5ba99e`** (branch `release/v0.13.0-rc-validation`, equal to `dev` after #120). Details: [review](V013_RELEASE_CANDIDATE_REVIEW.md), [findings](V013_RELEASE_BLOCKERS.md), [data](V013_RELEASE_CANDIDATE_RESULTS.json).

## 1. Verdict

**GO, conditional: release `bcaa6d1` as v0.13.0 once the conditions below are met. There are no BLOCKERs.**

- **Clean:** correctness, compatibility, API/ABI, format and durability (review §3–§4, §7).
- **Performance:**
  - Every performance gate passes on x86_64 and arm64, against both the PR base and v0.12.1.
  - The Zen 3 `count_indexed_eq` check passes on an EPYC 7763 (review §6).
  - each cumulative Criterion job shows 19–23 improved cases against v0.12.1, and none regresses.

Conditions:
1. **SF-2 and SF-3:** correct the two CHANGELOG figures (review §10).
2. **SF-4:** a manifest-only version commit (0.13.0 + CHANGELOG heading), with CI passing on it.
3. **SF-1:** your decision (§3.1).
   - Shipping now, with SF-3's wording, keeps this GO.
   - Fixing first makes a new candidate, which needs re-validation: the Phase 3 harness, the release dispatch, and a Zen 3 draw.
4. **AL-1 to AL-7:** your acceptance of the missed targets and the untested platforms (findings, ACCEPTED LIMITATION).

**NO-GO** applies if you want SF-1 fixed in 0.13.0 or a missed target met; in either case the candidate changes.

## 2. Gates on the frozen SHA

| Gate | Result | Run / evidence |
|---|---|---|
| CI dispatch, full matrix (Linux, macOS and Windows builds; Rust tests; wasm32; MSRV; audit; Miri; fuzz; Tier 0 on x86_64 + arm64; RN glue; TS) | **PASS**, 20/20 jobs | [40654361430](https://github.com/taladb/taladb/actions/runs/40654361430) |
| Performance, release size, vector + Criterion + browser, vs PR base 92b402c and v0.12.1 | **PASS**, 15/15 jobs, 0 regressions, 0 failures | [40654385980](https://github.com/taladb/taladb/actions/runs/40654385980) |
| Criterion re-draws for Zen 3 (three dispatches, criterion only) | **PASS**: 11/12 jobs, 0 regressions. One x86 job lost its runner (infrastructure shutdown; no result). x86 CPUs drawn: EPYC 7763 ×4, EPYC 9V74 ×1 | [40701046899](https://github.com/taladb/taladb/actions/runs/40701046899), [40701062010](https://github.com/taladb/taladb/actions/runs/40701062010), [40701075943](https://github.com/taladb/taladb/actions/runs/40701075943) |
| `count_indexed_eq` on Zen 3 vs PR base and v0.12.1 | **PASS**: EPYC 7763, 10k/100k: 1.005/1.003 vs PR base and 1.002/0.989 vs v0.12.1 (run 40701046899) | §6 of the review |
| Local x86_64 replay of the CI Rust gates (i5-9400F) | **PASS**: 876 tests, 0 failures | `logs/x86-summary.txt` |
| aarch64 workspace tests (emulated) | **PASS**: 876 tests, 0 failures | `logs/arm64-qemu-test.log` |
| Public API / ABI / storage format / durability | **Unchanged** | review §3 |
| Kotlin wrapper (52 tests), Swift wrapper on Linux (43), TS client with the native Node binding (224), web Node tests (8) | **PASS** | `logs/` |
| v0.12.1 ↔ v0.13.0 data compatibility, snapshots, encryption, 28 SIGKILL recoveries, concurrent stress | **PASS** | review §7 |
| Cold partial-cache investigation | **Done**: sustained regression for one search per vector write (SF-1) | review §5 |

## 3. Decisions for the owner

1. **SF-1, partial-block rebuild cost.** Choose one:
   - **(a)** Fix before 0.13.0 with the buffer-reuse change (review §5.4), then re-run the Phase 3 harness and the release dispatch. Recommended if write-then-search on large indexes is a common pattern for your users (notes apps, for example).
   - **(b)** Ship 0.13.0 with corrected release notes (SF-3) and fix in 0.13.1. Recommended if typical apps search more than twice per vector write, where the change is already a 25–55% gain.
2. **Release notes:** accept the corrections in SF-2 and SF-3.
3. **Missed targets:** accept AL-1 to AL-4 as measured floors, or keep them as open items for Stage 3.
4. **Zen 3 check:** none needed. It passes without an exception, so Stage 2B's suggested cumulative exception can be dropped. The root-cause evidence stays in the Stage 2B reports.
5. **Repository settings:** apply the ruleset in review §8 (SF-5). The `perf-pass` workflow change is a separate PR.

## 4. Release steps once approved (none performed)

1. Release commit on `release/v0.13.0-rc-validation`: version 0.13.0 (`node scripts/sync-version.js`) and the CHANGELOG heading and date, plus the SF-2/SF-3 wording. Check that `git diff --stat bcaa6d1` shows only manifests, lockfile version entries, the CHANGELOG and docs config.
2. Run CI on that commit (workflow dispatch). The engine is unchanged, so the performance evidence for `bcaa6d1` stands.
3. Merge `dev` → `main` by PR (signed merge commit). Today this is an admin bypass of the one-approval rule (SF-5).
4. Tag `v0.13.0` on `main`. release.yml runs preflight, its own performance gate (the release dispatch) and the publishing jobs. Expect the x86 Criterion job to draw a random CPU.
5. Merge the Kotlin and Swift engine-bump PRs after their CI passes. Wrappers keep their own versions.

## 5. Rollback

There is no data migration in either direction, because v0.12.1 reads everything v0.13.0 writes. Yank the crate, deprecate the npm packages and move `latest` back, and revert the merge on `main`/`dev`. Details: review §12.

## 6. Stage 3

Not started. It should wait for this review's decisions, and in particular for SF-1. Its fix touches the same cache code that a Stage 3 shared graph-node cache (AL-3) would.
