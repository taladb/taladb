# v0.13.0 release candidate — classified findings

Candidate `bcaa6d1d5ac42e3f0a08736fe4945fe07d5ba99e`. Details for every item are in [V013_RELEASE_CANDIDATE_REVIEW.md](V013_RELEASE_CANDIDATE_REVIEW.md) (§ numbers below).

| Class | Meaning |
|---|---|
| **BLOCKER** | Must be resolved before tagging; the candidate would have to change |
| **SHOULD FIX** | Resolve before tagging unless the owner explicitly accepts it for 0.13.0 |
| **ACCEPTED LIMITATION** | Proposed for acceptance as-is, with the owner's sign-off; documented |
| **DEFERRED** | Pre-existing or cosmetic; tracked for a later release |

## BLOCKER

**None.** No correctness, compatibility, API/ABI, format, durability or security problem was found, and no performance gate failed.

The Zen 3 `count_indexed_eq` check is **PASS**: neutral on an EPYC 7763 against both the PR base (1.005 / 1.003) and v0.12.1 (1.002 / 0.989), run 40701046899. It is therefore not a blocker and needs no exception.

## SHOULD FIX

| ID | Finding | Evidence | Proposed resolution | Owner decision needed |
|---|---|---|---|---|
| SF-1 | **The partial-block rebuild is a sustained regression for write-then-search patterns.** With one unfiltered exact search per vector write on an index larger than the cache, every search rebuilds the prefix: +89% total query time and +86% CPU per request at 50k × 384 / 64 MiB; +16% at 10k × 384 / 8 MiB. Break-even is about 2.5 and 1.7 searches per write. Cause: about 16,200 page faults per rebuild of a fresh 64 MB block. | §5.2–5.3; `p3-time-summary.json`; perf stat 221,150 vs 26,833 faults | Separate PR: reuse the stale block's buffers for the rebuild (§5.4). No retention-policy change. | Fix before 0.13.0, or ship with SF-3's wording and fix in 0.13.1 |
| SF-2 | **The CHANGELOG ANN-rescoring figure did not reproduce.** It says "13–21% less time on arm64"; the release run measured −9.1% arm64 and −7.5% x86_64 (64 MiB, block retained, report-only). | §9, §10; perf 40654385980 | Reword to the range across runs, or "about 10%" | Wording |
| SF-3 | **The CHANGELOG understates the partial-retention cold cost.** It says "8–21% longer, or up to 80% longer when it keeps tens of megabytes". File-backed: +16% (10k × 384, 8 MiB) and +90–100% (50k × 384, 64 MiB). The one-search-per-write case is not mentioned. | §5, §10 | Restate the figures and the break-even (about 2 searches per write); or drop the caveat once SF-1 is fixed and re-measured | Wording |
| SF-4 | **The version is not bumped.** Every manifest says 0.12.1 (`sync-version.js --check 0.13.0` fails), and the CHANGELOG heading is "Unreleased". | §10 | One release commit on top of `bcaa6d1`: `sync-version` + CHANGELOG heading/date only. Verify with `git diff --stat bcaa6d1` (manifests, lockfile entries, CHANGELOG, docs config) and a CI run; no new performance run is needed for a manifest-only diff. | Release date |
| SF-5 | **Branch protection does not enforce validation.** `dev` is unprotected. `main` has no required checks, `enforce_admins` off and **force pushes allowed**, and a one-approval rule that a sole maintainer can only bypass. Engine PRs merged 11 s–25 min after opening, before their performance runs finished. | §8 | Ruleset as in §8: 0 approvals, required `CI — all checks passed` + a new always-reporting `perf-pass`; block force pushes; no standing bypass | Repository settings; the `perf-pass` workflow change is its own PR |

## ACCEPTED LIMITATION (proposed)

