# v0.13.0 release candidate — review

Frozen candidate: **`bcaa6d1d5ac42e3f0a08736fe4945fe07d5ba99e`** (`dev` after PRs #119–#120), on branch `release/v0.13.0-rc-validation`, which carries no commits of its own. Baseline: v0.12.1 at `a2ef46602127919c60a0fe6333d676b1b1ab391e`. PR base for A/B comparisons: `92b402c6ac4837e9af5a56c21bfe4bc9ab999421` (`dev` before Stage 2C).

Decision page: [V013_RELEASE_MERGE_READINESS.md](V013_RELEASE_MERGE_READINESS.md). Classified findings: [V013_RELEASE_BLOCKERS.md](V013_RELEASE_BLOCKERS.md). Data: [V013_RELEASE_CANDIDATE_RESULTS.json](V013_RELEASE_CANDIDATE_RESULTS.json).

No features were added, no Stage 3 work was started, no repository setting was changed, nothing was merged to `main`, tagged or published.

## 1. Summary

- **Correctness and compatibility: no problem found.**
  - Every test suite passes on the frozen SHA: Rust on x86_64 and on emulated aarch64, wasm32, Node, the TypeScript client, Kotlin and Swift.
  - An independent review with a randomized differential test found no wrong result, panic or stale-cache use.
  - v0.12.1 and v0.13.0 open each other's databases and snapshots, and both recover from writers killed mid-operation.
- **Scope: as intended.**
  - No public API, ABI, storage-format or durability change.
  - The only dependency change is the RUSTSEC lockfile fix; MSRV 1.90 holds.
- **Performance: every gate passes on both architectures.** Against v0.12.1 and against the PR base, nothing regresses, and each cumulative Criterion job shows 19–23 improved cases. Four original targets are missed (§9). DOC-11 and DOC-13 are at measured floors; HNSW-3 and CONC-2 would need a Stage 3 change.
- **One sustained regression, from Stage 2C (§5).** When an application runs one exact vector search per vector write on an index larger than the cache, every search rebuilds the partial block:
  - 50k × 384 at 64 MiB: +89% query time.
  - 10k × 384 at 8 MiB: +16%.
  - The cause is page faults on a freshly allocated block. A small, separately reviewable fix is proposed and was not applied.
- **Release notes need two corrections** (§10): the Stage 2B ANN figure did not reproduce, and the Stage 2C cold-cost figure understates the write-then-search case.
- **Zen 3 (§6):** on an EPYC 7763, `count_indexed_eq` is neutral against both the PR base and v0.12.1: **PASS**.
- **Process (§8):** `dev` has no protection, and `main` can only be merged by administrator bypass.

## 2. What was frozen and how

- `origin/dev` was `bcaa6d1` when validation started, equal to the reviewed SHA. The branch `release/v0.13.0-rc-validation` was pushed pointing at that commit. No workflow triggers on it.
- CI and performance runs were dispatched on that branch, and every run records `headSha = bcaa6d1…`. One performance dispatch with a mistyped baseline (40654367252) was cancelled before it ran and re-dispatched.
- Local runs used detached worktrees at the same SHA. The reports are on a separate branch (`docs/v013-release-candidate-reports`), so the frozen branch stays equal to the candidate.
- The build inputs (packages, benchmarks, scripts, Cargo files, toolchain, workflows) are byte-identical to Stage 2C's PR head `ebdee16`. Only `CHANGELOG.md` and three reports differ (`git ls-tree` hash `b551f540…` on both).

## 3. Phase 1 — release scope audit

All eight merged items were audited on the final code, not only through their reports.

| Check | Result | How |
|---|---|---|
| Public Rust API | **Unchanged** | `cargo public-api -p taladb --all-features`: identical on v0.12.1 and the candidate, 2,406 items simplified and 5,002 lines in the full listing, auto traits and blanket impls included |
| C ABI / bindings | **Unchanged** | No file under `packages/bindings` changed; `taladb.h --check` passes in CI |
| Storage format | **Unchanged** | Key and value encoders untouched; the index type tags are now named constants with the same values (0x40, 0x50); no new tables or meta keys. The cross-version tests in §7 confirm it. |
| Durability | **Unchanged** | No write-transaction, commit, flush or durability code in the diff; all new code runs inside existing read transactions |
| Dependencies | **Lockfile only** | ctutils 0.4.2→0.4.5, tokio 1.50.0→1.54.1, tokio-macros 2.6.1→2.7.2, mio 1.1.1→1.2.4 (RUSTSEC-2026-0385/-0386, 5413b42); mio/tokio now depend on windows-sys 0.52; no manifest change |
| MSRV | **1.90 holds** | `cargo +1.90 check --workspace --all-features`, CI and local |
| Correctness regressions | **None found** | §3.1 |

Behaviour notes for API users, beyond the CHANGELOG:
- The public helper `index::index_range_cmp` now returns wider ranges for Str/Bytes bounds. Its callers post-filter.
- `sort_documents` and `partial_sort_documents` use the new NaN and 2^53 order. This is documented.
- `CachedVectors` and `VectorCacheMap` remain public but the cache no longer uses them. Removing them would be an API break, so they stay.

### 3.1 Independent code review

A separate reviewer read the whole `packages/core/src` diff without the stage reports. It also ran a randomized twin test: about 34,000 rounds over 5 seeds, comparing an indexed collection with an unindexed one. The test covered:
- Sorted pages and aggregations, in both directions and with one or two keys.
- One- and two-sided ranges, `count`, `findOne`, `updateMany` and `deleteMany`.
- Int, ±inf, NaN, ±0, Str and Bytes containing NUL, mixed types, arrays that appear later, and ids starting with 0xFF.

Both planted mutations failed the test quickly: a descending-window off-by-one at round 57, and a reverted NUL count fix at round 6. **No wrong result, panic, stale-cache use or budget-accounting error was found.** Findings:
1. A partial block can be admitted next to a graph that was cached while the block was being built: `reserve_partial` checks for a graph, but `insert` does not check again. Results are unaffected, and the next graph reservation drops the block. DEFERRED.
2. Any graph reservation drops every partial block. §5 measured whether this causes rebuild churn under an ANN/exact mix: it does not, because a resident graph keeps partial blocks from being built at all.
3. The dead public types noted above. Cosmetic.

## 4. Phase 2 — validation of the frozen SHA

| Gate | x86_64 | aarch64 | Evidence |
|---|---|---|---|
| fmt, check, clippy `--workspace --all-targets --all-features -D warnings` | PASS (CI, local) | — | [CI 40654361430](https://github.com/taladb/taladb/actions/runs/40654361430) |
| Workspace tests, all features | PASS: 64 suites, 876 passed, 0 failed, 3 ignored (local); CI pass | PASS: the same 876 / 0 / 3, **emulated** (qemu-user 7.2, release profile, cross-built) | logs `x86-test.log`, `arm64-qemu-test.log` |
| wasm32 clippy (with and without legacy reader), wasm32 tests on Node | PASS (CI, local) | — | |
| MSRV 1.90 | PASS (CI, local) | — | |
| `cargo audit` | PASS (CI, local) | — | |
| Miri, Stacked and Tree Borrows (FFI pointer layer) | PASS (CI, local) | — | |
| Fuzz smoke (60 s per target) | PASS (CI) | — | |
| WASM build + browser integration (storage, ownership, vectors) | PASS (CI, Chromium) | — | CI job "Build — WASM" |
| Browser worker + OPFS A/B, 1 MiB and 8 MiB | PASS vs PR base and v0.12.1 | — | [perf 40654385980](https://github.com/taladb/taladb/actions/runs/40654385980) |
| Native vector A/B (8 MiB, 64 MiB, staged, filtered, concurrency; none + binary) | PASS | PASS | perf 40654385980, 8 jobs |
| Tier 0: fingerprints, VEC-6 codegen (x86_64, aarch64 Linux/iOS, wasm32), MEM-2 | PASS | PASS | CI Tier 0 jobs |
| MEM-3 (cache accounting asserts, 1 MiB and 8 MiB, concurrent) | PASS | PASS | perf `vector-memory-*.json` |
| Full Criterion A/B, release size (1k, 10k, 100k doc) | PASS: 0 regressions | PASS: 0 regressions | perf 40654385980 and the three draw runs (§6) |
| Node binding build | PASS: Linux, macOS, Windows (CI); local Linux build + TS client suite: 17 files, 224 tests, native e2e not skipped | — | CI "Build — Node.js" ×3, `node-ts.log` |
| CLI build | PASS: Linux, macOS (aarch64), Windows | — | CI "Build — CLI" ×3 |
| TypeScript (taladb, @taladb/react, @taladb/react-native) type check and builds | PASS | — | CI |
| React Native native glue (header check, JSI/JNI compile, Android 16 KB alignment) | PASS | — | CI "React Native — native glue" |
| Kotlin wrapper (taladb-kotlin `main` d8bf3df), host JVM unit tests against this engine | PASS: 52 tests, 0 skipped | — | `wrapper-kotlin.log` |
| Swift wrapper (taladb-swift `main` d56d88b), Linux XCTest, warnings as errors | PASS: 43 tests | — | `wrapper-swift.log` |
| Web binding Node tests (worker ownership, cache config) | PASS: 8/8 | — | `web-node-tests.log` |

The aarch64 test run is functional coverage under emulation. It is not hardware. On real arm64 hardware (GitHub `ubuntu-24.04-arm`), the candidate ran Tier 0, the vector suite and Criterion; those runs include the fingerprint correctness checks. **What could not be tested is listed in §11.**

## 5. Phase 3 — cost of cold partial-cache builds

### 5.1 Method

`p3_cold.rs` (evidence) was built into the candidate and the PR base `92b402c` from one copy, with the JCC-erratum mitigation on both sides.
- **Data:** file-backed databases with cosine indexes.
  - L: 50k × 384-d at 64 MiB; the prefix is 63.9 MB.
  - P: 10k × 384-d at 8 MiB; the prefix is 7.9 MB.
  - Variants with an HNSW graph (`L1`, `P1`) and with two vector fields (`L2`, `P2`).
- **Runs:** each run starts from a fresh copy of the dataset. Candidate and base are interleaved, with 3 repeats, medians reported. Single-threaded runs are pinned to one core.
- **Metrics per request:** wall latency (p50/p95/p99), thread CPU, decoded vectors, allocations, and peak heap above the request's start (a separate counting-allocator build), plus process high-water marks.
- **Machine:** the local i5-9400F was shared with the owner's own builds during the run (run queue 14–16 on 6 cores, swap in use).
  - The first attempt, on a btrfs working copy, showed multi-second I/O stalls. It was discarded and kept as `p3-time-diskbacked-aborted.jsonl`.
  - The measured runs use tmpfs copies, so writes carry no fsync cost.
  - **Ratios and CPU per request are the reliable outputs. Absolute wall latencies are indicative.**

Base pays a large first-query cost after every write too (101.7 MB peak, about 60 ms of p95 on L). That is redb refilling its page cache, the same on both sides.

### 5.2 Results

`E0` is the first exact query after the round's invalidation; `Ew` the later ones. Latencies are p50 / p95 / p99 in ms, candidate vs base.

| Pattern | Data | Total query time cand / base | CPU per request (ms) | `E0` | `Ew` |
|---|---|---|---|---|---|
| 1 exact query per vector update | L, 64 MiB | **1.89×** (589 / 311 ms) | 48.3 / 26.0 | 46.1/48.7/86.5 vs 22.6/25.1/64.9 | — |
| 2 queries per update | L | 1.13× | 28.3 / 24.8 | 45.5/82.6/82.6 vs 23.0/65.3/65.3 | 8.2/8.9/8.9 vs 22.9/23.6/23.6 |
| 5 queries per update | L | 0.70× | 16.5 / 23.5 | 45.3 vs 24.1 | 8.2 vs 22.9 |
| 10 queries per update | L | 0.53× | 12.4 / 23.6 | 45.2 vs 23.3 | 8.4 vs 23.0 |
| 2 queries per insert | L | 1.17× | 28.4 / 24.4 | 43.8 vs 21.8 | 8.7 vs 21.8 |
| Read-only, 20 queries | L | 0.47× | 11.9 / 25.3 | first 86.4 vs 64.5 | 8.1/9.1/9.9 vs 23.4/25.3/25.4 |
| Update, 1 unfiltered + 4 dense-filtered (50%) | L | 0.93× | 83.2 / 89.4 | 48.5 vs 23.3 | filtered 87.5 vs 100.5 |
| 5 per update at 32 MiB | L | 0.80× | 19.4 / 24.1 | 24.9 vs 23.7 | 15.1 vs 22.8 |
| 1 exact query per vector update | P, 8 MiB | **1.16×** (110 / 95 ms) | 5.6 / 4.8 | 4.8/7.1/15.3 vs 4.2/5.0/12.6 | — |
| 2 / 5 / 10 per update | P | 0.95× / 0.75× / 0.73× | 4.3/4.5 · 3.2/4.3 · 3.1/4.3 | ~5.0 vs 4.2 | ~2.6 vs 4.1 |
| 5 per update at 4 MiB | P | 0.85× | 3.7 / 4.4 | 4.3 vs 4.3 | 3.3 vs 4.2 |
| Read-only, 30 queries | P | 0.65× | 3.0 / 4.6 | first 14.8 vs 12.5 | 2.5 vs 4.3 |
| Graph present, 1 ANN + 3 exact per round | L1 / P1 | 0.97× / 1.04× | equal | equal (no prefix is built while the graph is resident) | equal |
| Graph on disk, never loaded, read-only | L1 | 0.51× | 12.7 / 24.9 | as L | as L |
| Two vector fields, alternating, read-only | L2 / P2 | 0.83× / 0.91× | | `v` first 92.8 vs 69.9 · 25.6 vs 12.6 | `v` 8.6 vs 24.5; `w` unchanged (no idle memory left, nothing evicted) |
| Two fields, update `v`, 3 × (`v`, `w`) | L2 | 0.95× | | `v` 46.0 vs 21.8; `w` unchanged | `v` 7.9 vs 20.9 |
| Pressure Normal→Moderate→Critical→Normal, 5 queries each | L / P | 0.75× / 0.85× | | Moderate first 27.4 vs 23.6; Critical equal; first after recovery **43.4 vs 24.7** · 6.7 vs 4.2 | Moderate 18.9 vs 23.8; after recovery 8.5 vs 24.0 |
| 4 threads, 1 update then 3 queries per thread per round | L / P | 0.74× / 0.77× | | 38.6/74.7 vs 33.4/61.0 | p99 34.0 vs 34.1 (threads that query during the build stream) |
| 4 threads, read-only | L | 0.46× | | first 76.5 vs 60.3 | 9.7 vs 33.5 |

Memory, allocations and decodes (counting-allocator build):
- **Cold queries:** the candidate peaks 64.0 MB higher than base on L (165.7 vs 101.7 MB) and 7.9 MB higher on P (28.5 vs 20.6 MB). That is exactly the prefix. Allocation counts equal base's within 6 per query.
- **Warm queries:** 6,914 decodes instead of 50,000 on L, and 4,651 instead of 10,000 on P. Allocations fall by the same proportion.
- **Process high-water mark:** 172–178 MB vs 109–114 MB on L, and 35–37 MB vs 27–29 MB on P.
- **Concurrent builds:** under 4 threads only one thread builds a prefix (admission refuses while a reservation is active). The peak stays at 166 MB, not 4 × 64 MB.

### 5.3 Break-even and cause

- **Break-even** = 1 + (cold extra ÷ warm saving), from mean latencies:
  - About **2.4–2.6 queries per vector write** on L at 64 MiB (cold +21.7 ms, saving 14.6 ms per later query).
  - About **1.6–1.8** on P at 8 MiB (cold +1 ms, saving 1.5 ms).
  - About 1.6 on L at 32 MiB, where the prefix is smaller.
- **Cause: page faults on a fresh block.** Over 12 rounds of one query per write on L, the candidate takes 221,150 minor faults and base 26,833. The difference, about 16,200 per cold build, is 64 MB in 4 KiB pages. At about 1.3 µs per fault this accounts for the 21–23 ms cold cost; the copy and norm computation are small next to it.
- **Why every rebuild faults:** when a write changes the revision, the stale block is freed at the next lookup. glibc returns the memory to the OS, and the rebuild faults a new block in.
- **Sustained regression:** an application that writes a vector and immediately searches, once per write, pays this on every search: +89% at 50k × 384 on a 64 MiB desktop budget and +16% at the phone budget. Two searches per write roughly break even; more than that is a net gain. Read-mostly workloads gain 35–55%.

### 5.4 Proposed fix (not applied)

**Recycle the stale block's buffers.** When the next unfiltered exact search finds a block of an older revision for the same key, the cache now drops it. It could instead hand the block's `Vec` capacity (ids, values, norms) to the rebuild, which is sized the same or smaller.
- The stale block is already resident until that lookup, so this holds no memory the current policy does not.
- Admission, budget and eviction rules stay unchanged.
- It does not help the first build after open or after a pressure recovery; those keep the current cost.
- Expected effect: the cold build loses most of its ~16,000 faults on L, which should bring the one-query-per-write case close to base.
- It needs its own PR with a mutation-tested reuse path and this harness as the gate.

The alternatives change the retention policy and are not recommended without a decision:
- Build only on the second uncached search of a revision. This moves the break-even rather than removing the cost.
- Cap the prefix size.
- `MAP_POPULATE` or huge pages, which are platform-specific.

## 6. Phase 4 — cumulative x86 performance and `count_indexed_eq`

**PASS on Zen 3.** Four performance runs on the frozen SHA gave eight x86 Criterion jobs. The CPU model of each runner, read from the artifact's `environment.cpu_model`:

| Run | x86 job | CPU | `count_indexed_eq/10000` | `/100000` | Other cases |
|---|---|---|---|---|---|
| [40654385980](https://github.com/taladb/taladb/actions/runs/40654385980) (full release dispatch) | vs PR base 92b402c | Intel Xeon Platinum 8573C | 0.996 neutral | 1.003 neutral | 77 neutral, 1 improvement |
| | vs v0.12.1 | AMD EPYC 9V74 (Zen 4) | 1.000 neutral | 0.987 neutral | 63 neutral, 15 improvements |
| [40701046899](https://github.com/taladb/taladb/actions/runs/40701046899) | vs PR base | **AMD EPYC 7763 (Zen 3)** | **1.005 neutral** | **1.003 neutral** | 77 neutral, 1 improvement |
| | vs v0.12.1 | **AMD EPYC 7763 (Zen 3)** | **1.002 neutral** | **0.989 neutral** | 62 neutral, 16 improvements |
| [40701062010](https://github.com/taladb/taladb/actions/runs/40701062010) | vs PR base | AMD EPYC 7763 | 1.014 neutral | 1.009 neutral | 77 neutral, 1 improvement |
| | vs v0.12.1 | — | runner shut down during pair 2 ("The runner has received a shutdown signal"): infrastructure, no result | | |
| [40701075943](https://github.com/taladb/taladb/actions/runs/40701075943) | vs PR base | AMD EPYC 7763 | 1.022 neutral | 1.013 neutral | 77 neutral, 1 improvement |
| | vs v0.12.1 | AMD EPYC 9V74 | 1.024 neutral | 1.031 neutral | 61 neutral, 17 improvements |

- **No regressions:** across the 15 completed Criterion jobs (7 x86, 8 arm64, both baselines), no case regressed. The x86 `doc/count_indexed_eq` ratios at 1k, 10k and 100k range from 0.987 to 1.031. On arm64 they range from 0.995 to 1.018.
- **Stage 2B's Zen 3 regression is absent:** +22% to +34%, 6/6 pairs, on an EPYC 7763 does not appear on the frozen candidate, on the same CPU model, against either baseline. The Stage 2C report gave the reason: the closure the controls identified (`count_plan_entries::{closure#0}`) moved with Stage 2C's layout. The candidate's build inputs are byte-identical to Stage 2C's PR head (§2), whose own EPYC 7763 cumulative run also measured neutral (0.986 / 0.988, run 40642210555 attempt 2). **This is a layout outcome, not a fix.** Any later change to the engine can move the closure again.
- **Root-cause evidence is preserved, nothing changed:** Stage 2B readiness §1.1 and `layout_controls` in `V013_STAGE2B_BENCHMARK_RESULTS.json`. No padding, alignment, tolerance or exception was added. `benchmarks/gates/exceptions.json` is unchanged.
- **The exception Stage 2B proposed is no longer needed for this release:** the gate passes without it on Zen 3.

Other cumulative results against v0.12.1 are in §9. Every Criterion job also compared result fingerprints before timing, and all matched.

## 7. Phase 5 — compatibility and recovery

`p5_compat.rs` (evidence) was built into v0.12.1 (`a2ef466`) and the candidate with `--features encryption`. Its database covers:
- Indexes created both before and after the data: scalar, string, float, a NUL-string field and a compound index.
- An array field, an FTS index, an HNSW cosine field and a flat Euclidean field.
- Deletes, updates and a second collection.

The answer sheet hashes ids, score bits and full document content (`_changed_at` excluded) in 123 answers:
- `find`/`count` over 13 filters, and sorted pages on 5 fields × 2 directions × 4 skip/limit windows.
- Exact, ANN, filtered and sparse vector searches, with their execution paths.
- Vector index status, the index list, BM25 and hybrid results.

| Scenario | Result |
|---|---|
| v0.12.1 database read by v0.13 | Equal, except the 3 intended NUL fixes (`count {z:'a'}` 104→46 now equals `find`; `{z:{$gt:'a'}}` 0→58) and one BM25 tie order (below) |
| Reopen by v0.13; v0.12.1 reopening after v0.13 read it | Equal |
| v0.13 writes on a v0.12.1 database, read back by v0.12.1 (rollback) | Equal except the same 3 NUL queries, where v0.12.1 shows its old answers |
| The same writes made by v0.12.1 vs by v0.13 | Identical answers |
| Snapshots | Byte-identical exports (2,331,582 bytes); each version restores the other's |
| Encryption | Cross-version reads equal; the wrong passphrase gives the same error on both; v0.13 writes read by v0.12.1 |
| SIGKILL during inserts, updates, index creation and graph rebuild (offsets inside each phase), writer v0.13 or v0.12.1 | 28 kills, all consistent when reopened by both versions: committed prefix only, indexes = scans, vector status = documents, graph ready |
| 40 s of 2 writers and 3 readers (indexed equality, sorted pages, ranges, exact search with score recomputed from the returned vector) | 0 violations on both; the candidate served 11,129 reads vs v0.12.1's 5,577 |

Pre-existing observations, unchanged by v0.13:
- **BM25 score bits vary between processes for queries of three or more distinct terms**, in both versions. Query tokens come from `HashMap::into_keys()` with a random hasher, so the per-term sum is added in a different order each run. Near-ties can then swap. Two-term sums are exact, so their results are stable. DEFERRED.
- **Both versions rewrite the file on every open.** The hash sequence over successive opens is identical between versions. DEFERRED, observation only.
- **FTS tokens are stored in plain text in encrypted databases.** This is covered by the documented "index keys are not encrypted"; the docs could name FTS terms explicitly.

The Tier 0 fingerprints (77 cases, CI x86_64 and arm64) also match the v0.12.1 references.

## 8. Phase 6 — release process

Current state (GitHub API, 2026-10-28):
- **`dev`:** no branch protection.
- **Rulesets:** none on the repository. Organization rulesets could not be read without the `admin:org` scope.
- **`main`:** requires 1 approving review and signed commits. It requires no status checks, `enforce_admins` is off, and **force pushes are allowed**.
- **Collaborators:** one, `akosidencio`, an admin. With one maintainer, the required review cannot be given by anyone else, so every merge to `main` is an administrator bypass.
- **How engine PRs have merged:** #106, #110, #111, #114, #116 and #119 were merged between 11 s and 25 min after opening. #119 was merged 44 s after `CI — all checks passed`, about 28 minutes before its PR-size performance run finished.

Recommended configuration (maintainer action; nothing was changed):

1. **One repository ruleset for `dev` and `main`, administrators included in enforcement.**
   - Require a pull request with **0 required approvals**. With one maintainer, any approval requirement guarantees a bypass on every merge.
   - Block force pushes and deletions.
   - Require signed commits on `main`.
2. **Required status checks:** `CI — all checks passed` (ci.yml, already designed for docs-only PRs) and a new `Vector performance — all checks passed`.
3. **Make the performance gate requirable without deadlocking docs PRs.** Move `performance.yml`'s `pull_request` path filter into a `changes` job, the same pattern `ci.yml` uses:
   - Add a final `perf-pass` job with `if: always()` that succeeds when no engine path changed and otherwise requires every comparison job to succeed.
   - A skipped workflow reports no status at all, which is why a required check on the path-filtered workflow would hang docs-only PRs.
   - This is a workflow edit for its own PR, not part of the release.
4. **Leave the release-size dispatch (cumulative, Zen 3 draw) as a pre-tag checklist step** rather than a required check. It needs 50 minutes per job and a random runner CPU.
5. **Bypass list:** empty. If an emergency bypass is needed, use the ruleset's per-PR bypass with a recorded reason, not a standing admin role.

## 9. Original targets

| Item | Target | Measured on the frozen candidate vs v0.12.1 (CI, release size) | Status |
|---|---|---|---|
| 1. Two-sided ranges, DOC-11 | ≥ 10× at 10k, ≥ 50× at 100k | 6.0× x86, 5.8× arm64 at 10k; 44× x86, 39× arm64 at 100k (1.06–1.68 ms, the decode floor of 8 × `page_full`) | **Missed** (floor; decision in Stage 1A readiness §3) |
| 2. Cosine norms, VEC-2 | ≥ 35% | −40.7% x86, −35.6% arm64 | Met (arm64 by 0.6 pt) |
| 3. Sorted pages, DOC-12 asc | ≥ 20× at 10k | 41.6× x86, 46.2× arm64 (361–377× at 100k) | Met |
| 3. Sorted pages, DOC-6 desc | ≥ 20% | −35.5% to −43.8% (`stabilization/sorted_page`) | Met |
| 4. Projected scans, DOC-4 | ≥ 40% | −44.4% to −52.2% | Met |
| 4. DOC-13 count | ≥ 40% | −26.5% to −28.4% | **Missed** (error-parity floor; Stage 2A decision) |
| 4. DOC-5 | within T | x86 neutral, arm64 −10.4% | Met |
| 5. ANN rescore, HNSW-3 with a block | ≥ 15% | −7.5% x86, −9.1% arm64 (64 MiB, report-only; Stage 2B reported 13–21% arm64) | **Missed on this run** |
| 5. CONC-2 four-thread CPU | ≤ +40% over 1 thread | +45% to +122% (Stage 2B) | **Missed** (needs a shared graph node cache) |
| 6. Partial retention, VEC-2 at 8 MiB | ≥ 25% | −43.4% x86, −37.5% arm64; browser 1 MiB exact p50 −16.6% | Met; cold cost in §5 |

## 10. Release notes (CHANGELOG "Unreleased")

The CHANGELOG separates performance, fixes and behaviour changes correctly, and every compatibility statement checked out. Two numbers need changing:
- **ANN rescoring: "13–21% less time on arm64 Linux CI runners".** The release run measured 9.1% on arm64 and 7.5% on x86_64. Use the range across runs ("7–21% … depending on the run") or "about 10%".
- **Partial retention: "The query that fills the cache takes 8–21% longer, or up to 80% longer when it keeps tens of megabytes."** On a file-backed database it takes about +16% at 10k × 384 / 8 MiB and +90–100% at 50k × 384 / 64 MiB. State that an application running about one exact search per vector write on such an index is slower overall (+16% / +89%), and that the gain starts at about two searches per write.

The release notes should also say that DOC-11, DOC-13 and HNSW-3 did not reach their roadmap targets, if the roadmap targets are published. The CHANGELOG itself makes no target claims.

Missing before tagging:
- The `0.13.0` heading and date.
- The version bump (`node scripts/sync-version.js --check 0.13.0` currently fails: every manifest says 0.12.1).

## 11. Not tested, or not measured

| Platform | Built | Tests run | Performance measured |
|---|---|---|---|
| Linux x86_64 | yes | full | CI (EPYC 7763/9V74, Xeon 8573C) and local i5-9400F |
| Linux aarch64 | yes | Tier 0 + benchmarks on hardware; full suite **emulated only** | CI arm64 |
| macOS (Apple Silicon) | Node, CLI | **none** (build only); the Swift macOS/iOS job did not run against this engine | **no** |
| Windows x86_64 | Node, CLI | **none** (build only) | **no** |
| Browser | WASM | Chromium headless in CI (storage, ownership, vectors, OPFS); no Firefox or Safari | Chromium A/B only |
| iOS | **no** (xcframework built only in release.yml) | **none** | **no** |
| Android | JNI/JSI compile, 16 KB alignment (CI) | Kotlin host JVM only; no device connected, emulator does not boot here | **no** |
| React Native | native glue compile | **no runtime test** (kepta untouched) | **no** |

No performance improvement is claimed for macOS, Windows, iOS, Android, React Native or non-Chromium browsers.

## 12. Rollback

- **Data.** v0.13.0 changes no format. v0.12.1 reads databases, indexes, graphs and snapshots written by v0.13.0 (§7), so a rollback needs no data migration.
  - After a rollback, v0.12.1's old answers return for NUL-containing strings and NaN sorts, and the NaN sort panic can return.
  - Indexes written by v0.13 are the same keys, so nothing needs rebuilding.
- **Packages.**
  - crates.io: `cargo yank --version 0.13.0 taladb`.
  - npm: `npm deprecate` for each of the five packages, and move the `latest` dist-tag back to 0.12.1 (`npm dist-tag add <pkg>@0.12.1 latest`). Unpublishing is limited to 72 hours and breaks lockfiles, so prefer deprecation.
  - GitHub release: mark it as a pre-release or edit its notes. Do not delete the tag; release.yml re-runs use the scripts at the tag.
  - Kotlin and Swift wrappers: revert their engine-bump PRs. Never tag a wrapper with the engine version (`dev.taladb:taladb-android:0.12.1` is permanently on Maven Central).
- **Code.** `main` would move by merge, not force push. Revert the merge commit on `main` and on `dev`, then release 0.13.1 = 0.12.1 + fixes if needed.
- **Partial cache only.** If only the Stage 2C cost is a problem in the field, an application can avoid it without a rollback by lowering `setVectorCacheBudget`. The supported way is the §5.4 fix in 0.13.1.

## 13. Evidence

- Local evidence: `~/.cache/taladb-stage0/evidence/v013-rc/`:
  - `scripts/`: `validate_x86.sh`, `validate_arm64_qemu.sh`, `p3_cold.rs`, `p3_run.sh`, `p3.cases`, `p3_summarize.py`, `p5_compat.rs`, `p5_matrix.sh`, `p5_crash2.sh`, `wrappers.sh`, `node_ts.sh`, `build_harness.sh`, `make_results.py`
  - `logs/`, `p5/`, `api/`
  - `ci/<run-id>/`: all downloaded artifacts.
- Worktrees: `rc` (the frozen candidate, pristine), `rc-bench` (harness builds), `wrappers/taladb-{kotlin,swift}`.
