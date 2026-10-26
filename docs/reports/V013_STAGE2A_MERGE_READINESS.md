# v0.13.0 Stage 2A — merge readiness

Engine change: PR #114 (`feat/v013-projected-scan`, `b44fa28`). Reports and changelog: the follow-up PR on `docs/v013-stage2a-reports`. Details: [V013_STAGE2A_IMPLEMENTATION_REPORT.md](V013_STAGE2A_IMPLEMENTATION_REPORT.md); data: [V013_STAGE2A_BENCHMARK_RESULTS.json](V013_STAGE2A_BENCHMARK_RESULTS.json).

## 1. State

**#114 is already merged.** It was merged into `dev` as `f71aafd` at 09:49 UTC on 2026-10-26, while the dispatched performance run was still going. Every check it was waiting for has since passed:

| Check | Run | Result |
|---|---|---|
| CI (pull request) | 40302180264 | success |
| Vector performance (pull request, PR size) | 40302180299 | success |
| Vector performance (dispatch: release size, vector + Criterion + browser + Tier 0, against `dev` 606b423 and v0.12.1 a2ef466) | 40302214957 | success, 17/17 jobs |
| CI on `dev` after the merge (`f71aafd`) | 40302535579 | success |

Run 40302178680 was cancelled by me: its baseline SHA was mistyped. It was redispatched as 40302214957 with the full SHA.

Nothing about the merged change needs to be reverted or followed up for correctness. Stage 2B has not been started.

## 2. Checklist

| Requirement | Status | Evidence |
|---|---|---|
| Projected filter, then whole decode for matches only | Done | `ScanDecoder` in `query/executor.rs` |
| Original document contents, id, field order and output order | Done | 69 Criterion fingerprints, 102 profile fingerprints and every oracle test equal |
| One read snapshot | Done | Same scan callback and bytes; `a_write_committed_mid_query_is_invisible_to_it` |
| Direct collector for `Filter::All` | Done | Separate branch, unchanged |
| Whole decoding where projection does not pay | Done | Windowed guard (§3.2 of the report); whole mode equal to `dev` |
| Deadlines, errors, early termination | Done | Same callback; corrupt-record tests; `find_one` and page tests |
| No public API, C ABI, format, index encoding, cache-default, planner, threshold or fingerprint changes | Done | Diff limited to `executor.rs`, `filter_document.rs`, one type in `vector_window.rs`, one example and one test file |
| Oracle tests across operators, paths, arrays, null versus missing, logic, regex, `$contains`, numbers, NaN, ±0, strings, bytes, large payloads, selectivity sweep, all query shapes, concurrency, encryption, malformed data | Done | 3 unit and 8 integration tests (report §4) |
| Mutation tests | Done | 10 of 10 caught |
| x86_64 and ARM64 native, browser, WASM, vector, Criterion, Tier 0 | Done | CI 40302214957; local wasm32 tests |
| Compare against PR base and v0.12.1 separately | Done | Report §5.1 and §5.2 |

## 3. For the maintainer to decide

1. **DOC-4's local result.** The roadmap's ≥ 40% is met on both CI architectures (−44% to −52%) but not on the local i5-9400F at 10k and 100k (−36% to −37%). Further gains would need byte-level field-name decoding, a second string path in the decoder, for an estimated ~10%. I did not add it; it would be a separate change.
2. **DOC-13 missed its 40% target.** Count has used projection since v0.12.1. Stage 2A makes it 20–27% faster by fixing the decoder, and what remains is UTF-8 validation, postcard walking and redb iteration, all needed to keep failing a query on a damaged record exactly as before (report §2.3). Reaching 40% would mean accepting undetected corruption in fields a filter does not read. I recommend keeping the current behaviour and recording the target as not reachable under the error contract.
3. **The first window on small collections.** With dense matches, a 1k-document scan decodes its first 32 documents twice: +1% to +5% in the allocation-counting profile, neutral in every Criterion case on CI. Starting in whole mode would remove it but delay projection for every selective scan by one window. I left it as it is.

## 4. Limitations

- macOS, Windows, devices and browser query latency are not measured. ARM64 numbers come from CI runners only.
- Indexed plans still decode every candidate whole before the residual filter (out of scope).
- The sort-hardening reports (`V013_SORT_HARDENING_*`) are not committed, so this stage builds on #110–#113's merged code only. The new tests reproduce one of their bugs: on v0.12.1, two sorted-page checks over a NaN field fail.
