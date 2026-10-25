# v0.13.0 Stage 1C — sorted pages without materialising the index: implementation report

Date: 2026-10-25. Branch: `feat/v013-sorted-page-scanning`, from `dev` at `ab886fd` (after Stage 1B, PRs #104 and #105). Performance baseline: v0.12.1, `a2ef46602127919c60a0fe6333d676b1b1ab391e`.
Companions: [V013_STAGE1C_BENCHMARK_RESULTS.json](V013_STAGE1C_BENCHMARK_RESULTS.json), [V013_STAGE1C_MERGE_READINESS.md](V013_STAGE1C_MERGE_READINESS.md); roadmap item 3 in [V013_OPTIMIZATION_ROADMAP.md](V013_OPTIMIZATION_ROADMAP.md).

| Commit | Content | In `dev` |
|---|---|---|
| `8d53a22` | The change: `query/sorted_index.rs`, integration in `collection.rs`, removal of `index_ordered_entries`; `tests/sorted_page_index.rs` | yes (PR #106) |
| `ee907c4` | `examples/sorted_page_profile.rs`: per-request latency, keys walked, documents decoded, heap | yes (PR #106) |
| `40fe920` | Clippy fixes in the new test | yes, as `55d6df1` (PR #107) |
| `a23567d` | `docs/api/aggregation.md`: what a sorted page reads, and when the index is not used | yes (PR #108) |
| `c622848` | Descending walk in flat queues (from profiling DOC-6) | yes (PR #108) |
| `9cb41df` | Changelog | no |
| this commit | Stage 1C reports | no |

PR #106 merged the first two commits before validation had finished. Its test file failed `dev`'s workspace clippy step (run 40189769033); PR #107 fixed that. PR #108 merged the flat descending window and the docs. `dev` at `15aa8fe` (CI green) now has an engine tree byte-identical to `c622848`, the head every measurement below was taken on. Only the changelog and these reports remain, in their own PR.

## 1. Summary

A sorted page on an indexed field no longer copies the whole index into memory. Two callers use it: `find_with_options` with `sort` and `limit`, and `aggregate` starting with a bounded `$sort`. The engine now proves with a constant number of index reads that index order is sort order, then walks only what the page needs.

- **Results are unchanged.** A new test compares every page with an unindexed twin collection holding the same documents under the same ids: 7 eligible and 8 fallback fixtures × 6 sort specifications × 120 pages, plus aggregation pipelines, projection, a page-by-page walk of the whole collection, writes, index rebuilds, both kinds of encryption and concurrent writes. Every Criterion, Tier 0 and browser fingerprint is unchanged. Each of 11 mutations fails the new tests: wrong tie order, invalid early stop, lost documents, dropped runs, a mixed-type probe removed.
- **Ascending pages stop at the page.**
  - Keys walked: `skip + limit` plus at most one key per eligibility probe. A page of 10 after skipping 20 reads 31 keys at any collection size, where `dev` read every key.
  - DOC-12 (`doc/sorted_page_asc`) against `dev` on CI: 6.4× / 40× / 379× on x86_64 and 6.7× / 46× / 383× on arm64, at 1k / 10k / 100k.
  - Locally: 7.8× / 52× / 551× (136 → 17.4 µs, 936 → 18.0 µs, 10.3 ms → 18.6 µs).
- **Descending pages still walk every key, with memory bounded.** The internal read API iterates forward only, and the brief rules out adding reverse iteration to `ReadTxn`. Only the trailing runs that can reach the page are kept, and nothing is allocated per key.
  - DOC-6 (`stabilization/sorted_page`) against `dev` on CI: −46% / −37% on x86_64 and −44% / −38% on arm64, at 1k / 10k.
  - Locally: −37% / −26% / −31% at 1k / 10k / 100k.
- **Memory is O(`skip + limit`).** That holds for every eligible plan, including a run of ties spanning the collection.
  - Peak live heap for the DOC-6/DOC-12 shape at 100k: 7.2 MB → 13 KB.
  - Collection-wide tie-run with a multi-key sort: 108.7 MB → 0.3 MB.
  - `find_with_options` no longer decodes documents wholly before `skip`: 10 documents for a page at skip 50,000, where `dev` decoded 50,010.
- **No public API, C ABI, storage-format, index-encoding, sort-semantics or default change.** No persistent metadata was added. `ReadTxn` is untouched.

## 2. What changed

### 2.1 Before

`sorted_index_candidates` did four things in turn:

1. copied every index entry into a `Vec<IndexEntry>`, with one `Vec<u8>` per entry;
2. compared the entry count with the documents table;
3. scanned all entries for mixed Int/Float, NaN and ±0;
4. reversed the vector for a descending sort and kept the page plus the tie-run at its boundary.

The caller then decoded every kept document, including those before `skip`, and sorted them with the full specification. That cost O(N) time and O(N) memory for a page of any size.

### 2.2 Eligibility in a constant number of reads (`index_order_is_sort_order`)

The checks that stay unchanged:

- the field has an index;
- no field encryption;
- the array counter in `meta::index_arrays` is `Some(0)`.

The checks that replace the full walk:

| Check | Before | Now |
|---|---|---|
| Every document has an index entry | Count all entries | `len()` of the index table and of the documents table. redb 4 keeps a table's length in its B-tree header (`Btree::len` reads `root.length`), so this is O(1). Each document has at most one key because the array counter is zero |
| No field mixes Int and Float | Check every entry's type tag | Two range probes that stop at the first key: any key in `[TAG_INT, TAG_INT+1)`, any in `[TAG_FLOAT, TAG_FLOAT+1)` |
| No NaN or ±0 (comparator calls them equal; keys differ) | Decode every float | If floats exist, three probes over the key ranges that hold exactly those values: negative NaNs (below −∞), `−0.0..=+0.0` (adjacent keys), positive NaNs (above +∞). A unit test checks the ranges against 10,000 random bit patterns and every edge value |

At most five probes, each reading at most one key. Whole-database encryption leaves keys in the clear and passes `count_entries` through, so the probes hold there; the test covers it.

### 2.3 The walks (`query/sorted_index.rs`)

How index order relates to the comparator (`cmp_rows`):

- Runs of equal value prefixes are ordered by value.
- Inside a run, a single-key sort orders by `_id` **ascending in both directions**: `cmp_rows` breaks ties on the id without reversing it. That is the run's own key order.
- Inside a run, a multi-key sort orders by the later keys, which only the documents can tell.

| Plan | Walk | Memory held | Documents decoded |
|---|---|---|---|
| Ascending, single key | Forward; stores ids of rank `skip..keep`; stops at rank `keep` | `limit` ids | `limit` |
| Ascending, multi-key | Forward, grouped into runs; stops at rank `keep` | `keep` ids | Runs overlapping `skip..keep`; runs wholly before `skip` are not decoded |
| Descending, single key | Every key, through a window of the trailing runs | ≤ `keep` ids plus one 24-byte record per held run | `limit` |
| Descending, multi-key | Same window, also keeping held runs' prefixes | as above, plus prefixes | Runs overlapping `skip..keep` |

**Descending window.** A run is retired once the runs after it hold `keep` rows. Then either one run is held, or every held run but the lowest lies wholly on the page and the lowest is the boundary. A run holds at most `keep` ids, its first ones, which are the ones the `_id` tie-break ranks first. Ids, run records and prefixes live in three flat queues, so a key that starts a run and retires another costs a few pushes and pops.

The first version (`8d53a22`) held runs as structs with two vectors each. Profiling DOC-6 showed that bookkeeping at 31.7% of the query, ahead of redb's iterator (24.0%); `c622848` replaced it. With the flat queues, DOC-6 at 10k went from −14% to −26% against `dev`.

**A boundary run that must be ordered by later keys** (multi-key, and the run is only partly needed or longer than what was held) is read again by key range in chunks of 256. Each chunk is decoded and narrowed to the best `need` documents with `partial_sort_documents` over the full specification. That holds at most `need + 256` documents, however long the run. The range `prefix ++ 00…00 ..= prefix ++ FF…FF` can contain keys of another value: a string with an escaped NUL encodes as an extension of a shorter string's prefix. Such keys are skipped by exact prefix comparison.

**Integration.**

- `sorted_index_candidates` takes the full sort specification and `skip`, and returns `SortedPage { docs, skipped }`.
- `find_with_options` then orders `keep − skipped` documents and drops the remaining `skip − skipped`.
- `aggregate` passes `skip = 0`, because `$skip` runs inside the pipeline.
- Both run every read on one read transaction.
- The fast path still runs only with `Filter::All` and no timeout, so error and cancellation behaviour are unchanged.

### 2.4 Fallback conditions (unchanged in effect)

The index is not used, and the query scans and sorts as before, when:

- the field is not indexed, or field-encrypted;
- the array counter is missing (an index built before counters existed) or non-zero;
- some documents lack the field (or hold an unindexable object);
- the field mixes Int and Float;
- it holds NaN, −0.0 or +0.0 floats;
- an index key visited by a walk is malformed (shorter than its 16-byte id).

The test `ineligible_fields_take_the_scan_path` asserts that the missing, mixed, NaN and zero cases decode every document.

### 2.5 What the fast path no longer checks

`dev` decoded every key, so a corrupt key anywhere — a float key whose value is not exactly 8 bytes, or a key shorter than 16 bytes — made it fall back. An ascending walk now reads only the keys it uses, so a corrupt key beyond the page goes unseen. Keys it does read are still checked. Such keys cannot be written by the engine; this matters only for storage corruption.

## 3. Correctness

`packages/core/tests/sorted_page_index.rs`. Each query runs on an indexed collection and on an unindexed twin, and compares ids, order and every returned value. Floats are compared by bit pattern, and `_changed_at` is excluded because it is written per collection.

| Test | Coverage |
|---|---|
| `eligible_fixtures_match_a_full_sort_on_every_page` | Unique ints; all equal (one run of 300); runs of 50; runs of 1–24; floats with ±∞ and subnormals; strings with NUL, prefixes, empty and non-ASCII; nulls and bools. × 6 sort specs (asc, desc, asc+desc, desc+asc, desc+desc+asc, asc+asc) × 15 skips × 8 limits (0 to `u64::MAX`), across run boundaries, past the end and empty |
| `fallback_fixtures_match_a_full_sort_on_every_page` | Mixed Int/Float, NaN, negative NaN, signed zero, +0.0 only, missing field, array value, object value; same grid |
| `aggregation_pipelines_match_a_full_sort` | `[$sort, $limit]`, `[$sort, $skip, $limit]`, with `$project`, and `[$sort, $limit, $skip]`, on eligible and mixed fields |
| `projection_and_exclusion_apply_to_index_served_pages` | `fields` and `exclude` |
| `paging_through_the_whole_collection_returns_every_document_once_in_order` | 503 documents in pages of 7, every sort: each document once, in order |
| `writes_between_snapshots_and_index_rebuilds_are_seen` | Updates across runs, inserts, `delete_many`, a field unset then restored, index dropped and rebuilt; the full grid after each |
| `encrypted_fields_and_encrypted_databases_match_a_full_sort` | Field encryption on the sort key (fallback) and whole-database encryption (fast path) |
| `concurrent_writes_never_produce_an_inconsistent_page` | 3 readers during 300 updates: every page full, no duplicate, in comparator order |
| `ascending_walks_stop_at_the_page_and_decode_only_the_page` | 10k: ≤ 35 keys and 10 documents for skip 20; ≤ 5,015 keys and 10 documents for skip 5,000 |
| `descending_walks_every_key_but_decodes_only_the_page` | 10k: every key, 10 documents |
| `single_key_ties_decode_only_the_page_however_long_the_run` | One run of 5,000, skip 2,000: 10 documents in both directions |
| `multi_key_pages_decode_the_runs_they_touch_and_no_more` | Runs of 100: a page inside one run decodes 100; across two, 200 |
| `ineligible_fields_take_the_scan_path` | Missing, mixed, NaN, zero: every document decoded |

The unit test `probe_ranges_hold_exactly_nan_and_signed_zero` checks the probe ranges against the encoder.

**Oracle and NaN.** On NaN data the scan-and-sort path itself can panic, as described in §6. Each page's outcome is therefore compared, panic included: both collections run the same scan and sort over the same input and must behave alike.

**Mutation checks.** Each mutation was applied, the four sorted-path suites run, and the mutation reverted. Counts are failing tests out of each suite's total.

| Mutation | New suite (13) | `aggregate_sorted_index` (11) | `stabilization` (5) |
|---|---:|---:|---:|
| M1 descending boundary run keeps its last ids (first version) | 7 | 2 | 0 |
| M1b same, on the flat window | 7 | 2 | 0 |
| M9 ids inside a descending run ranked in reverse | 7 | 1 | 1 |
| M2 ascending boundary run treated as complete (invalid early stop) | 7 | 0 | 1 |
| M3 ascending walk stops one key early | 9 | 2 | 1 |
| M4 completeness check removed (lost documents) | 3 | 1 | 0 |
| M5 mixed Int/Float probe removed | 1 | 1 | 0 |
| M6 / M6b descending trims one run too many | 4 / 4 | 0 / 0 | 1 / 1 |
| M7 run straddling `skip` counted as skipped | 7 | 0 | 1 |
| M8 multi-key boundary run not narrowed by later keys | 7 | 1 | 1 |

`query_engine` (67 tests) caught none of them. M2, M6 and M7 are caught only by the new suite and `stabilization`.

**Results** at `c622848`:

- **Workspace:** 58 suites, **805 passed**, 0 failed, 3 ignored (791 before, plus 13 integration tests and 1 unit test). No existing assertion changed.
- **Lints:** rustfmt and clippy clean (workspace, all targets, all features; `taladb-web` on wasm32 with and without default features).
- **WASM:** `taladb-web` builds for wasm32 (release), and its 22 tests pass under Node with `wasm-bindgen-test-runner` 0.2.114.

## 4. Performance

### 4.1 Local Criterion (i5-9400F, pinned core 5, 3 interleaved pairs; final head)

| Case | `dev` → candidate | vs `dev` | v0.12.1 → candidate | vs v0.12.1 |
|---|---:|---:|---:|---:|
| DOC-12 `doc/sorted_page_asc/1000` | 136.1 → 17.4 µs | 7.8× | 136.1 → 17.2 µs | 7.9× |
| DOC-12 `doc/sorted_page_asc/10000` | 935.7 → 18.0 µs | 52.0× | 953.8 → 18.0 µs | 53.0× |
| DOC-12 `doc/sorted_page_asc/100000` | 10,276 → 18.6 µs | 551× | 9,946 → 18.6 µs | 536× |
| DOC-6 `stabilization/sorted_page/1000` | 134.3 → 85.1 µs | −36.6% | 136.3 → 85.4 µs | −37.4% |
| DOC-6 `stabilization/sorted_page/10000` | 945.9 → 703.0 µs | −25.7% | 957.3 → 706.5 µs | −26.2% |
| DOC-6 `stabilization/sorted_page/100000` | 10,469 → 7,255 µs | −30.7% | 9,633 → 7,162 µs | −25.7% |

All 39 measured cases per gate (DOC-1/2/3/7/8/9/10/11/13 at 1k, 10k, 100k) are neutral or improved: no regression, no failure, 66/66 fingerprints equal against both baselines. Stage 1A's DOC-11 (`doc/range_window`) is neutral. `doc/aggregate_group/100000` is 0.94 against `dev` and 0.95 against v0.12.1.

The first version (`8d53a22`) measured DOC-6 at −27% / −14% / −13%. That run is kept as `v1-*` in the evidence.

### 4.2 Per-request profile (`sorted_page_profile`, local, 5 interleaved rounds, final head)

**Fixture.** Unique integers `n`, runs of 100 `g`, one value `t`, a mixed-type `x` and a 512-byte payload, all sort fields indexed. Fingerprints of every page are identical across v0.12.1, `dev` and the candidate in every case at every size. The read and allocation counts are identical in every round.

| 100k documents | `dev` p50 | Candidate p50 | Keys walked | Docs decoded | Bytes allocated | Peak live heap |
|---|---:|---:|---|---|---|---|
| asc, skip 20, limit 10 | 13.1 ms | 20.8 µs | 100,000 → 31 | 30 → 10 | 13.5 MB → 20 KB | 7.19 MB → 12.5 KB |
| desc, skip 20, limit 10 | 13.7 ms | 7.94 ms | 100,000 → 100,001 | 30 → 10 | 13.5 MB → 23 KB | 7.19 MB → 12.8 KB |
| asc, skip 50,000 | 132.5 ms | 3.16 ms | 100,000 → 50,011 | 50,010 → 10 | 104.9 MB → 20 KB | 55.2 MB → 12.5 KB |
| desc, skip 50,000 | 134.3 ms | 9.29 ms | 100,000 → 100,001 | 50,010 → 10 | 104.9 MB → 6.1 MB | 55.2 MB → 3.4 MB |
| asc, first row | 14.0 ms | 6.9 µs | 100,000 → 2 | 1 → 1 | 13.5 MB → 4 KB | 7.19 MB → 3 KB |
| multi-key asc (`g` asc, `m` desc), skip 20 | 13.8 ms | 169 µs | 100,000 → 131 | 100 → 100 | 13.7 MB → 196 KB | 7.19 MB → 115 KB |
| multi-key desc, skip 20 | 14.2 ms | 7.05 ms | 100,000 → 100,101 | 100 → 100 | 13.7 MB → 198 KB | 7.19 MB → 115 KB |
| multi-key asc, skip 50,000 | 116.0 ms | 4.10 ms | 100,000 → 50,111 | 50,100 → 100 | 106.6 MB → 2.3 MB | 56.9 MB → 1.06 MB |
| multi-key desc, skip 50,000 | 115.0 ms | 7.58 ms | 100,000 → 100,001 | 50,100 → 100 | 106.7 MB → 3.2 MB | 56.9 MB → 1.91 MB |
| one run of 100k, asc | 195.0 ms | 20.4 µs | 100,000 → 31 | 100,000 → 10 | 186.7 MB → 20 KB | 105.5 MB → 12.5 KB |
| one run, desc, skip 50,000 | 189.7 ms | 7.15 ms | 100,000 → 100,001 | 100,000 → 10 | 191.5 MB → 2.9 MB | 106.3 MB → 1.85 MB |
| one run, multi-key | 187.7 ms | 164.3 ms | 100,000 → 100,031 | 100,000 → 100,000 | 189.9 MB → 183.7 MB | **108.7 MB → 0.32 MB** |
| mixed Int/Float (fallback) | 139.1 ms | 119.6 ms | 100,000 → 2 | 100,000 → 100,000 | 129.4 MB → 115.9 MB | 107.0 MB → 107.0 MB |
| `$sort` desc, `$skip` 20, `$limit` 10 | 14.7 ms | 7.96 ms | 100,000 → 100,001 | 30 → 30 | 13.5 MB → 59 KB | 7.19 MB → 35 KB |

The full table for 1k, 10k and 100k is in the results JSON.

- **Ascending stops early,** as shown by keys walked: `skip + limit`, plus one for the Int probe, at every size.
- **Descending.** Time is O(N) keys walked, with no per-key allocation; memory is O(`skip + limit`).
- **One run, multi-key** must decode every document to apply the later key, but holds at most `keep + 256` at a time.
- **Fallback** reads 2 probe keys instead of walking the index before giving up.
- **Aggregation still decodes rows before `$skip`,** because the pipeline applies it: 30 documents here.

### 4.3 CI — run 40193341830 (final head `c622848`; release size; all suites; against `dev` `ab886fd` and v0.12.1)

Each comparison ran on one runner with 3 interleaved pairs: x86_64 on an AMD EPYC 7763, arm64 on `ubuntu-24.04-arm`. The PR gate's baseline is `dev` before any Stage 1C code, so the run measures the whole stage.

| Case | x86_64 vs `dev` | x86_64 vs v0.12.1 | arm64 vs `dev` | arm64 vs v0.12.1 |
|---|---:|---:|---:|---:|
| DOC-12 `doc/sorted_page_asc/1000` | 122.0 → 19.1 µs (6.4×) | 120.3 → 18.7 µs (6.4×) | 120.2 → 18.0 µs (6.7×) | 119.1 → 17.9 µs (6.7×) |
| DOC-12 `doc/sorted_page_asc/10000` | 787 → 19.7 µs (40×) | 776 → 19.4 µs (40×) | 857 → 18.7 µs (46×) | 862 → 18.6 µs (46×) |
| DOC-12 `doc/sorted_page_asc/100000` | 7,735 → 20.4 µs (379×) | 6,855 → 20.3 µs (338×) | 7,454 → 19.5 µs (383×) | 7,406 → 19.7 µs (377×) |
| DOC-6 `stabilization/sorted_page/1000` | 123.3 → 66.7 µs (−45.9%) | 121.7 → 65.9 µs (−45.9%) | 120.1 → 67.8 µs (−43.6%) | 119.7 → 67.7 µs (−43.5%) |
| DOC-6 `stabilization/sorted_page/10000` | 803 → 506 µs (−37.0%) | 798 → 504 µs (−36.8%) | 863 → 533 µs (−38.2%) | 849 → 531 µs (−37.4%) |
| DOC-11 `doc/range_window` (Stage 1A) | 0.99–1.02 | 0.02–0.64 (Stage 1A's gain) | 0.98–1.00 | 0.03–0.64 (Stage 1A's gain) |
| `vector/find_nearest` (Stage 1B) | neutral | 0.63–0.81 (Stage 1B's gain) | neutral | 0.64–0.77 (Stage 1B's gain) |
| `doc/aggregate_group` 1k / 10k / 100k | 1.01 / 1.05 / 1.00 | 1.01 / 1.04 / 1.01 | 1.00 / 0.93 / 0.99 | 1.00 / 0.98 / 1.01 |

The CI Criterion suite runs `stabilization/` at 1k and 10k only. DOC-6 at 100k is measured locally (§4.1).

- **Criterion:** 4 jobs, 77 + 11 cases each. Every case is neutral or improved: 0 regressions, 0 failures, 0 fingerprint mismatches.
- **Vector:** 8 jobs, 32 steps (HNSW at 64 and 8 MiB, resumable, filtered, concurrency), 0 failures. Recall@10 identical in every step.
- **Browser:** both jobs passed at 1 and 8 MiB, with no confirmation round.
- **Tier 0:** fingerprints, MEM-2 allocations and VEC-6 codegen all pass on both runners.

**An earlier run's failure (40186376354, head `ee907c4`).** That run failed one job, x86_64 against v0.12.1 at 100k: `doc/aggregate_group/100000` +12.9% on an Intel Xeon 6973P. Its pairs were 1.19, 1.16, 1.16, then 1.13, 0.91 and 1.05 in the confirmation round. It is not a regression of this change, on four counts:

- **The case cannot reach the change.** Its pipeline starts with `$match`, so `aggregate` declines the sorted-index path at its first check; the planner, executor and `$group` code it runs are unchanged.
- **The same run, against `dev`, measured 0.99.**
- **Local runs against `dev` and v0.12.1 measured 0.94 and 0.95.**
- **This run measured 1.00, 1.01, 0.99 and 1.01** on the four runner/gate pairs.

A re-run of the failed job and a first run on `c622848` were both cancelled manually, mistaken for duplicates; this run replaced them. History in `ci-runs.txt`.

## 5. Not done, limitations and unverified

| Item | Status |
|---|---|
| O(page) descending | Not possible without reverse iteration on `ReadTxn` (a public trait), which the brief rules out. Descending stays O(N) keys walked; memory is O(`skip + limit`) |
| Deep descending pages | Hold up to `skip + limit` ids and one 24-byte record per distinct value held: 3.4 MB at skip 50,000 over 100k distinct values. The multi-key version also rebuilds runs at the end: 1.9 MB, against 1.1 MB in the first version and 56.9 MB on `dev` |
| Aggregation and `skip` | `aggregate` still decodes rows before `$skip`; only `find_with_options` skips them |
| Corrupt keys beyond an ascending page | No longer force a fallback (§2.5) |
| String keys and caller-supplied ids | Pre-existing, unchanged. A shorter string's key followed by an id whose first byte is `0xFF` can sort after a longer string that extends it with an escaped NUL (`"a"` + id `FF…` vs `"a\0b"`). Index order then differs from the comparator. Generated ULIDs do not start with `0xFF` before the year 10889; caller-supplied ids can. `dev` relied on index order in the same way |
| ARM64 | CI runners only (§4.3). The profile's counts (keys, documents, allocations) are deterministic and architecture-independent; its timings are x86_64 only |
| macOS, Windows, devices, browser query timing | Not measured; the browser suite is vector-only |
