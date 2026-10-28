# v0.13.0 Stage 2C — partial retention of the exact-search vector block

Engine change: PR #119 (`feat/v013-partial-vector-retention`; `cdfced2` engine and tests, `ebdee16` harness), merged into `dev` as `dcf1fa6`. Reports and changelog: the follow-up PR on `docs/v013-stage2c-reports`. Data: [V013_STAGE2C_BENCHMARK_RESULTS.json](V013_STAGE2C_BENCHMARK_RESULTS.json). Merge status and decisions: [V013_STAGE2C_MERGE_READINESS.md](V013_STAGE2C_MERGE_READINESS.md).

## 1. Summary

When a field's decoded vectors do not fit the cache budget, unfiltered exact search used to stream and decode the whole vector table on every query. It now retains the **prefix** of valid vectors, in key order, that fits in idle memory. Later searches score the prefix and stream only the rest of the table, starting after the prefix's last ID. Every vector is still scored exactly once, in the same order, with the same score bits.

A prefix only borrows idle memory. It is never built while a graph is resident, never evicts anything, and is the first thing given up when anything else needs room, including every graph walk.

| | Result |
|---|---|
| **VEC-2** (10k × 384-d cosine, 8 MiB, warm) | **CI Criterion: −44.5% on x86_64 (EPYC 7763) and −38.3% on arm64** vs `dev`; −47.5% and −40.3% vs v0.12.1. Local: −40.8% (4.14 → 2.45 ms p50). Vectors decoded per query: 10,000 → 4,651. Instructions per query: −36.6%. |
| Other exact searches above the budget | −20% to −73% (dot, Euclidean, 768-d, 10k × 128-d at 4 MiB, 20k and 50k × 384-d at 16 and 64 MiB, the full-block boundary). |
| Exact searches whose block fits, and tables too small for a useful prefix | Unchanged: within ±1.5% on every row. |
| First query, the one that builds the prefix | +8% to +21%; +80% for a 67 MB prefix (50k × 384-d at 64 MiB). Repaid within one warm query (two for the 67 MB prefix). Same cost per byte as a full block. |
| ANN-only and mixed exact/ANN traffic | Unchanged: graph reads, recall and fallbacks equal `dev`, and latency is within ±2.4%, in every configuration (1 and 4 threads, 8 and 64 MiB). |
| 4 threads, exact only | −45% p50 and −43% CPU per request. |
| Browser (WASM, 1 MiB) | Unfiltered exact −13.5% vs `dev`, where a prefix is now retained. |
| Correctness | 17 integration tests and 3 unit tests. 20 of 21 mutants killed, including "treat a partial block as complete"; the survivor is equivalent. Fingerprints identical across `dev`, candidate and v0.12.1 on every row. All local gates pass. |
| CI | Dispatch 40642210555: 17 of 17 jobs pass, with no failures or warnings. `doc/count_indexed_eq` is neutral on an EPYC 7763 against both `dev` and v0.12.1 (§9). |

## 2. Phase 1 — audit (`dev` 92b402c)

### 2.1 Consumers of `RetainedVectors`

| Consumer | Where | Assumption on `dev` |
|---|---|---|
| Unfiltered exact search | `collection.rs::find_nearest_in`, last branch | A hit is the whole table: score the block and stop. A miss streams the table and, when a full block can be reserved, retains it. |
| Dense filter, visited (more than 256 matches) | `find_nearest_in`, first branch | A hit is the whole table: a visited ID absent from the block is skipped as invalid. |
| Dense ID set (≤ 256 IDs, more than an eighth of the table) | `find_nearest_in`, middle branch | A hit is the whole table: walk the block and check membership; no scan. |
| Sparse ID set (≤ 256 IDs, at most an eighth) | `find_nearest_in`, middle branch | Never uses a block: batched point reads. |
| ANN rescoring (Stage 2B) | `vector_api.rs::search_vectors_in` via `SearchCache::peek_vectors` | Already safe for a partial block: a candidate missing from the block is point-read, never dropped. |
| Cache accounting | `search_cache.rs` (`insert`, `trim`, `trim_memory`, `reserve`, `reserve_graph`, `pinned_bytes`) | A block is built only after `reserve(full estimate)`, which can evict least-recently-used entries, graphs included. |

The six assumptions in the brief hold on `dev`. The first two are the ones a partial block breaks. Cached norms are optional per block (Stage 1B). Shared accounting covers graph loans, vector loans and pinned blocks (Stage 2B). Every block is reserved before it is allocated.

`VectorBlock` and `CachedVectors` are public with public fields. The partial marker therefore lives in the crate-private `RetainedVectors`, beside the cached norms, as Stage 1B did. Neither public type changes.

### 2.2 Design: complete and partial blocks

`RetainedVectors` gains one field, `partial: bool`, and one method, `resume_after()`.

- **Complete** (`partial == false`): the default, and every block `dev` builds. The block holds every valid vector of its revision. Valid means a 16-byte key whose value decodes to exactly `dimensions` floats.
- **Partial**: the block holds, in key order, **every** valid vector whose key is at most `block.ids.last()`, and **none** after it. It is never empty. `resume_after()` returns that last ID for a partial block and `None` for a complete one.

The boundary is the last retained ID, not the last key the build examined. Any key between the two is either malformed, and is skipped on every read exactly as without a block, or is the first valid vector that did not fit, and is streamed. No other state is needed. A scan that resumes at `Bound::Excluded(last)` visits exactly the keys that a full scan visits after the prefix, in the same order.

