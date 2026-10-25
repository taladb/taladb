# v0.13.0 Stage 1C — merge readiness

Date: 2026-10-25. Branch: `feat/v013-sorted-page-scanning` → `dev`. Details: [V013_STAGE1C_IMPLEMENTATION_REPORT.md](V013_STAGE1C_IMPLEMENTATION_REPORT.md), [V013_STAGE1C_BENCHMARK_RESULTS.json](V013_STAGE1C_BENCHMARK_RESULTS.json).

## 0. State of `dev`

Part of this stage is already merged:

- **PR #106** (`8d53a22` + `ee907c4`) was merged before validation finished. It contains the first version: probes, the ascending walk, the first descending walk, the tests and the profile example.
- **PR #107** fixed the Clippy failure #106 caused on `dev`.

The **follow-up PR** contains the rest:

- `c622848`: the descending walk in flat queues, worth about 12 percentage points more on DOC-6;
- `a23567d`: the corrected aggregation docs (`dev` still claims no document outside the page is decoded);
- the changelog;
- these reports.

`dev` as merged returns correct results; everything below applies to it too. What it lacks is the faster descending walk and accurate documentation.

## 1. Verdict

**Correct and safe to merge.** Every requirement is met. The few limitations are inherent or pre-existing, and are listed in §2.

| Requirement | Result |
|---|---|
| Results identical to a full-scan sort, ids, order and values | Yes: 15 fixtures × 6 sorts × 120 pages against an unindexed twin, plus aggregation, projection, full paging, writes, index rebuilds, both encryptions, concurrent writes. All Criterion, Tier 0 and browser fingerprints equal on x86_64 and arm64 |
| Ties, Int/Float mix, NaN, ±0, ±∞, missing, null, arrays, objects, multi-key with conflicting directions, deep pages, empty and oversized pages | Covered by the grid; fallbacks asserted to decode every document |
| Mutation tests catch bad descending ties, invalid early stops and lost documents | Yes: 11 mutations, each failing 1–9 of the new tests (M2, M6 and M7 caught by no older suite) |
| Ascending traversal proportional to `skip + limit` | Yes, measured: 31 keys for skip 20 + limit 10 at 100k; `skip + limit + ≤ 5` in tests |
| Descending: bounded memory, no O(N) entries | Yes: O(`skip + limit`) ids, no per-key allocation. Time stays O(N) keys, without reverse iteration (§2) |
| Unbounded tie-runs held in memory | None: single-key reads a run's first ids; multi-key narrows a run in chunks of 256 (one 100k run: 108.7 MB → 0.32 MB peak) |
| No public API, C ABI, format, index encoding, sort semantics, cache default or `ReadTxn` change; no persistent metadata | Yes: new crate-private `query/sorted_index.rs`; `collection.rs` and `executor.rs` internal changes only |
| One read snapshot; error and cancellation behaviour | Yes: one read transaction for probes, walks and fetches; the fast path still requires `Filter::All` and no timeout |
| Stage 1A ranges and Stage 1B cosine unchanged | Yes: DOC-11 neutral against `dev`; vector fingerprints, recall and VEC-6 unchanged |
| Workspace, rustfmt, clippy, WASM, Tier 0, Criterion, vector CI (8 and 64 MiB), browser CI, cumulative vs v0.12.1 | All green: 805/805 tests; run 40193341830, all 17 jobs, 0 gate failures; local A/B against both baselines |
| Baselines and thresholds untouched | Yes |

## 2. Known limitations and fallbacks

- **Descending stays O(N) in keys walked.**
  - Making it O(page) needs reverse iteration on the public `ReadTxn` trait, which the brief excludes.
  - Gains come from removing per-entry allocation and copying: −37% to −46% on CI, −26% to −37% locally.
  - A deep descending page holds one 24-byte record per distinct value it keeps: 3.4 MB at skip 50,000 over 100k distinct values, against 55 MB on `dev`.
- **Fallbacks (unchanged in effect).** The query scans and sorts as before when:
  - the field is not indexed, or field-encrypted;
  - the array counter is non-zero, or missing on indexes built before counters existed;
  - a document lacks the field or holds an object;
  - the field mixes Int and Float, or holds NaN or ±0 floats;
  - a walked key is malformed.
- **Aggregation still decodes rows before `$skip`,** because the pipeline applies `$skip` itself. `find_with_options` does not decode them.
- **A corrupt key beyond an ascending page no longer forces a fallback.** `dev` decoded every key, so it noticed one; the new walk only reads the keys it uses. The engine cannot write such keys.
- **Not measured:**
  - macOS, Windows, devices and browser query timing (the browser suite is vector-only);
  - the profile's timings on arm64. Its read and allocation counts are architecture-independent.

## 3. Found during this stage, not changed

1. **NaN sort panic (pre-existing, also in v0.12.1).** `find_with_options` sorting an *unindexed* field that holds NaN can panic with "user-provided comparison function does not correctly implement a total order". `cmp_values` calls NaN equal to every number, which is not transitive, and the standard-library sort detects that since Rust 1.81. Reproduced on `dev` and on v0.12.1 with 120 documents and no limit, or with `limit: 120`. Fixing it means choosing where NaN sorts, which changes sort semantics, and that is outside this stage. **Recommendation:** a small dedicated change that orders NaN like the index does (above +∞ for positive NaN), so index and comparator agree.
2. **String keys with caller-supplied ids (pre-existing).** A shorter string's key followed by an id starting with `0xFF` can sort after a longer string that extends it with an escaped NUL (`"a"` vs `"a\0b"`). Index order then disagrees with the comparator, for the fast path and for range scans alike. Generated ULIDs cannot start with `0xFF` before the year 10889; caller-supplied ids can. **Recommendation:** document it, or disallow `0xFF`-leading caller ids if it matters.

## 4. Before merging the follow-up PR

- Its own CI must pass: `ci.yml` including `ci-pass`, and the PR-size performance gates against `dev`.
- Merge it only after reading §0: `dev` already has the first version, so the PR's diff is the flat descending window, the docs and the reports.
- Release notes: measured on Linux x86_64 and arm64 runners and one local x86_64 machine. Do not claim mobile or browser gains.
- Stage 2 was not started.
