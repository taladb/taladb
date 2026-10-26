# v0.13.0 Stage 2A — selective full-scan projected decoding

Roadmap item 4 ([V013_OPTIMIZATION_ROADMAP.md](V013_OPTIMIZATION_ROADMAP.md)). Engine change: PR #114 (`feat/v013-projected-scan`, head `b44fa28`), merged into `dev` as `f71aafd` on 2026-10-26. PR base `dev` `606b423` (includes #110–#113). Cumulative baseline v0.12.1 `a2ef466`. Raw data: [V013_STAGE2A_BENCHMARK_RESULTS.json](V013_STAGE2A_BENCHMARK_RESULTS.json).

## 1. Summary

- **What changed.** A filtered unindexed full scan now tests a projection holding only the top-level fields the filter reads. It decodes the whole document only for a match, with the unchanged full decoder, from the same bytes in the same read transaction. Every 32 documents a guard decides whether projection pays. When it does not, the scan runs the previous whole-document loop. The projected decoder that `count` and the ID collectors already used also got faster.
- **DOC-4 (selective `find`)** is 44–52% faster on CI against both baselines and both architectures. On the local i5-9400F the gain is 36–40%, so the roadmap's ≥ 40% is met on CI and missed locally at 10k and 100k (§5).
- **DOC-13 (`count`)** is 20–27% faster. Projected count already existed in v0.12.1, so this is not the roadmap's "≥ 40% from projection". It comes from two decoder fixes, and DOC-13 is now close to the floor this storage format and error contract allow (§2.3).
- **DOC-5 (50% matches)** is 7–13% faster, not slower. Unfiltered scans are neutral.
- **No regressions** in any CI or local Criterion case: 88 cases × 4 CI jobs and 51 cases × 2 local gates.
- **Results are unchanged.** Ids, order and every field value are identical in 69 Criterion fingerprints, 102 profile cases and the oracle tests. Errors on corrupt records are identical too.

## 2. Phase 1 — audit of the current decoder (`dev` 606b423)

### 2.1 Path by path

| Path | Reached from | Decoder before Stage 2A | Unnecessary decoding? |
|---|---|---|---|
| `execute_limited`, `FullScan`, any filter | `find`, `find_one`, `find_with_options` (sorted or not, with pushdown limit), `aggregate` starting with `$match`, watch snapshots | **Full** `postcard::from_bytes::<Document>` for every document, then filter | **Yes — the target.** Every miss allocates and copies every field |
| `execute_limited`, `FullScan`, `Filter::All` | unfiltered `find`, `aggregate` without `$match` | Full | No: every document is returned |
| `execute_into(ids_only = true)`, `FullScan` | `count` (DOC-13), `update_many`/`delete_many` candidates (`candidate_ids`) | **Projected** (`filter_document::decode`), since v0.12.1 (`bf2ab1c`) | No, but the projected decoder itself was slow (§2.2) |
| `execute_into(ids_only = false)`, index plans (`IndexEq/Range/In/And/Or`, `FtsSearch`, `ById`) | indexed `find` | Full, then post-filter | Mostly no: index candidates are mostly matches. Unchanged (indexed queries are out of scope) |
| `matching_ids_limited`, `visit_matching_ids` | filtered vector search, BM25/hybrid filters | Projected | No |
| `vector_window::Ranker` | `group_by` in vector search | Projected (`group_by` field only) | No |
| `count_plan_entries` | exact single-predicate indexed `count` | Index keys only | No |

### 2.2 Measurements (local, i5-9400F, 10k documents of the stabilization fixture: 7 fields, 1 KiB payload)

**Full decode against projected decode.** Same filter (`counter = -1`, 0% matches), `dev`:

| | Time (Criterion) | Allocations / doc | Bytes allocated / doc |
|---|---:|---:|---:|
| DOC-4 `find`, full decode | 6.96 ms | 12 | 1,764 |
| DOC-13 `count`, projected decode | 5.46 ms | 3 | 256 |

**Where the time goes** (`perf`, flat, `--time 35%-80%`, share of CPU):