## 3. Phase 2 — building a prefix

In the unfiltered build path of `find_nearest_in`, after `dev`'s two attempts (a full block with norms, then for cosine a full block without them), both unchanged:

1. **Reserve.** `SearchCache::reserve_partial(vector_bytes, count, ⌈count / 8⌉)` runs under the cache mutex. Here `vector_bytes = 16 + 4 × dimensions`, plus 4 for a cosine norm. It returns `None` unless all of the following hold:
   - no graph is resident;
   - nothing is reserved or pinned (`active_bytes() == 0`);
   - the prefix that fits in `min(budget − retained, memory_budget − retained − 64 KiB)` holds at least an eighth of the table's entries.

   The reservation is exactly `capacity × vector_bytes` and is taken before anything is allocated.
2. **Allocate.** The block's IDs, values and norms are allocated with `reserve_exact(capacity)`. No collection-sized map, set or temporary block is built.
3. **Build.** The single scan that scores the query also keeps the first `capacity` valid vectors. The first valid vector that does not fit sets `partial = true`. It and every later vector are scored from their decoded values, as without a block. If the table holds no more than `capacity` valid vectors, the block is complete.
4. **Install.** As before, the block is installed only if `bytes() ≤ reservation` and the cache epoch is unchanged. It is tagged with the existing generation (the vector revision read in the query's transaction).

A cosine prefix always carries norms. The vectors-only fallback stays exactly where `dev` uses it: a full block that fits only without norms. The minimum of an eighth keeps a sliver of idle memory from pinning a block that saves almost nothing. A prefix is never grown in place: a larger one is built only after the old one is evicted, dropped by a budget change, or invalidated by a write.

## 4. Phase 3 — consumers

| Path | Complete block | Partial block |
|---|---|---|
| Unfiltered exact | Unchanged fast path. | Score the prefix with its cached norms, then `scan(Excluded(last), Unbounded)` and decode the rest. Order and arithmetic are those of the uncached scan. |
| Dense filter, visited | Unchanged: visit order, binary search. | Batch the visited IDs as without a block (≤ 64 KiB, ≤ 256 IDs). Sort and dedup each batch. Score the IDs at or below `last` from the prefix; one absent from it is invalid, as without a block. Read the rest with the unchanged `key_batch::visit`. The offers are those of the uncached path, in its order. |
| Dense ID set | Unchanged. | Walk the prefix checking membership, then scan from `Excluded(last)` checking membership: the uncached scan's order. |
| Sparse ID set | Unchanged (never uses a block). | Unchanged. |
| ANN rescoring | Unchanged (Stage 2B). | Code unchanged. A candidate at or below `last` that is absent is invalid and is point-read anyway; a candidate after the prefix is absent and point-read. |

Ranking does not depend on offer order: the `Window` keeps the best candidates by `(score, ULID)`, a total order, and `vector_window`'s unit test checks grouped quotas under random orders. The orders above are preserved anyway, so a grouped query makes the same group-key reads as without a block.

**ANN and partial blocks in practice.** Under the policy in §5, a graph walk reclaims every prefix before it reserves memory, and no prefix is built while a graph is resident. Rescoring therefore meets a prefix only in a race: an exact search reserves its prefix before a walk starts, and installs it before the walk rescores. That race is tested deterministically (§6.2). I did not let ANN keep a prefix for its own field, because that would shrink the graph's allowance, which is the cost the brief ranks highest.

## 5. Phase 4 — cache coexistence

The risk the brief names is a prefix evicting or squeezing a graph. On `dev`, a full block reserves with `reserve()`, which evicts least-recently-used entries, graphs included, until it fits. The block then stays resident beside the graph: Stage 2B §6.3 measured ANN 1.4–3.7× slower at 10k × 128-d and 8 MiB. A prefix does none of that. It only borrows idle memory:

| Rule | Where | Effect |
|---|---|---|
| Refused while any graph is resident | `reserve_partial` | Once a graph has been used, ANN-heavy and mixed workloads behave exactly as on `dev`. |
| Refused while anything is reserved or pinned | `reserve_partial` | Never sized against, or racing, another query's reservation. Under four exact threads, one builds and the others stream once. |
| Leaves the 64 KiB walk workspace idle | `reserve_partial` | A walk that starts while a prefix is being built still gets the working allowance the cache guarantees, instead of falling back to exact search. |
| Never evicts | `insert` | Installed only if it fits beside everything resident and active; otherwise refused. |
| Evicted first | `victim` (used by `trim` and `trim_memory`) | Any admission that needs room takes the prefix before any least-recently-used entry. |
| Reclaimed by graph reservations | `reserve_graph` | A walk or build starts with the allowance it would have had without the prefix. |
| Dropped on budget changes | `apply_budget` | It was sized to the old idle memory. The next exact search sizes a new one; without this, a prefix built under memory pressure would stay small after recovery. |

Unchanged: full blocks, `reserve`, the grant formula of `reserve_graph`, the LRU order among other entries, default budgets and pinned-loan accounting. No new cache, no lock held while scoring, no new public API.

The workspace rule came out of testing. In the first version a prefix took all idle memory. A deterministic test showed that a cold walk starting during the build got only what was left, and the prefix was then refused at install time. On `dev` that walk gets 75% of the budget. With the rule, budgets of 64 KiB or less never hold a prefix (they are the floor at which the cache keeps only a walk workspace), and VEC-2's prefix holds 5,349 instead of 5,391 vectors.

### 5.1 Comparison with the existing policy (`s2c_loop`, 10k × 384-d unless noted, 3 interleaved rounds, medians)

| Workload | Budget | Threads | `dev` → candidate, warm p50 | CPU per request | Graph reads per ANN request | Fallbacks (`dev` / cand) | Resident at the end |
|---|---|---|---|---|---|---|---|
| Exact only | 8 MiB | 1 | −38.9% | −40.0% | — | 0 / 0 | `dev` nothing; candidate the prefix |
| Exact only | 8 MiB | 4 | −44.6% | −43.3% | — | 0 / 0 | same |
| Exact, dense filter, after unfiltered queries (`EF`) | 8 MiB | 1 | exact −39.8%, filtered −12.5% | −36.7% / −12.7% | — | 0 / 0 | same |
| ANN only | 8 MiB | 1 | +0.5% | −0.4% | 2,221.2 = 2,221.2 | 0 / 0 | graph on both |
| Mixed 1:1 (`EA`) | 8 MiB | 1 | exact +0.1%, ANN +0.7% | −0.1% / −0.1% | 2,202.0 = 2,202.0 | 0 / 0 | graph on both |
| Mixed 1:4 (`EAAAA`) | 8 MiB | 1 | exact −0.1%, ANN −0.7% | +0.1% / −0.9% | 2,230.6 = 2,230.6 | 0 / 0 | graph on both |
| Mixed 1:1 | 8 MiB | 4 | exact −1.7%, ANN +0.1% | −1.5% / +0.4% | 2,778.6 → 2,780.1 | 0 / 0 | graph on both |
| Mixed 1:1 | 64 MiB | 1 | exact 0.0%, ANN −0.1% | −0.5% / −6.8% | 47.2 = 47.2 | 0 / 0 | complete block and graph on both |
| ANN only | 64 MiB | 1 | −2.4% | −3.2% | 23.9 = 23.9 | 0 / 0 | graph on both |
| 10k × 128, mixed 1:1 | 8 MiB | 1 | exact −0.2%, ANN −0.5% | −2.9% / +1.1% | 2,375.4 = 2,375.4 | 0 / 0 | complete block and graph on both (`dev`'s policy) |
| 10k × 128, ANN only | 8 MiB | 1 | +0.6% | −0.6% | 1,115.8 = 1,115.8 | 0 / 0 | graph on both |

Recall is identical on both sides in every ANN row. In mixed traffic the candidate builds one prefix, on the first exact query before any graph is used. The first walk reclaims it, and from then on exact search streams as on `dev`. The candidate's peak accounted bytes therefore include that one prefix (8.3 MB at 8 MiB, against `dev`'s 6.3 MB graph peak), always within the budget. All rows: [results JSON](V013_STAGE2C_BENCHMARK_RESULTS.json) `mixed_local`.

What this policy gives up: once any graph is resident, an exact-only field elsewhere in the same database gets no prefix. In those databases exact search behaves as on `dev`. I chose that over a rule that would let exact traffic compete with graphs.

## 6. Correctness

### 6.1 Completeness proof (unfiltered exact search)

Let T be the vector table at revision r, in key order, and V ⊆ T its valid entries. On `dev`, a streamed search offers `score(v)` for each v ∈ V, in key order.

1. **Build.** A prefix P installed for revision r comes from one scan of T at r, the query's snapshot. The scan appends valid entries in key order while |P| < capacity, and marks `partial` at the first valid entry it does not append. Hence P = {v ∈ V : key(v) ≤ key(last(P))}, in key order, and P ≠ ∅.
2. **Lookup.** `vectors(key, generation)` returns only a block whose generation equals the query's revision. Every vector-table change bumps the revision in the same write transaction, and revisions are never reused within a cache's lifetime. A block of generation r was therefore built from T at r.
3. **Read.** A partial hit offers `score(p)` for each p ∈ P in key order. It then scans T at r from `Excluded(key(last(P)))` and offers `score(v)` for each valid v there, in key order. The two ranges partition V by key, so every v ∈ V is offered exactly once, in key order: the streamed sequence.
4. **Bits.** Prefix scores use the cached norm, which Stage 1B proved bit-equal to the recomputed one. Streamed scores recompute it, as on `dev`.

The same argument covers the dense ID-set path, with each range filtered by membership. For the visited path, each batch of visited IDs is split at key(last(P)): an ID at or below it is in P exactly when it is valid (step 1), and the others are read as without a block. A complete block (`partial == false`) is handled as on `dev`. A complete block is never replaced by a prefix of the same generation, and an empty prefix is refused.

### 6.2 Tests (`tests/vector_partial_retention.rs`; unit tests in `search_cache.rs`)

The oracle reads the vector table straight from storage (`scan_all`), keeps the entries exact search treats as valid, scores them with `score_with_query_norm`, ranks by score then ID, and skips vectors without a document. It never touches a block. A probe backend counts, per thread, the vector entries scans stream and the vector point reads, and parks readers on their first read of a chosen table.

| Test | Covers |
|---|---|
| `partial_blocks_score_every_vector_once_and_bit_identically` | Cosine, dot and Euclidean × dims 3, 8, 13, 128, 383, 384, 385, 768, 1536 × prefixes of 1/8, 1/2 and the largest that fits × 5 queries. The corpus includes zero, negative-zero, underflowing, subnormal, overflowing (1e18, 1e30) and tied vectors. Asserts: cold and warm results equal the oracle, one score per vector (`distance_computations`), the cold query streams the table once, the warm one streams exactly `count − prefix` vectors, and there are no point reads. |
| `every_prefix_boundary_ranks_the_whole_table_exactly_once` | Every prefix length from 1/8 to the whole table, k above the table size, so any omission or double score changes the list. Includes the vectors-only fallback near the top, and the minimum below which nothing is retained. |
| `malformed_records_and_foreign_keys_around_the_boundary_are_skipped_once` | A truncated vector, bytes that do not decode, an over-long vector, a 17-byte key just after a valid ID, a 15-byte key just before one, and an orphan vector without a document, at every boundary position. Asserts the prefix counts only valid vectors and the warm pass streams exactly the raw keys after it. |
| `ties_across_the_boundary_break_by_id` | Identical vectors on both sides, with k cutting the tie run. |
| `filtered_searches_over_a_partial_block_match_streaming_and_the_oracle` | Sparse (batched point reads), ID set (prefix plus scan), visited (`$lt` and `$or`), also with an index and a full-text index so the visit follows index order rather than ID order. Asserts full fingerprints equal streaming and one score per eligible vector. |
| `grouping_offsets_thresholds_and_pages_match_streaming` | Offsets, thresholds, `group_by` with sizes 1–3, combinations, and paging with `next_offset` to the end, filtered and unfiltered, with the prefix built before or by the paging query. |
| `inserts_updates_and_deletes_rebuild_the_prefix_for_each_revision` | Inserts after, inside and before the prefix; updates inside, at and after the boundary; deleting the boundary vector and a range across it; a metadata-only update, which keeps the revision and the prefix. |
| `collections_and_fields_keep_separate_prefixes_that_give_way_to_full_blocks` | Two collections, two fields; a prefix never evicts, and a complete block evicts the prefix first. |
| `reopening_rebuilds_the_same_prefix_and_results` | File-backed database, reopened. |
| `graphs_reclaim_partial_blocks_and_block_new_ones_until_rebuilt` | A walk reclaims the prefix with the same ANN result; with a graph resident, exact search streams; a rebuild evicts the field and allows a prefix again. |
| `ann_rescoring_reads_candidates_beyond_a_partial_prefix_from_storage` | The race from §4, forced: an exact reader parked after reserving its prefix, a walk parked after taking its lease, then the prefix installed. The ANN fingerprint (IDs, score bits, bodies, distances, path, revision) equals a cold point-read run, and its vector reads fall strictly between 0 and the reference count: candidates on both sides of the boundary. |
| `a_write_during_a_partial_hit_never_mixes_revisions` | A reader parked at its remainder scan while vectors inside and after the prefix change. The reader returns the old snapshot; the new revision drops the borrowed prefix and retains nothing until the reader is done. |
| `a_prefix_built_from_an_old_snapshot_is_never_served_for_a_newer_one` | A cold reader parked after reserving, then a write. The concurrent query streams. The old revision's prefix, once installed, is never served, and is rebuilt for the new revision. |
| `a_prefix_evicted_mid_query_stays_valid_and_accounted` | Critical memory pressure while a reader borrows the prefix: retained 0, active equals the prefix's bytes exactly, correct results, active 0 afterwards. |
| `memory_pressure_shrinks_drops_and_restores_the_prefix` | Normal, Moderate, Critical, Moderate, Normal: prefix sizes follow the budget, and results are identical. |
| `budgets_from_zero_to_64_mib_keep_results_and_accounting` | 3,000 × 1,536-d (18.5 MB) at 0, 1, 4, 8, 16 and 64 MiB: nothing, nothing (below the minimum share), three prefixes, then a complete block. Retained bytes, streamed counts, active 0, peak within the memory budget. |
| `concurrent_reads_writes_and_evictions_score_from_one_snapshot` | 3 readers (unfiltered, filtered, grouped), a writer, and a cycler over budgets 0 → 64 MiB and pressure levels. Every hit's score equals the score recomputed from the document returned with it, with no duplicates and sorted output. The helper loops stop on unwind. |
| Unit: `partial_blocks_take_only_idle_memory_and_never_evict`, `a_resident_graph_refuses_partial_blocks_and_graph_reservations_reclaim_them`, `partial_blocks_are_evicted_first_and_dropped_when_the_budget_changes` | Sizing, refusal conditions, the workspace, a walk's grant during a build, eviction order, generation replacement, empty prefixes. |

Stage 1B (`vector_cosine_norms`), Stage 2B (`vector_ann_retained_rescore`) and every other existing test pass unchanged.

### 6.3 Mutation testing (`evidence/…/mutations.json`)

| Mutant | Killed by |
|---|---|
| M1 partial block treated as complete everywhere (`resume_after` ignores the flag) | 15 integration |
| M2 builder never marks a prefix partial | 16 integration |
| M3 unfiltered remainder re-scores the boundary vector | 12 integration |
| M4 ID-set remainder rescans from the start | 1 integration |
| M5 ID-set path skips the remainder | 1 integration |
| M6 visited-filter path treats a partial block as complete | 2 integration |
| M7 visited-filter boundary key read from storage instead of the prefix | **survives: equivalent** (same vector, same bits) |
| M8 visited-filter keys past the prefix looked up in it | 2 integration |
| M9 builder keeps one vector beyond its reservation | 15 integration |
| M10 prefix admitted while a graph is resident | unit |
| M11 prefix admitted beside live reservations or pins | unit |
| M12 graph reservations do not reclaim prefixes | unit + 1 integration |
| M13 budget changes keep prefixes | unit + 1 integration |
| M14 prefixes evicted in plain LRU order | unit |
| M15 a prefix insert evicts other entries | unit |
| M16 a prefix replaces a complete block of its generation | unit |
| M17 prefix uses the walk workspace | 2 unit + 12 integration + 1 Stage 1B |
| M18 no minimum useful prefix | 2 integration |
| M19 empty prefix accepted | unit |
| M20 ANN drops candidates beyond a partial prefix | 1 integration |
| M21 visited-filter prefix scores with the wrong norm | 2 integration |

M1 is the mutant the brief asks for: it is detected by 15 of the 17 integration tests.

## 7. Performance

Local runs: i5-9400F, `powersave` governor. Every side was built from the candidate's harness with the JCC-erratum mitigation (`-x86-branches-within-32B-boundaries`), pinned (`taskset -c 5`; four threads on cores 2–5), and run in 3 interleaved rounds; medians are reported. "Warm" is p50 over 40–300 queries after the block exists; "cold" is p50 of queries that start from an empty cache. The fixture is Criterion's `vector/find_nearest` (fixed IDs, the same generator).

### 7.1 Exact search (`exact_vector_profile`)

| Workload | Budget | `dev` warm | Candidate warm | vs `dev` | vs v0.12.1 | Vectors read per query | Retained |
|---|---|---|---|---|---|---|---|
| **10k × 384 cosine (VEC-2)** | **8 MiB** | 4.140 ms | 2.451 ms | **−40.8%** | −40.1% | 10,000 → 4,651 | 8.32 MB prefix |
| 10k × 384 dot | 8 MiB | 3.597 | 2.158 | −40.0% | −40.2% | 10,000 → 4,638 | prefix |
| 10k × 384 Euclidean | 8 MiB | 3.743 | 2.298 | −38.6% | −39.2% | 10,000 → 4,638 | prefix |
| 10k × 768 cosine | 8 MiB | 7.279 | 5.842 | −19.7% | −19.2% | 10,000 → 7,309 | prefix |
| 10k × 128 cosine | 4 MiB | 1.467 | 0.613 | −58.2% | −58.4% | 10,000 → 2,240 | prefix |
| 20k × 384 cosine | 8 MiB | 8.648 | 6.836 | −21.0% | −20.8% | 20,000 → 14,651 | prefix |
| 20k × 384 cosine | 16 MiB | 8.671 | 5.091 | −41.3% | −41.5% | 20,000 → 9,260 | prefix |
| 50k × 384 cosine | 16 MiB | 22.84 | 18.77 | −17.8% | −17.2% | 50,000 → 39,260 | prefix |
| 50k × 384 cosine | 64 MiB | 22.78 | 7.890 | −65.4% | −64.9% | 50,000 → 6,914 | 67.0 MB prefix |
| 50k × 384 cosine | 8 MiB | 22.95 | 22.88 | −0.3% | +1.9% | 50,000 both | none: 5,349 < 6,250 (1/8) |
| 10k × 384 cosine | 16 / 64 MiB | 1.125 / 1.134 | 1.127 / 1.127 | +0.2% / −0.6% | −27% (Stage 1B) | 0 | complete, unchanged |
| 2k × 128, 10k × 128 cosine | 8 MiB | 0.094 / 0.373 | 0.094 / 0.379 | −0.1% / +1.4% | −25% / −28% | 0 | complete, unchanged |
| 20k × 384 cosine | 64 MiB | 2.214 | 2.220 | +0.3% | −27.7% | 0 | complete, unchanged |

p95 moves with p50 in every row (−17% to −69% where a prefix exists). Fingerprints (top-10 IDs and score bits) are identical on all three sides in every row.

### 7.2 The full-block boundary (10k × 384-d cosine: full block 15,560,000 bytes, vectors only 15,520,000)

| Budget | `dev` | Candidate | Change | Retained |
|---|---|---|---|---|
| 15,500,000 (just below a vectors-only block) | 4.214 ms, streaming | 1.154 ms | **−72.6%** | prefix of 9,919 vectors; 81 streamed |
| 15,540,000 (vectors-only fits) | 1.570 | 1.564 | −0.4% | complete without norms, unchanged |
| 15,600,000 (full block fits) | 1.131 | 1.141 | +0.9% | complete with norms, unchanged |

The cliff is gone: just below the full block, the candidate is 2% slower than a complete block with norms, instead of 3.7× slower. The step from the vectors-only block to the full block is Stage 1B's and remains.

### 7.3 First-query cost

| Workload | Budget | `dev` cold (streams) | Candidate cold (builds) | Change | Prefix |
|---|---|---|---|---|---|
| 10k × 384 cosine / dot / Euclidean | 8 MiB | 4.16 / 3.58 / 3.54 ms | 4.82 / 4.23 / 4.30 | +15.8% / +18.2% / +21.4% | 8.3 MB |
| 10k × 768 cosine | 8 MiB | 7.28 | 8.13 | +11.5% | 8.3 MB |
| 10k × 128 cosine | 4 MiB | 1.45 | 1.72 | +18.1% | 4.1 MB |
| 20k × 384 cosine | 8 / 16 MiB | 8.79 / 9.00 | 9.48 / 10.12 | +7.9% / +12.5% | 8.3 / 16.7 MB |
| 50k × 384 cosine | 16 / 64 MiB | 22.84 / 23.00 | 23.96 / 41.50 | +4.9% / +80.4% | 16.7 / 67.0 MB |
| For reference, `dev` building a full block: 10k × 384 at 16 MiB / 20k × 384 at 64 MiB | | 4.16 / 8.79 (`dev` streaming cold) | 5.29 / 11.35 (`dev` building) | +27% / +29% | 15.6 / 31.1 MB |

The cost is a copy of the retained vectors plus, for large blocks, the page faults of fresh memory. glibc serves allocations above its 32 MiB mmap threshold with new pages every time, which is what makes the 67 MB prefix expensive. Per byte it matches what `dev` already pays for full blocks. Every row repays it within one warm query, except the 67 MB prefix, which needs two (it saves 14.9 ms per query and cost 18.5 ms). A prefix is built once per revision: writes, budget changes and eviction start another.

### 7.4 Where the gain comes from

| Case (10k × 384-d cosine) | Instructions per query, `dev` → candidate | Change | Warm timing change |
|---|---|---|---|
| Unfiltered, 8 MiB (VEC-2) | 23,121,590 → 14,657,896 | **−36.6%** | −40.8% |
| Dense filter (50%), 8 MiB, with a prefix | 228,531,766 → 221,502,291 | −3.1% | −4.0% |
| Dense filter, 64 MiB, complete block | 214,960,197 → 214,970,713 | +0.005% | +4.0% |
| Sparse filter (1%), 8 MiB | 200,875,495 → 200,882,866 | +0.004% | +2.3% |

Callgrind totals were differenced between runs of 2 and 12 warm queries, because LTO inlines the whole search path. The VEC-2 gain matches the work removed: decoded vectors fall 53.5% (10,000 → 4,651), instructions 36.6%, time 40.8%. Each decode on `dev` also allocates and copies a `Vec<f32>`, which the prefix avoids. The two filtered rows whose work did not change move +2% to +4%, consistently across rounds. That is code placement: in `s2c_loop`, the same filter-only workload moved −5.8% with identical work. Filtered exact queries spend about 90% of their instructions evaluating the filter over the documents, which this change does not touch.

### 7.5 Filtered exact search (warm p50)

| Workload | `dev` | Candidate | Change | Vector reads |
|---|---|---|---|---|
| Dense (50%), 8 MiB, prefix present (`exact_vector_profile`) | 19.88 ms | 19.08 | −4.0% | 9,880 → 4,521 entries |
| Dense, alternating with unfiltered queries (`s2c_loop EF`) | 20.11 | 17.66 | −12.2% | 9,880 → 4,521 |
| Sparse (1%), 8 MiB | 15.93 | 16.30 | +2.3% (placement, §7.4) | 100 point reads both |

The dense visited path reads a batch's key range with one scan, so its "vector reads" count entries stepped over, not only decodes.

### 7.6 CI

Dispatch 40642210555 (release size; vector, Criterion, browser and Tier 0; against `dev` 92b402c and v0.12.1 a2ef466; three interleaved pairs per job) concluded **success**: 17 of 17 jobs, no failures and no warnings in any comparison. Its x86 Criterion job against v0.12.1 was re-run once (attempt 2, also a success) to get an EPYC 7763 (§9). The PR-size performance run 40642203156 and CI run 40642203151 also passed. The x86 job's CPU is recorded for every row.

**VEC-2 at 8 MiB, Criterion** (`vector/find_nearest_8mib/10000`, gated at the default T = 10%):

| Job | CPU | Ratio | Pairs |
|---|---|---|---|
| x86_64 vs `dev` | AMD EPYC 7763 | **0.555 (−44.5%)** | 0.508, 0.630, 0.526 |
| x86_64 vs v0.12.1 | AMD EPYC 9V45 | 0.525 (−47.5%) | 0.405, 0.697, 0.525 |
| x86_64 vs v0.12.1, re-run (attempt 2) | AMD EPYC 7763 | 0.494 (−50.6%) | 0.445, 0.632, 0.727 |
| arm64 vs `dev` | not reported (GitHub `ubuntu-24.04-arm`) | **0.617 (−38.3%)** | 0.608, 0.628, 0.617 |
| arm64 vs v0.12.1 | not reported | 0.597 (−40.3%) | 0.595, 0.602, 0.600 |

`vector/find_nearest/10000` (64 MiB, complete block) is neutral against `dev` on both architectures (1.037, 1.008) and improves against v0.12.1 through Stage 1B (0.631, 0.636).

**Gated vector rows** (`hnsw_profile`; `exact_mean_ms` includes each run's first, prefix-building query):

| Job | CPU | Exact at 8 MiB | Exact, staged builds (8 MiB) | Exact at 64 MiB | ANN p50 at 8 MiB, ef 50–400 |
|---|---|---|---|---|---|
| x86 none vs `dev` | EPYC 7763 | **0.499** (pairs 0.45–0.57) | 0.422 | 0.987 | 0.931–1.055, neutral |
| x86 binary vs `dev` | Xeon Platinum | 0.471 | 0.460 | 1.098, neutral | neutral |
| arm64 none vs `dev` | | **0.620** (0.60–0.65) | 0.602 | 1.025 | 0.998–1.024, neutral |
| arm64 binary vs `dev` | | 0.614 | 0.630 | 1.010 | neutral |
| x86 none vs v0.12.1 | EPYC 9V74 | 0.444 | 0.329 | 0.618 | neutral |
| x86 binary vs v0.12.1 | EPYC 7763 | 0.615 | 0.545 | 0.624 | neutral |
| arm64 none / binary vs v0.12.1 | | 0.565 / 0.628 | 0.604 / 0.607 | 0.684 / 0.697 | neutral |

Every build, ANN, storage-read, recall and fingerprint row is neutral on every job; filtered workloads (VEC-4) are neutral on both architectures (dense warm p50 0.85–1.01). Report-only rows:

- **Rescore at 8 MiB** (ANN after an exact query, which on the candidate builds a prefix that the walk reclaims): against `dev`, x86 0.954–1.015 and arm64 0.997–1.085. On arm64 the ef 400 rows move by the same amount with and without a block (1.085 and 1.079), so the prefix is not the cause; vector reads are identical (100 and 400). Locally (4 rounds) the same rows span the same band, with and without a block (§7.7).
- **CONC-2** (filtered ANN, 1–4 threads, with and without a retained block): CPU per request within ±6% against `dev` on both architectures, as on `dev`.
- **MEM-3** (`vector_memory_profile` at 1 and 8 MiB, 1–4 threads): peak ≤ memory budget and active 0 in every case. The 18–20 fallbacks at 1 MiB with 4 threads are the same as in Stage 2B's run.

**Browser** (Chromium, WASM, OPFS; 5k × 128-d, binary graph; 3 pairs, confirmation on demand): both comparisons pass. At 1 MiB the 2.66 MB block does not fit, so the candidate retains a 982,604-byte prefix in WASM:

| Case (exact) | Budget | vs `dev` | vs v0.12.1 |
|---|---|---|---|
| Unfiltered | 1 MiB | **−13.5%** (1.515 → 1.310 ms) | −31.0% |
| Grouped (`many-groups`) | 1 MiB | −14.8% | −18.0% |
| Unfiltered | 8 MiB (complete block) | −0.6% | −5.5% |
| Filtered cases | both | −6% to +14%, passing; the job against v0.12.1 shows −3% to +4% on the same rows, which hold no block | |

Exact and ANN fingerprints are identical on every browser case, with no memory fallbacks.

**Tier 0** (fingerprints, VEC-6 codegen, MEM-2 allocations) passed on x86_64 and arm64 in the dispatch and in CI.

**Criterion, everything else.** No other case is flagged on either architecture against either baseline. `doc/count_indexed_eq`: §9.

### 7.7 ANN after a prefix (`hnsw_profile --rescore`, 10k × 384-d, 8 MiB, local, 4 interleaved rounds)

Medians of 4 interleaved rounds; the graph is built at 8 MiB, as in CI. "Block" runs an unfiltered exact query first: on `dev` it streams (nothing fits), on the candidate it builds an 8.3 MB prefix, which the first walk reclaims.

| Row | `dev` | Candidate | Change | Per-round candidate p50 (ms) | Vector reads | Peak accounted |
|---|---|---|---|---|---|---|
| Exact, `exact_mean_ms` (includes the first query) | 4.028 ms | 2.562 | **−36.4%** | | | |
| ANN ef 50 / 100 / 200 / 400, p50 | 3.866 / 6.861 / 12.64 / 20.63 | 3.837 / 6.962 / 12.27 / 19.84 | −0.7% / +1.5% / −2.9% / −3.9% | | storage reads identical | |
| Rescore ef 100, no block, warm | 6.921 | 6.801 | −1.7% | 6.72, 6.48, 9.58, 6.88 | 100 = 100 | 6.29 MB both |
| Rescore ef 100, block, warm | 6.772 | 6.895 | +1.8% | 6.86, 6.63, 9.56, 6.93 | 100 = 100 | 6.29 → 8.32 MB |
| Rescore ef 400, no block, warm | 20.82 | 22.41 | +7.6% | 19.94, 19.20, 24.86, 26.58 | 400 = 400 | 6.29 MB both |
| Rescore ef 400, block, warm | 19.93 | 20.67 | +3.7% | 19.75, 20.35, 20.98, 23.26 | 400 = 400 | 6.29 → 8.32 MB |

Fingerprints, recall and distance counts are identical on both sides in every row. The no-block rows run the same code on both sides and still differ by up to 7.6%; the third candidate round was disturbed in every row. The block rows differ from `dev` by less than the no-block rows do, so I find no effect of the prefix on ANN beyond this noise. The walk's allowance after reclaiming a prefix is exactly `dev`'s (unit test), and steady-state mixed traffic is equal (§5.1). The only lasting difference is the peak: the candidate's includes the one prefix, within the budget.

## 8. Memory accounting

- **Reserved before allocated.** A prefix's reservation is `capacity × vector_bytes`, and the block allocates exactly that capacity. `bytes()` (capacity-based) never exceeds the reservation; `insert` checks it.
- **Peaks.** Peak accounted bytes stayed within the memory budget in every local run, both harnesses (`peak_bytes ≤ memory_budget_bytes` is asserted by `exact_vector_profile` and the budget test). Retained bytes at 8 MiB: 8,323,044 of 8,388,608 for 10k × 384-d cosine; the remaining 65,564 bytes are the walk workspace.
- **Pinned prefixes.** A prefix evicted while a query borrows it is charged as active memory until the borrow ends (`pinned_bytes`), exactly as full blocks are. The test asserts the exact byte delta.
- **Heap.** `heap_retained_by_cold_query` (counting allocator) equals the accounted bytes plus 121 bytes of map entry and key (8,323,165 vs 8,323,044).
- **MEM-2 and MEM-3.** Tier 0 allocation counts are unchanged. MEM-3 accounting asserts pass locally and in CI (`vector_memory_profile` at 1 and 8 MiB).

## 9. x86 Criterion: `doc/count_indexed_eq`

The brief asks to keep the Stage 2B evidence, to measure this case against both baselines, and to separate layout from work.

| Check | `dev` 92b402c | Candidate | v0.12.1 |
|---|---|---|---|
| Instructions, 2,000 counts over 10k documents (callgrind, CI-equivalent release build) | 431,016,278 | 431,016,861 (+583) | 430,398,676 |
| `count_plan_entries::{closure#0}` address in the CI-equivalent bench build (all sides compile the candidate's benches) | `0x3f1810` (16 mod 64) | `0x3f2c60` (32 mod 64) | `0x3ea430` (48 mod 64) |

The candidate executes the same work as `dev` on this case: the count path is untouched. The closure that Stage 2B's controls identified moves by +0x1450, a pure layout change whose effect on Zen 3 can only be read from an EPYC 7763 runner. GitHub assigns x86 runner CPUs at random, so the job against v0.12.1 was re-run until it drew one.

| Timing (Criterion, three interleaved pairs) | CPU | `/10000` | `/100000` |
|---|---|---|---|
| CI, candidate vs `dev` | **AMD EPYC 7763** | 0.987, neutral (1.002, 0.999, 0.987) | 1.014, neutral |
| CI, candidate vs v0.12.1 | AMD EPYC 9V45 | 0.991, neutral | 0.989, neutral |
| CI, candidate vs v0.12.1 (attempt 2) | **AMD EPYC 7763** | **0.986, neutral** (0.987, 1.066, 0.986) | **0.988, neutral** (0.982, 0.982, 0.996) |
| CI, arm64 vs `dev` / v0.12.1 | not reported | 0.997 / 1.010 | 1.004 / 1.008 |
| Local, i5-9400F (no JCC mitigation, as in CI), vs `dev` / v0.12.1 | Intel | +3.5% / +0.4% | +3.3% / −0.1% |

On Zen 3 the case is neutral against both baselines. Stage 2B's +22% to +34% against v0.12.1 on the EPYC 7763 does not appear at this layout: the closure sits elsewhere, and the same instructions run as fast as on v0.12.1. This is a layout outcome, not a fix. The cause Stage 2B confirmed, a Zen 3 sensitivity to where that closure lands, is unchanged, and any later change can move the closure again. The same attempt measured VEC-2 at 8 MiB against v0.12.1 on the 7763 at 0.494 (−50.6%).

I added no padding or alignment, did not touch the tolerances, and added no exception. The Stage 2B evidence and suggested exception text remain as they were in `V013_STAGE2B_MERGE_READINESS.md`.

## 10. Targets

| Target | Result |
|---|---|
| VEC-2 at 8 MiB ≥ 25% faster than `dev` | **Met:** −44.5% (x86_64 EPYC 7763) and −38.3% (arm64) in CI Criterion; −40.8% locally. |
| No substantial HNSW regression; graph not crowded out | Met: ANN-only and mixed traffic equal `dev` in graph reads, recall, fallbacks and latency. |
| No excessive first-query cost | Bounded: +8% to +21% for prefixes up to 16 MiB, +80% for a 67 MB prefix; repaid within one or two warm queries. |
| Large tables at 8 MiB | **Not improved** where 8 MiB holds less than an eighth of the table (50k × 384-d: −0.3%). Deliberate (§3). |

## 11. Limitations

- **Local timings are x86_64 (Intel) only.** arm64, Zen 3 and browser timings come from CI. Local noise is ±2–6% on filtered rows (§7.4).
- **No call-graph profiles.** This sandbox forbids call-graph sampling and hardware counters, so attribution uses callgrind instruction counts and decode counters.
- **A prefix is never grown in place**, and an exact-only field gets none while any graph is resident (§5).
- **Unmeasured:** macOS, Windows, iOS, Android, and browser runs with a prefix specifically (the browser suite's workloads: §7.6).

## 12. Reproducing

Evidence and scripts are in `~/.cache/taladb-stage0/evidence/v013-stage2c/` on the development machine:

- `scripts/build_sides.sh` builds the candidate's harness into each side;
- `scripts/run_matrix.sh scripts/exact.cases 3 …` and `scripts/run_matrix.sh scripts/mixed.cases 3 …` run the matrices, and `scripts/summarize.py` summarises them;
- `scripts/s2c_loop.rs` is the mixed and concurrent driver (not committed);
- `scripts/callgrind_filters.sh` gives per-query instruction counts;
- `scripts/count_eq.sh` covers `doc/count_indexed_eq` (layout, callgrind, Criterion);
- `scripts/gates.sh` runs the local gates;
- `scripts/mutate.py` with `scripts/mutations.spec.json` runs the mutation tests;
- `scripts/make_s2c_results.py` builds the results JSON.

In CI, dispatch `performance.yml` with `baseline=92b402c6ac4837e9af5a56c21bfe4bc9ab999421`, `cumulative=true`, `size=release`, `suites=vector,criterion,browser,tier0`.