| ID | Limitation | Evidence | Why acceptable |
|---|---|---|---|
| AL-1 | DOC-11 two-sided range: 5.8–6.0× at 10k (target ≥ 10×) and 39–44× at 100k (target ≥ 50×) | §9; Stage 1A readiness §2–3 | The candidate runs at 1.06–1.68 ms, the cost of decoding the 800 matching 1 KiB documents (8 × `doc/page_full`). The range work itself is gone; on small documents it gives 12.6× / 130×. |
| AL-2 | DOC-13 unindexed count −26% to −28% (target ≥ 40%) | §9; Stage 2A readiness §1 | The rest is UTF-8 validation, postcard walking and redb iteration. These are needed to keep failing on damaged records exactly as before. |
| AL-3 | HNSW-3 rescoring with a retained block −7.5% to −9.1% on this run (target ≥ 15%); CONC-2 four-thread CPU +45% to +122% (target ≤ +40%) | §9; Stage 2B readiness §3 | Results are unchanged and nothing regresses. Retained blocks rarely coexist with ANN (only an unfiltered exact search fills one; a graph reservation drops a partial one). The four-thread cost comes from the exclusive graph-cache lease and needs a shared node cache: Stage 3 material, not a regression. |
| AL-4 | VEC-2 arm64 met by 0.6 points (−35.6% vs ≥ 35%); Stage 1B measured −34.9% vs `dev` | §9; Stage 1B readiness §3 | Cosine now costs what dot product costs: this is the kernel's floor. |
| AL-5 | Partial blocks do not help while an HNSW graph is resident, and every graph reservation drops them | §5.2 (`ann:3` equal to base on L1 and P1) | By design (Stage 2C): a partial block never displaces a graph. Measured: no rebuild churn under an ANN/exact mix. |
| AL-6 | Untested platforms: macOS, Windows (build only), iOS (not built), Android device, React Native runtime, Firefox/Safari | §11 | Engine and bindings sources show no platform-specific change. The release workflow builds iOS/Android, and the wrapper bump PRs run their device and simulator CI. No performance claim is made for these platforms. |
| AL-7 | The full Rust test suite ran on aarch64 under qemu emulation, not on hardware | §4 | Hardware arm64 CI ran Tier 0 (fingerprints, VEC-6, MEM-2), all vector and Criterion A/B and MEM-3; the emulated suite passed 876/876. |

## DEFERRED

| ID | Finding | Evidence | Note |
|---|---|---|---|
| D-1 | BM25 score bits vary between processes for queries of three or more distinct terms, so near-ties can swap order. Query tokens come from `HashMap::into_keys()` (random hasher), and floating-point sums depend on the order. | §7; `p5x` repeat runs on an identical file: 3 different hashes for `fts/1`, the same on v0.12.1 | Pre-existing (`fts.rs` unchanged). Fix: sort the tokens before scoring. That changes score bits, so it belongs in its own release note. |
| D-2 | A partial block can be admitted next to a graph cached while the block was being built (`insert` does not check again what `reserve_partial` checked) | §3.1 | No wrong result; the next graph reservation drops the block. Tidy up with SF-1. |
| D-3 | `CachedVectors` and `VectorCacheMap` are public but unused by the cache | §3.1 | Removing them would be an API break; deprecate in a future minor. |
| D-4 | Every open rewrites the database file, in both versions (the same hash sequence) | §7 | Observation; matters only for read-only media or file-integrity monitoring. |
| D-5 | FTS terms are stored in plain text in encrypted databases | §7 (1,349 plaintext matches of an indexed word) | Covered by the documented "index keys are not encrypted"; name FTS terms explicitly in `docs/api/encryption.md`. |
| D-6 | Bytes `$gt`/`$lt` never match, on both the index and scan paths | Sort-hardening readiness (on the unmerged `docs/v013-sort-hardening-reports` branch) | Pre-existing feature gap. The sort-hardening reports themselves were never merged into `dev`. |
| D-7 | The public helper `index::index_range_cmp` now returns wider Str/Bytes ranges (callers post-filter) | §3 | Mention in the API docs if the helper is meant for external use. |