| Cost | DOC-4 `dev` (full) | DOC-13 `dev` (projected) | DOC-5 `dev` (full, small docs) |
|---|---:|---:|---:|
| `Value` enum decode (full) / `SkipValue` (projected) | 20.3% | 8.5% | 4.6% |
| UTF-8 validation (`from_utf8`) | 16.8% | 23.3% | 7.3% |
| Field-vector construction | 12.3% | 12.9% (visitor) | 14.4% |
| Allocator and drops (`malloc`, `free`, `drop_glue`) | ~17% | ~4% | ~20% |
| **Field-name lookup (SipHash `HashSet<&str>`)** | — | **18.9%** | — |
| redb iteration (`get_page`, `Range::next`, guards, `scan`) | 11.9% | 15.3% | 15.4% |
| `Ulid::from_string` (id is stored as a 26-char string) | 2.1% | 2.8% | 5.6% |
| Filter evaluation (`Filter::eval`, `Document::get`, path split) | ~1.7% | ~2.1% | ~3.5% |

- **String validation.** In a micro-benchmark, `from_utf8` costs 27 ns for the 1 KiB payload and 8–10 ns per short string. A document has about nine short strings (the id, seven field names, `body`), so per-call overhead, not the payload, is most of the 17–23%. A skipped string must still be validated (§3.4).
- **Skipped fields** still cost the enum tag, the varint, and validation of every string inside them. They cost no allocation, copy or drop.
- **Full-scan iteration** (redb) is ~80 ns per document, about 12% of DOC-4 before the change and 18% after.
- **Filter evaluation** is 2–4%: not worth optimising.
- **Match selectivity.**
  - DOC-4 and DOC-13: 0% matches.
  - DOC-5: 50%, but in one contiguous half (`n >= 5000` with `n` in insertion order) on two-field documents.
  - `query/equality`: 1% unindexed on the same small documents.
- **The existing projected count path.** In v0.12.1, count with projection was only 17% cheaper than find with full decode (5.69 against 6.88 ms). The SipHash lookup of every field name of every document ate most of what projection saved. That lookup, plus a heap-allocated id string, is what Stage 2A removes for count.

### 2.3 Is DOC-13 at the floor?

Before Stage 2A it was not: about 19% of its CPU was hashing and 1 of 3 allocations per document was avoidable. After the change, `perf` on the merged head (`b44fa28`) attributes:

- UTF-8 validation: 31.3%;
- postcard walking (the decode visitor, `SkipValue`, varints): 29%;
- redb iteration: ~18.5%;
- the ULID parse: 3.6%;
- allocator: ~3%;
- name comparison (`memcmp`): 2.9%.

Everything except the last two is required as long as the storage format is postcard and a corrupt record must fail the query exactly as before (§3.4). The 40% roadmap target for count is not reachable without skipping validation of skipped strings, which would change error behaviour on damaged data. Two levers were evaluated and dropped for measured cause:
- **Reusing a scratch document** across rows would save at most ~3%.
- **An ASCII fast check before `from_utf8`** wins nothing on 1 KiB strings (26.2 against 26.8 ns). It saves ~6 ns per short string only if field names were decoded as bytes, an estimated ~10% at the price of a second string-handling path in the decoder. Not pursued: CI already meets the DOC-4 target.

## 3. Phase 2 — implementation

Files: `query/executor.rs`, `query/filter_document.rs`, `vector_window.rs` (one type change), plus `examples/scan_profile.rs` and `tests/projected_scan.rs`. No public API, C ABI, storage format, index encoding, cache default, planner output or benchmark threshold changed.

### 3.1 Selective find (`execute_limited`, `FullScan`, filter ≠ `All`)

For each document in the existing `txn.scan` callback:

1. **Projected mode.** `filter_document::decode` keeps only the fields the filter reads: the head of each path, so `meta.tag` keeps `meta`, and every child of `$and`, `$or` and `$not`. The compiled `Matcher` tests that projection. A match is decoded again with `postcard::from_bytes(bytes)`, the decoder the scan always used, from the same borrowed bytes.
2. **Whole mode.** The previous loop body, byte for byte, inlined into the same closure.
3. **Window bookkeeping.** Every 32 documents (`DECODE_WINDOW`), `ScanDecoder::tick` chooses the mode for the next window.

Order, limit pushdown, deadline checks, `ScanFlow::Stop` and the single read transaction are unchanged, because the callback is the same one. `Filter::All` keeps its own loop.

### 3.2 The guard

Projection saves the allocation, copy and drop of every field a miss would have built. It costs a second decode per match. The rule: keep projecting while

    decayed match rate × 3 < share of encoded bytes the projection skipped

- **Match rate.** Counted in every window, decayed by 1/8 per window (`rate = rate − rate/8 + window`), so the memory is about 8 windows.
- **Skipped share.** Measured in the last projected window: `1 − decoded_len / bytes.len()`. `decoded_len` is an upper-bound estimate of the id and kept fields' encoded size.

| Shape | Skipped share | Projects while matches < |
|---|---:|---:|
| Wide documents (1 KiB payload) | ≈ 0.97 | ≈ 32% |
| Two-field documents (`n`, `bucket`) | ≈ 0.30 | ≈ 10% |
| A filter reading every field | 0 | never (whole mode after the first window) |

The measured break-even on wide documents was ~38–40% matches; the factor 3 keeps a margin.

**How the guard was arrived at:**

| Version | Rule | Problem measured (`scan_profile`, 10k, vs `dev`) |
|---|---|---|
| 1 | last window only, factor 2 | `bucket < 50` (alternating runs of 50 matches and 50 misses): +13.6% on small documents. Projected windows kept landing on runs of matches |
| 2 | decayed rate, factor 3 | Fixed the oscillation, but whole mode ran +2.6% (wide) to +6.9% (small) slower than `dev`: `perf stat` showed ~95 extra instructions per document from returning `Result<Option<Document>, TalaDbError>` through a helper |
| 3 (merged) | same rule; whole-mode branch inlined as the original loop body | whole mode equal to `dev` (cycles within noise, +32 instructions/doc for bookkeeping) |

The JCC-erratum control (`-x86-branches-within-32B-boundaries` on both sides) reproduced version 2's overhead, so it was code, not layout.

### 3.3 Projected decoder (shared with `count`, the ID collectors and vector filters)

- **Field names.** A `HashSet<&str>` became a list sorted by length and then bytes, searched with `binary_search_by`. Most comparisons end at the length check. Two steps were measured on wide documents:
  - SipHash `HashSet` to plain lexicographic binary search: ~19% of DOC-13's CPU removed;
  - lexicographic to length-first: another 4–7% on wide projected cases, 2% on small ones.
- **Document id.** `ulid`'s `Deserialize` goes through `String::deserialize`. The projection now borrows the `&str` and calls `Ulid::from_string(..).map_err(de::Error::custom)`. It accepts and rejects exactly the same inputs and saves one allocation per document.

### 3.4 Errors and corrupt records

The full decoder is the reference. If a projected decode fails, the scan decodes the record whole: it returns that decoder's error, or that decoder's document if it accepts what the projection rejected. That cannot happen, and the tests check that it does not. The opposite case would be a lenient projection: one that accepts bytes the full decoder rejects, so a non-matching corrupt record no longer fails the query. By construction that cannot happen either:
- every skipped string is still validated as UTF-8;
- every tag, varint, bool and length is still read with postcard's own primitives.

The unit test checks it on every truncation and every single-byte corruption of a document, for all 86 filters.

### 3.5 Matching documents decoded twice

In projected mode, a match is decoded twice. Avoiding that would need byte offsets per field from inside a serde visitor, which postcard does not expose, or a second, unchecked string path. The guard bounds the cost instead: projected windows by construction hold few matches. Measured costs:
- At 10% matches on wide documents the find is still 39–41% faster than `dev`.
- When matches are dense, the scan pays the double decode only until the guard switches modes, mostly the first 32-document window (§5).

## 4. Phase 3 — correctness

**Oracles.** In the unit tests, the full decode plus the same matcher. In the integration tests:
- `find(Filter::All)` (the untouched whole-document loop) with `Filter::matches`, which shares the scan's evaluator;
- for corrupt records, a re-implementation of the old scan over the raw stored bytes;
- for field encryption, a second collection handle that does not decrypt.

Results are compared as encoded bytes, so NaN fields compare and every field value is checked.

| Test | What it covers |
|---|---|
| `executor::tests::projected_decision_and_document_match_the_full_decode_for_every_filter` | 86 filters (every operator; Int/Float/NaN/±0/string/null operands; nested paths; arrays; `$and`/`$or`/`$not`; regex; `$contains` including empty and missing fields; `_id` operators) × 120 documents (missing versus null, nested objects, arrays and nested arrays, bytes, NUL strings, 70 KB payloads). The projected decision and document must equal the full decode; 63 of the 86 filters split the corpus |
| `executor::tests::corrupt_encodings_fail_or_succeed_exactly_as_the_full_decode_does` | Every truncation, 9 byte values at every position, and trailing bytes (~9,000 variants) × 86 filters. The find path must return the same document or the same error string; the count decoder must fail exactly when the full decoder does |
| `executor::tests::decoder_projects_selective_scans_and_decodes_whole_when_matches_are_dense` | Guard transitions: no matches → projects; all match → whole; a filter reading every field → whole; matches stop → back to projecting within 32 windows |
| `projected_scan::every_filter_shape_matches_the_full_scan` | 90 filter shapes (the unit test's, plus `$ne`/`$nin` on null, bytes and arrays) and the parser-parity fixture's 5 valid filters, over 600 documents, through `find`, `count`, `find_one`, a page (skip 3, limit 5) and a sorted page (`n` desc, `i` asc, skip 2, limit 7) |
| `projected_scan::selectivity_and_distribution_sweep_matches_the_full_scan` | 0/1/10/50/100% matches, distributed periodically, at the start, at the end and scattered, on wide and narrow documents (3,000 each), all five query shapes |
| `projected_scan::invalid_regex_is_an_error_never_an_empty_or_full_result` | Invalid regex direct, under `$not`, and under `$and` + `$not` → `InvalidFilter` from `find`, `find_one`, `count` and a limited `find_with_options` |
| `projected_scan::malformed_records_fail_exactly_as_the_full_scan_does` | A stored record truncated 8 ways, or given an invalid ULID character, invalid UTF-8 in the id, in a field name or in a skipped value, an invalid `Value` tag, a bad bool, or trailing bytes. `find`, `find_one` and a limit-60 page must equal the old scan (documents or error string); `count` fails exactly when the full decode does |
| `projected_scan::a_write_committed_mid_query_is_invisible_to_it` | Deterministic: the read transaction is held open while `update_many` commits. `find`, `find_one` and `count` see only the old snapshot, and the next query sees only the new one |
| `projected_scan::a_filtered_scan_reads_one_snapshot_under_concurrent_writes` | A writer flips every document's phase and generation 60 times while 2 readers check that each result is all-or-nothing, of one generation, and consistent with the filter |
| `projected_scan::encrypted::field_encryption_filters_the_stored_values_then_decrypts` | Filters on plaintext fields and on encrypted fields (ciphertext semantics: `$exists`, `$eq`, `$ne`) |
| `projected_scan::encrypted::encrypted_database_matches_the_full_scan` | `EncryptedBackend` with a sample of the shapes |

- **On the baselines.** All 8 integration tests pass unchanged on `dev` 606b423, where every filtered scan is a full decode. On v0.12.1, 6 of 8 pass; the 2 failures are the sorted-page checks over a NaN field, where v0.12.1 panics or orders inconsistently (the bug #110 fixed).
- **Workspace.** 62 suites, 844 passed, 0 failed, 3 ignored, all features. `taladb-web` on wasm32: 22 tests pass under `wasm-bindgen-test-runner` 0.2.114. rustfmt, workspace clippy (all targets, all features), wasm32 clippy with and without default features, and the MSRV 1.90 check are clean.

**Mutations.** Each was applied to the committed tree, the suites run, then reverted (`mutate.py`). Entries are failed / total tests per suite.

| Mutation | lib | `projected_scan` | `query_engine` | `regex_failure_is_an_error` |
|---|---|---|---|---|
| M1 nested path projects the whole path, not its head | 3/155 | 1/6 | 6/67 | 0/8 |
| M2 `$not` children not projected | 2/155 | 1/6 | **0/67** | 2/8 |
| M3 `$and`/`$or` project their first child only | 3/155 | 1/6 | 2/67 | 0/8 |
| M4 missing projected field decoded as null | 2/155 | 1/6 | **0/67** | 0/8 |
| M5 projected decode error swallowed as a miss | 1/155 | 1/6 | **0/67** | 0/8 |
| M6 a match returns the projection, not the whole document | 2/155 | 5/6 | 5/67 | 0/8 |
| M7 length-ordered list searched lexicographically | 3/155 | 1/6 | 1/67 | 0/8 |
| M8 invalid id accepted by the projection | 1/155 | 1/6 | **0/67** | 0/8 |
| M9 regex fields not projected | 2/155 | 2/6 | 4/67 | 2/8 |
| M10 invalid regex in a selective find returns no rows | 0/155 | 1/6 | 1/67 | 3/8 |

All 10 are caught by `projected_scan`, and 9 by the new unit tests. `query_engine` alone would have missed four (M2, M4, M5, M8). `stabilization` caught none. The integration suite was run without `--features encryption` (6 tests); the 2 encryption tests are extra.

## 5. Phase 4 — benchmarks

### 5.1 CI, run 40302214957

Head `b44fa28`, release size, all suites. Three interleaved pairs per job. x86_64 ran on an AMD EPYC 7763 (PR gate) and an Intel Xeon 6973P (cumulative); arm64 on `ubuntu-24.04-arm`.

| Case | x86_64 vs `dev` | arm64 vs `dev` | x86_64 vs v0.12.1 | arm64 vs v0.12.1 |
|---|---:|---:|---:|---:|
| **DOC-4** `stabilization/unindexed_selective/1000` | 848 → 436 µs (−48.7%) | 819 → 394 µs (−51.9%) | 483 → 250 µs (−48.2%) | 812 → 394 µs (−51.5%) |
| **DOC-4** `stabilization/unindexed_selective/10000` | 8.59 → 4.53 ms (−47.3%) | 8.23 → 4.09 ms (−50.3%) | 5.00 → 2.79 ms (−44.1%) | 8.59 → 4.29 ms (−50.0%) |
| **DOC-13** `doc/count_unindexed/1000` | −23.6% | −25.2% | −23.7% | −25.1% |
| **DOC-13** `doc/count_unindexed/10000` | −26.7% | −26.9% | −21.4% | −21.4% |
| **DOC-13** `doc/count_unindexed/100000` | −21.5% | −20.9% | −19.9% | −21.0% |
| **DOC-5** `query/range_scan_unindexed` | −12.7% | −10.6% | −6.6% (neutral) | −10.3% |
| `query/equality` (1% unindexed, small documents) | −19.2% | −25.8% | −17.9% | −25.2% |
| `query/range_scan_mixed_numeric` | −12.3% (neutral) | −10.4% | −7.5% (neutral) | −10.2% |
| DOC-8 `doc/count_indexed_range` (post-filter decoder) | −11 to −16% | −12 to −13% | −12 to −14% | −10 to −14% |
| Unfiltered and indexed (`page_full`, `indexed_eq`, `indexed_page`, `sorted_page*`, `find_by_id`, `range_window`, `update_many`) | neutral | neutral | Stage 1 gains only | Stage 1 gains only |

- **Criterion:** 4 jobs × 88 cases. 0 regressions, 0 failures, 0 fingerprint mismatches. Improvements over `dev`: 9 cases on x86_64 and 10 on arm64, all scan or count cases above. Against v0.12.1 the improvements also include the Stage 1 cases.
- **Vector:** 32 comparisons (HNSW 8 and 64 MiB, resumable, filtered, concurrency) pass.
- **Browser:** worker + OPFS passes at 1 and 8 MiB against both baselines.
- **Tier 0:** fingerprints, codegen and MEM-2 pass on both runners. MEM-2 `update` and `delete` mutations allocate 1.4% less (71,595 → 70,595), from the projected candidate decoder.

The CI Criterion suite runs `stabilization/` at 1k and 10k only. DOC-4 at 100k was measured locally.

### 5.2 Local Criterion

i5-9400F, `powersave` governor, pinned to core 5, 3 interleaved pairs, `local-i5-9400f` tolerances, 51 cases (`doc/`, `query/` and the relevant `stabilization/` cases at 1k, 10k and 100k).

| Case | vs `dev` | vs v0.12.1 |
|---|---:|---:|
| DOC-4 1k / 10k / 100k | −39.6% / −37.1% / −35.9% | −40.1% / −36.7% / −36.1% |
| DOC-13 1k / 10k / 100k | −24.2% / −21.4% / −21.7% | −28.0% / −25.1% / −24.2% |
| DOC-5 | −4.8% (neutral) | −6.0% (neutral) |
| `query/equality` | −12.5% | −13.2% |
| DOC-8 `count_indexed_range` 1k / 10k / 100k | −13.4% / −11.5% / −10.9% | −16.1% / −14.6% / −12.5% |

0 regressions and 69/69 fingerprints equal in both gates.

### 5.3 Per-request profile (`examples/scan_profile.rs`)

Interleaved v0.12.1 / `dev` / head, medians of 3 rounds (1 round at 100k), pinned. This harness counts every allocation through a global allocator, which slows allocation-heavy code, so its latency gains run above Criterion's. Use it for allocations and for comparing selectivities.

**Wide fixture** (1 KiB payload, only `n` indexed; `bucket < k` matches k%), 10k documents. "Allocs/doc" counts per document scanned.

| Case | `dev` | Head | vs `dev` | Allocs/doc `dev` → head |
|---|---:|---:|---:|---:|
| `find`, no match | 9.56 ms | 4.68 ms | −51% | 12 → 2 |
| `find` 1% | 9.73 ms | 4.84 ms | −50% | 12 → 2.1 |
| `find` 10% | 10.06 ms | 5.96 ms | −41% | 12 → 3.2 |
| `find` 50% | 11.69 ms | 11.84 ms | +1.3% | 12 → 12 |
| `find` 100% | 14.05 ms | 14.28 ms | +1.6% | 12 → 12 |
| `find(All)` | 13.81 ms | 13.89 ms | +0.6% | 12 → 12 |
| `count` (any selectivity) | 6.12–6.16 ms | 4.59–4.68 ms | −24 to −25% | 3 → 2 |
| `find_one` (match at doc 100) | 102 µs | 44 µs | −56% | |
| page, 1% filter, skip 5 limit 10 | 1.39 ms | 0.63 ms | −55% | |
| sorted page, 10% filter, limit 10 | 10.16 ms | 6.10 ms | −40% | |

**Narrow fixture** (`{ n, bucket }`), 10k documents:

| Case | vs `dev` |
|---|---:|
| `find` 0–1% | −27 to −28% |
| `find` 10%, 50%, 100%, `find(All)` | −0.1% to +1.0% |
| `count` | −22 to −23% |
| page, 1% | −32% |

At 100k the results are within a point of these. Peak live heap is unchanged except where fewer documents are retained.

## 6. Targets

| Roadmap target | Result |
|---|---|
| DOC-4 ≥ 40% faster | **Met on CI** on both architectures against both baselines (−44.1% to −51.9%). **Missed locally** at 10k and 100k (−36% to −37%; −40% at 1k): the i5-9400F spends a larger share on UTF-8 validation and redb iteration, which projection cannot remove |
| DOC-13 ≥ 40% faster | **Missed** (−20% to −27%). Projected count predates Stage 2A; the gain here is the decoder fix only, and the remaining cost is required work (§2.3) |
| DOC-5 within *T* | Met: 4.8–12.7% faster |
| No significant regression to unfiltered scans | Met: `find(All)` and every unfiltered or indexed Criterion case neutral |

## 7. Known costs and limitations

- **Small collections with dense matches.** The guard decides after a 32-document window, so a scan whose documents almost all match decodes the first window twice. In the profile harness at 1k documents: +4.8% for wide documents at 100% matches, +1.4% at 50%, +2–3% for narrow documents. At 10k and above the effect is under 2%. Every 1k Criterion case was neutral on CI.
- **Matches are decoded twice** while the guard projects (§3.5).
- **Indexed plans still decode whole candidates** before the residual filter. They are out of scope, and their candidates are mostly matches.
- **Platforms not measured:** macOS, Windows, devices, and query latency in the browser (the browser suite covers vectors only). ARM64 numbers come from CI runners only.

## 8. Reproducing

- **Profile:** `cargo run --release -p taladb --example scan_profile -- 10000 [wide|small] [case]`.
- **Local Criterion:** `scripts/vector-bench-ci.py BASELINE . --workload criterion --gate-kind pr|cumulative --criterion-groups doc,stabilization,query --criterion-sizes 1000,10000,100000 --criterion-filter '^(doc/|query/|stabilization/(unindexed_selective|indexed_page|sorted_page|update_many|bm25_selective)/)' --repeats 3 --tolerances-profile local-i5-9400f --pin 5`.
- **CI:** dispatch `performance.yml` with `baseline=606b4235dc8598c7cc9e3c9341252bca8ed6093e`, `cumulative=true`, `size=release`, `suites=vector,criterion,browser,tier0`.
