# v0.13.0 Stage 2B — ANN rescoring from the retained vector block

Engine change: PR #116 (`feat/v013-ann-retained-rescore`; `6f90eeb` engine and tests, `f4efebc` harness). Reports and changelog: the follow-up PR on `docs/v013-stage2b-reports`. Data: [V013_STAGE2B_BENCHMARK_RESULTS.json](V013_STAGE2B_BENCHMARK_RESULTS.json). Merge status and decisions: [V013_STAGE2B_MERGE_READINESS.md](V013_STAGE2B_MERGE_READINESS.md).

## 1. Summary

After the graph walk, ANN rescoring used to read every candidate with its own storage `get` and decode. When unfiltered exact search has retained the field's decoded vectors for **exactly the revision of the query's read snapshot**, rescoring now takes the candidates from that block. A candidate missing from the block, and every query without a block, reads storage as before. Scores are bit-identical and results unchanged.

| | Result |
|---|---|
| With a retained block (the case this stage targets) | HNSW-3 10k × 384-d at 64 MiB, unquantized: −19.4% warm / −18.6% cold at `efSearch` 100, −14.9% / −19.7% at 400. Other unquantized configurations: −10% to −28%. Binary-quantized graphs: −4% to −18%. Vector point reads per query drop from `efSearch` to 0. |
| Without a block | Unchanged within noise. This is what the CI HNSW-3 and CONC-2 runs measure by default. |
| CONC-2 as defined (filtered, no block) | Unchanged. With a block: −5% to −7%, because the graph walk dominates filtered queries. |
| 4-thread CPU per request ≤ +40% over 1 thread | **Not met:** +45% on the candidate (+48% on `dev`) for CONC-2. The inflation comes from the graph cache lease, not rescoring (§6.4). |
| Correctness | 11 new integration tests and 1 unit test. 8 of 9 mutants killed; the survivor is equivalent. Fingerprints identical across `dev`, candidate and v0.12.1 in 128 sweeps. All local gates pass. |
| CI | All vector, browser and Tier 0 jobs pass. arm64 with a block: −11% to −21% vs `dev`. **x86 Criterion failed** on `doc/count_indexed_eq` (+22% to +34%, Zen 3 only): a code-placement effect of the count closure, **confirmed by control runs** (§6.6). |

The main finding is that **a block and a ready graph rarely coexist by default**. Only unfiltered *exact* search builds the block, Auto mode chooses ANN whenever the graph is ready, and finishing a graph build evicts the field. §6.5 lists where hits do occur. I did not add machinery to create blocks for ANN, or raise any budget, to manufacture hits.

## 2. Phase 1 — the current ANN path (`dev` 2c0c170)

### 2.1 Trace of `vector_api.rs::search_vectors_in` (HNSW)

1. **Snapshot and revision.** One `ReadTxn` serves the whole query. `revision(txn, "vec::<c>::<f>")` reads the vector table's counter from `meta::revisions`. Every write to a vector table bumps that counter in the same write transaction (`collection.rs` insert/update `apply_batch` + `bump_revision`, delete likewise; index create and drop bump it too). Counters are never reset, and `restore_from_snapshot` builds a new `Database` with its own cache.
2. **Readiness.** `graph::header` must carry the same revision (`ready`). Explicit `Ann` on a stale graph is an error; Auto falls back to exact. Unchanged.
3. **Graph-cache lease.** `CacheLease::take(shared, table, revision)` *removes* the graph's `NodeCache` from the shared map (or starts an empty one), reserves memory with `reserve_graph` (75% of what is free, at least 64 KiB) and returns the cache on drop if the epoch still matches.
4. **Candidates.** `graph::search` returns about `ef` IDs. `ef = max(efSearch or 100, wanted × oversampling)` capped at the collection size. A filter is either an ID set (`Eligibility::Ids`) or a bitmap charged to the lease (`Mask`). `SearchMemoryLimit` falls back to exact search.
5. **Candidate vectors.** For each ID: `txn.get(vec table)`, `decode_f32_vec` (an allocation and a copy), and a length check.
6. **Rescoring.** `score_with_query_norm`, which for cosine recomputes the stored vector's norm.
7. **Ranking and grouping.** A sort, or the grouped `Ranker` (which reads each competitive document for its group key), then the score threshold.
8. **Documents.** `load_results_limited_in` reads up to `wanted + 1` documents, then `reduce_vector_rows`. If too few rows remain, `ef` doubles and steps 4–8 repeat.

### 2.2 Measurements (local i5-9400F; flat `perf -e cpu-clock` profiles; this sandbox allows no call graphs)

Candidates: vector point reads per query equal `ef` exactly (100 or 400) in every configuration. Storage reads add about 14 (10–11 documents plus metadata).

Per-candidate cost model (`hnsw_profile --rescore`, public API, warm pages):

| Config | get + decode | binary search in block | score |
|---|---|---|---|
| 2k × 128 | 346–356 ns | 48 ns | 35 ns |
| 2k × 384 | 451–464 ns | 48 ns | 97–101 ns |
| 10k × 128 | 402–410 ns | 60 ns | 35 ns |
| 10k × 384 | 542–586 ns | 60 ns | 98–100 ns |

The model is a lower bound. At 10k × 384 and 64 MiB it predicts 6% of an `ef` 100 query, but removing the reads saves 19%: in a real query the reads miss caches that the graph walk is also using.

Flat profiles (CONC-2 harness shape: 2k × 128, binary graph m=8 efC=64, `ef` 100):

| Share of CPU | Unfiltered, 1 thread | Filtered (CONC-2), 1 thread |
|---|---|---|
| redb point-read path (`get_page`, `Btree::get`, `child_for_key`, `key_end`, `key`, `memcmp`) | ≈ 26% | ≈ 9.5% (shared with the filter's index scan and document loads) |
| `memmove` (decode copies) | 7.2% | 3.6% |
| Graph walk (`Code::decode_into`, `Reader::{layer,load,visit,distance,push_queue}`) | ≈ 30% | ≈ 63% |
| Rescoring score kernel | 2.9% | 0.6% |
| Document decoding (postcard `Value`) | 11.7% | 3.4% |

Contention at four threads (2k × 128, `ef` 100, unfiltered, steady state): graph-table point reads per request rise from 0 at one thread to 76–93 at four threads, at **both** 8 and 64 MiB, on `dev` and on the candidate alike. Mechanism in §6.4.

The conclusion I drew before implementing: point reads are about a quarter of unfiltered ANN CPU and under a tenth of filtered ANN CPU. Removing them can meet ≥ 15% only for unfiltered, unquantized walks, and cannot fix four-thread scaling.

## 3. Phase 2 — implementation

### 3.1 Rescoring (`vector_api.rs`, inside the retry loop, after `graph::search`)

```text
retained = shared.lock().peek_vectors("<c>::<f>", rev)      // one short lock per walk
             .filter(|r| r.block.dimensions == query.len())
for id in candidates:                                         // order unchanged
    if retained has id (binary_search on block.ids):
        v = block.values[i*d .. (i+1)*d]
        score = norms present ? score_with_norms(.., norms[i]) : score_with_query_norm(..)
    else if txn.get(vec, id) decodes to a vector of the query's length:
        score = score_with_query_norm(..)                     // the original path
    else: skip                                                // as before
    finite scores go to the ranker or grouped window exactly as before
```

- **Same vectors.** The block was decoded from the same vector table at the same revision, and is built only from entries that decode to `dimensions` floats. A candidate absent from it is therefore one the point-read path would also skip. It is still read from storage, not dropped, so an inconsistency would fall back to the original behaviour instead of silently losing a result (requirement 7, tested in §4.1).
- **Same bits.** For cosine, `score_with_norms(norm_sq(v))` equals `score_with_query_norm(v)` bit for bit: Stage 1B's construction, re-verified here across tails, zero, subnormal and overflowing vectors. Euclidean ignores norms. Dot product cannot reach this code: HNSW rejects dot-product indexes.
- **Unchanged:** graph traversal, candidate order, `efSearch`, oversampling, retries, filters, grouping, thresholds, ranking, document loading, the `Exact`/`Ann`/Auto selection and the memory fallback.
- **No new allocation and no second map.** The lookup is a binary search on the existing ordered ID vector. The decoded `Vec<f32>` per candidate disappears on hits.

### 3.2 Cache lifecycle (`search_cache.rs::peek_vectors`)

`peek_vectors(key, generation)` returns the resident block only when its generation equals the snapshot revision. It differs from `vectors()` (the exact path's lookup) in two deliberate ways:

- It does **not refresh LRU recency**. ANN traffic does not change which entry is evicted next, so eviction order stays what exact searches alone produce, at every budget.
- It does **not remove an older-generation entry**. Removal is left to the exact path, as before.

Like `vectors()`, it records the borrow in `vector_loans`. The returned `Arc` keeps the block alive until rescoring finishes; if the block is evicted meanwhile (budget change, memory pressure, a newer insert), `pinned_bytes` counts it as **active** memory, so new reservations and admissions still see it. The mutex is held only for the lookup, never during scoring.

Interaction with the graph lease: the lease's reservation is taken before the lookup and covers only graph nodes, scratch and bitmaps. The borrowed block is either resident (counted in `retained_bytes`) or pinned (counted in `active_bytes`), so the two loans never share or bypass an allowance. Budget defaults, `reserve`/`reserve_graph` and `trim` are untouched.

## 4. Snapshot safety

**Claim.** A query's scores and documents come from its one read snapshot, never a mixture.

**Argument.**

1. The candidate set, filter, graph and documents come from the query's `ReadTxn` (redb MVCC) and the leased graph pinned to `(table, revision)`. Unchanged from `dev`.
2. The block is used only if `block.generation == revision(txn)`. Every vector-table mutation bumps that revision in the same commit, and revisions are never reused within a cache's lifetime. The block's contents therefore equal the snapshot's vector table entry for entry.
3. The `Arc` keeps that block's memory valid for the whole rescoring loop even if it is evicted or replaced.
4. Misses read the snapshot through the same `ReadTxn`.

**Tests** (`tests/vector_ann_retained_rescore.rs`). A probe backend counts vector point reads per thread and can park a reader on its first read of a chosen table.

| Test | Interleaving | Asserts |
|---|---|---|
| `a_write_and_newer_block_during_the_walk_never_mix_generations` | Reader parked inside the graph walk (cold graph, first `hnsw::` read); a write commits and an exact search installs the **newer** revision's block; reader resumes | Old revision's IDs, score bits and body; reads storage (the newer block is refused) |
| `a_write_during_the_walk_leaves_the_snapshot_block_usable` | Same pause; write commits, the old revision's block stays resident | Rescored from the block (0 vector reads), old snapshot's results |
| `a_write_between_the_walk_and_rescoring_reads_never_mix_generations` | Reader parked at its first rescoring read, after the walk; write plus newer block | Old snapshot's results |
| `a_block_evicted_mid_rescoring_stays_valid_and_accounted` | Grouped query parked mid-rescoring (first group-key read) while borrowing the block; budget set to 0 (eviction), write, newer block | Active bytes rise by exactly the block's bytes; rescoring finishes on the borrowed block (0 reads); old results; all loans released |
| `concurrent_reads_writes_and_evictions_score_from_one_snapshot` | 3 ANN readers, a writer, a block-building exact loop and a budget cycler (64/1/0/16/64/8 MiB) | Every hit's score equals the score recomputed from the vector in the document returned with it; some queries were block hits; active bytes 0, peak ≤ budget |

A walk reads only its snapshot and its leased graph. A write parked inside the walk therefore leaves the shared state at rescoring time identical to a write committed just after the walk. The third test covers the point after the walk directly, and the fourth covers the middle of rescoring. `vector_score_and_body_share_a_snapshot_even_when_cache_is_warm` (`concurrent_consistency.rs`) is unchanged and passes.

### 4.1 Other correctness tests

- `retained_block_rescoring_is_bit_identical_to_point_reads`: cosine and Euclidean × none/scalar/binary (binary is cosine-only) × dims 3, 8, 13, 130 × 3 queries (random, zero, ±1 pattern) × 8 option sets: plain; offset; `ef` 1 with oversampling 1 (retries); score threshold; grouping with size 2; grouping that forces retries; indexed filter; `topK` beyond the collection. The data include zero, `1e18` (squares overflow), subnormal and unnormalized vectors. For each combination, the full result (IDs, score bits, bodies, `next_offset`, path, reason, revision, `ef`, distance count) is equal without and with a block, the control reads storage and the hit reads none. The test also asserts that HNSW rejects dot product.
- `large_filters_through_the_graph_bitmap_rescore_identically`: the bitmap filter path at 256 KiB.
- `a_candidate_absent_from_the_block_falls_back_to_its_point_read`: a block built while a record was truncated, then a valid record restored behind the revision. The candidate is read (exactly 1 read) and ranks first; an invalid record is still skipped.
- `a_block_of_another_revision_field_or_collection_is_never_used`: four vector tables with equal revision numbers.
- `writes_and_rebuilds_invalidate_and_reuse_blocks_correctly`: delete, insert, rebuild (which evicts the block; the revision is unchanged).
- `budgets_from_zero_to_64_mib_keep_results_and_accounting`: 0, 1, 8, 16 and 64 MiB. At 0 nothing is retained.
- `search_cache::peeking_borrows_one_generation_without_changing_eviction_order` (unit): recency, generations, pinned accounting.

### 4.2 Mutation testing (`evidence/…/mutations.json`)

| Mutant | Killed by |
|---|---|
| M1 generation check removed | unit + 2 integration |
| M2 older block accepted | unit + 1 integration |
| M3 lookup returns the insertion point of an absent ID | 1 integration |
| M4 lookup off by one | 8 integration |
| M5 candidate absent from the block dropped | 1 integration |
| M6 norm of the wrong vector | 8 integration |
| M7 loan not recorded (accounting bypass) | unit + 1 integration (after tightening the eviction test from `≥` to the exact byte delta) |
| M8 peek refreshes recency and drops older generations | unit |
| M9 dimension guard removed | survives: equivalent, because the same revision implies the same index definition |

The first run hung on M1: a reader panic in the scoped stress test left its helper loops spinning. The test now stops them on unwind, and the mutation driver has a per-suite timeout.

## 5. Phase 3 — regression gates

| Gate | Result |
|---|---|
| `cargo fmt --check`, `cargo check --workspace --all-features` | pass |
| `cargo clippy --workspace --all-targets --all-features -D warnings` | pass |
| wasm32 clippy (`taladb-web`, all features and no default features) | pass |
| `cargo test --workspace --all-features` | 856 passed, 0 failed |
| `cargo test -p taladb-web --target wasm32-unknown-unknown` (wasm-bindgen 0.2.114) | 22 passed |
| MSRV `cargo +1.90 check --workspace --all-features` | pass |
| `actionlint performance.yml` | pass |
| Score fingerprints, 16 HNSW configurations × 8 sweeps, `dev` / candidate / v0.12.1 | 128 of 128 identical; block, no-block and plain sweeps agree at each `ef` |
| CI: Tier 0 (fingerprints, VEC-6, MEM-2), native vector 8/64 MiB, browser, cumulative | pass (§6.6) |
| CI Criterion | arm64 pass; **x86 fail** on `doc/count_indexed_eq`: Zen 3 code placement, confirmed (§6.6) |

## 6. Phase 4 — performance

Local runs: i5-9400F with the `powersave` governor. Both sides were built from the candidate's harness with the JCC-erratum mitigation (`-x86-branches-within-32B-boundaries`), pinned (`taskset -c 5` for HNSW, `1-5` for CONC-2), and run in 3 interleaved rounds; medians are reported. "Cold" is the first pass over 100 probes after a cache reset (graph cold; the block, when present, retained by one exact query); "warm" is the second pass. The point-read control is `dev` built with the same harness.

### 6.1 HNSW-3, block retained (`hnsw_profile --rescore`, warm mean, `dev` → candidate)

| Config | `ef` 100 warm | `ef` 100 cold | `ef` 400 warm | `ef` 400 cold | Block resident |
|---|---|---|---|---|---|
| 2k × 128, 8 MiB, none | −15.9% | −16.2% | −26.9% | −25.0% | yes |
| 2k × 128, 64 MiB, none | −17.2% | −14.9% | −27.6% | −25.5% | yes |
| 2k × 128, 8 MiB, binary | −9.1% | −8.4% | −18.2% | −17.5% | yes |
| 2k × 128, 64 MiB, binary | −8.0% | −8.1% | −18.4% | −17.8% | yes |
| 2k × 384, 8 MiB, none | −10.0% | −7.7% | −16.5% | −22.1% | yes, graph squeezed |
| 2k × 384, 64 MiB, none | −15.9% | −13.0% | −25.6% | −25.1% | yes |
| 2k × 384, 8 MiB, binary | −3.7% | −5.1% | −19.2% | −14.4% | yes |
| 2k × 384, 64 MiB, binary | −6.1% | −4.8% | −12.8% | −12.6% | yes |
| 10k × 128, 8 MiB, none | −1.3% | −0.4% | −1.5% | −0.7% | yes, graph displaced |
| 10k × 128, 64 MiB, none | −10.0% | −8.2% | −14.1% | −12.3% | yes |
| 10k × 128, 8 MiB, binary | −1.7% | −2.0% | −2.8% | −2.1% | yes, graph displaced |
| 10k × 128, 64 MiB, binary | −9.8% | −5.7% | −9.2% | −8.1% | yes |
| **10k × 384, 64 MiB, none (HNSW-3)** | **−19.4%** | **−18.6%** | **−14.9%** | **−19.7%** | yes |
| 10k × 384, 64 MiB, binary | −4.1% | −5.5% | −9.2% | −7.7% | yes |
| 10k × 384, 8 MiB, none / binary | −0.9% / −1.7% | −0.6% / −0.3% | +1.2% / −0.8% | +0.1% / −1.7% | **no** (15.5 MB > 8 MiB) |

Recall@10 and distance counts are identical on both sides in every row. Vector point reads per query: `ef` → 0 wherever the block is resident.

### 6.2 HNSW-3, no block (the CI default)

Every configuration × `ef` ∈ {50, 100, 200, 400} (main sweep), plus the no-block rows of the rescore section: within local noise. Most deltas fall within ±4%; isolated rows reach ±10–14% in both directions (e.g. +10.4% and −14.4% at `ef` 100 in two 64 MiB rows) and do not recur in the cold column or at other `ef` values. The no-block path adds one mutex lock and a `None` check per candidate.

### 6.3 Coexistence of block and graph

- Wherever the block fits beside the graph (2k at both budgets, 10k at 64 MiB), it is retained, and Stage 2B pays off.
- At **8 MiB**, 10k × 384 never retains a block, so there is nothing to reuse.
- At **8 MiB**, 10k × 128 retains its 5.3 MB block, which leaves the graph too little room: ANN with the block resident is 3.1× (binary) and 1.7× (none) slower than without it at `ef` 100, and 3.7× and 1.4× at `ef` 400, on `dev` and the candidate alike. This cost predates this stage; Stage 2B recovers only 1–3% of it.
- 2k × 384 at 8 MiB unquantized shows the same effect, milder (+50%).

### 6.4 CONC-2 (`vector_memory_profile --steady 400`, CPU ms per request, `dev` → candidate)

| Config | Block | 1 thread | 2 threads | 4 threads | 4t over 1t, `dev` → cand |
|---|---|---|---|---|---|
| **2k × 128, 8 MiB, filtered (CONC-2)** | no | −1.5% | 0.0% | −3.5% | +48% → +45% |
| 2k × 128, 8 MiB, filtered | yes | −5.3% | −7.5% | −6.5% | +50% → +48% |
| 2k × 128, 8 MiB, unfiltered | yes | −27.1% | −10.0% | −16.1% | +74% → +100% |
| 2k × 128, 64 MiB, unfiltered | yes | −26.8% | −9.3% | −15.4% | +69% → +96% |
| 10k × 384, 64 MiB, unfiltered | yes | −15.6% | −32.0% | −20.2% | +51% → +42% |
| 10k × 384, 8 MiB, any | (never resident) | ≈ 0 | ≈ 0 | ≈ 0 | unchanged |

All rows: [results JSON](V013_STAGE2B_BENCHMARK_RESULTS.json) `concurrency_local`. No memory fallbacks, recall unchanged, and peak accounting within budget in every case.

**Why four threads cost more.** `CacheLease::take` moves the graph's node cache *out* of the shared map for the duration of a query. A query that starts while another holds it gets an empty node cache and reads graph nodes from storage. With 2k nodes, which all fit even at 8 MiB, graph-table reads per request go from 0 at one thread to 76–93 at four (`graph_lease_scaling_local`). That brings back exactly the redb and allocation cost Stage 2B removed from rescoring. The faster candidate overlaps more often, so its 2-thread graph reads are higher (35–42 vs 7–10). That is why its 4t/1t ratio rises in the unfiltered 2k rows even though absolute CPU falls at every thread count. Meeting ≤ +40% needs concurrent walks to share a read-only node cache, which changes the cache's ownership model and lies outside this stage.

### 6.5 Where blocks and ANN meet in practice

- **Explicit unfiltered `mode: Exact`** alternating with ANN on the same field and revision: verification, "exact" toggles, and the built-in `recall` command, which runs exact then ANN for each query on one snapshot.
- **`memoryBudget` fallback**: exact search when a graph walk cannot reserve memory. It retains a block when one fits, and later ANN queries at the same revision then hit.
- **Never by default:** Auto mode with a ready graph runs ANN, filtered exact search never builds a block, and completing or rebuilding a graph evicts the field's block. Any write moves the revision, after which the block is unusable until the next unfiltered exact search.

### 6.6 CI

Dispatch 40328873601: release size; vector, Criterion, browser and Tier 0; against `dev` 2c0c170 and v0.12.1 a2ef466; three interleaved pairs per job. **It concluded `failure`**: both x86_64 Criterion jobs failed on one case (below). The other 15 jobs passed. PR run 40328873104 (PR size) and CI run 40328872773 passed.

**Gated vector rows** (build, exact, ANN p50 at 8 and 64 MiB, staged builds, filtered workloads; none and binary; both architectures; both baselines): all neutral, no failures, no warnings, and no fingerprint failures or notes. The new fingerprint check compared every sweep against `dev` and v0.12.1. The improvements flagged against v0.12.1 are `exact_mean_ms` (−32% to −38%: Stage 1B).

**Report-only rescore rows** (10k × 384-d; warm p50, candidate vs base):

| Job | Budget | `ef` 100 block | `ef` 400 block | `ef` 100 / 400 no block | Vector reads with block |
|---|---|---|---|---|---|
| arm64 none, vs `dev` | 64 MiB | −16.2% | −20.5% | −6.9% / −6.2% | 100 → 0, 400 → 0 |
| arm64 none, vs v0.12.1 | 64 MiB | −14.8% | −13.1% | −5.2% / −1.6% | 100 → 0, 400 → 0 |
| arm64 binary, vs `dev` | 64 MiB | −11.3% | −13.0% | −6.3% / −4.8% | 100 → 0, 400 → 0 |
| arm64 binary, vs v0.12.1 | 64 MiB | −6.0% | −8.2% | +1.1% / +0.7% | 100 → 0, 400 → 0 |
| x86 none, vs v0.12.1 (EPYC 7763) | 64 MiB | −24.8% | −30.1% | +1.3% / +1.7% | 100 → 0, 400 → 0 |
| x86 none, vs `dev` (EPYC 9V45) | 64 MiB | +13.7% | +4.9% | +36.2% / −16.0% | 100 → 0, 400 → 0 |
| x86 binary, vs `dev` / v0.12.1 | 64 MiB | −2.2% / −7.7% | −3.8% / −9.5% | ±4% | 100 → 0, 400 → 0 |
| every job | 8 MiB | ±8% (no block can be retained at 10k × 384) | | ±8% | unchanged |

arm64 pairs are tight: every block pair at 64 MiB lies between 0.78 and 0.93. The x86 vs-`dev` job's pairs range from 0.66 to 1.51 on rows this change cannot affect, so that job carries no signal either way.

**CONC-2** (report only; CPU ms per request, filtered):

| Job | No block, 1 / 2 / 4 threads | With block (`--retain`), 1 / 2 / 4 threads | 4t over 1t (cand) |
|---|---|---|---|
| arm64 vs `dev` | 0% / 0% / −5% | −10% / −6% / −8% | +74% (no block), +87% (block) |
| arm64 vs v0.12.1 | 0% / +1% / −8% | −9% / −9% / −8% | +74%, +94% |
| x86 vs `dev` | +3% / +2% / −4% | +2% / 0% / −7% | +106%, +102% |
| x86 vs v0.12.1 | 0% / 0% / 0% | −8% / −3% / −5% | +118%, +122% |

**Criterion: `doc/count_indexed_eq` on x86_64 (AMD EPYC 7763), both baselines.**

| Case | vs `dev` | vs v0.12.1 |
|---|---|---|
| `doc/count_indexed_eq/10000` | +23.2% (6/6 pairs, T 12%) | +22.3% (6/6) |
| `doc/count_indexed_eq/100000` | +32.4% (6/6 pairs, T 14%) | +34.1% (6/6) |

Every other Criterion case on both architectures is neutral against `dev`. Against v0.12.1, 17 cases on x86 and 22 on arm64 improve (the earlier stages). arm64 shows no `count_indexed_eq` change.

This change does not touch that path. `count({ bucket: 42 })` goes through `count_plan_entries`, a redb range scan with a counting closure. What I found:

- **Same work.** Under callgrind, a driver doing 2,000 such counts over 10k documents executes 431,016,920 instructions on `dev` and 431,016,166 on the candidate. The 754-instruction difference is in malloc; `Range::next` executes exactly 36,358,894 on both.
- **Same code, moved.** The hot functions have identical sizes. Their addresses shift uniformly (+0x140 for `scan` and `Range::next`, +0x3e0 for the closures) and keep the same alignment modulo 64.
- **No change on Intel.** Locally (i5-9400F, Criterion, 3 interleaved rounds, with and without the JCC mitigation), both sides are equal within 5% per round.
- **Base inside history.** Stage 2A's CI history for this case on x86 is 5.0–5.5 µs (10k) and 35–39 µs (100k). This run's baseline (5.35 / 35.7 µs) is in range; the candidate (6.5 / 48 µs) is not.

**Control runs confirm code placement.** Throwaway branches added only inert padding, adjusted so that particular functions land at chosen addresses, and their x86 jobs were re-run until they drew an EPYC 7763:

| Variant (x86 Criterion, `doc/count_indexed_eq/10000`, against 2c0c170) | Main hot loop (`Range::next`, AccessGuard drop, `scan`, leaf accessors, `get_page`) | Count closure | EPYC 7763 (Zen 3) | Other x86 CPUs |
|---|---|---|---|---|
| **Stage 2B as merged** | moved (+0x140) | moved (+0x3e0, 32 → 0 mod 64) | **+20% to +23%** in 4 of 4 jobs (+32% to +36% at 100k) | not drawn |
| A: `dev` + 0x140 read-only bytes | at Stage 2B's addresses | base alignment | 0.988, neutral | 1.007 (9V74), 1.005 (Xeon) |
| B: Stage 2B + 0xec0 read-only bytes | back to base, mod 4 KiB | third placement | not drawn | 0.994, 0.995 (9V74), 1.010 (9V45) |
| **C: `dev` + 0x140 bytes + 0x280 NOPs** | at Stage 2B's addresses | **at Stage 2B's address** | **1.223, regression** (6/6 pairs) | 1.003 (9V74) |
| **D: Stage 2B + 0xc10 NOPs** | at Stage 2B's addresses | **back to base, mod 4 KiB** | **0.992, neutral** | 1.020 (9V45) |

The cause is where the closure that `count_plan_entries` passes to `scan` lands: one indirect call per index entry, about 40 cycles slower per entry on Zen 3 at Stage 2B's placement. Stage 2B's logic plays no part (C reproduces the regression without it, D removes it with it). The hot loop's own placement does not matter (A). Merge readiness §1.1 and §3 cover the consequences for the release gate.

### 6.7 Memory

No new allocations. A hit avoids one `Vec<f32>` per candidate. Peak accounted memory with a block resident equals `dev`'s: the block was already charged when the exact query retained it, and the graph reservation is unchanged (`peak_bytes` in each rescore row). The only accounting difference is intended: a block evicted while ANN borrows it now stays charged as active until the borrow ends, as it already did for exact search.

## 7. Targets

| Target | Result |
|---|---|
| ≥ 15% on HNSW-3 where a block is retained | **Met** for unquantized graphs at 10k × 384 / 64 MiB (−19.4% / −14.9% warm at `ef` 100/400, −18.6% / −19.7% cold), and at 2k × 128 / 384. **Missed** for binary-quantized graphs (−4% to −18%) and for 10k × 128 / 64 MiB at `ef` 100 (−10%). |
| CONC-2 4-thread CPU ≤ +40% over 1 thread | **Missed:** +45% (from +48% on `dev`; the audit's +80% was measured on v0.12.1). Not attributable to rescoring (§6.4). |
| No regression without a block | Met on every vector row (local and CI, both architectures). **One Criterion gate failed on x86** (`doc/count_indexed_eq`, +22% to +34% on Zen 3) on a path this change does not touch: a code-placement effect, confirmed by control runs (§6.6). |

## 8. Limitations

- ARM64, browser and WASM timings come from CI only. The local machine is x86_64 with a fixed `powersave` governor, and local noise is about ±10% on single rows.
- Local profiles are flat (no call graphs are allowed in this sandbox), so attribution is by symbol self-time.
- The block is only as useful as its presence. Nothing in this stage makes blocks more common, by design.

## 9. Reproducing

Evidence and scripts live in `~/.cache/taladb-stage0/evidence/v013-stage2b/` on the development machine:

- `build_sides.sh` builds each side from the candidate's harness;
- `run_matrix.sh hnsw|conc 3` runs the matrices;
- `run_fp.sh` runs the fingerprint pass;
- `profile.sh` runs the perf profiles;
- `gates.sh` runs the local gates;
- `mutate.py` with `mutations.spec.json` runs the mutation tests;
- `make_s2b_results.py` builds the results JSON.

In CI, dispatch `performance.yml` with `baseline=2c0c170d6757d395e7f65b812c31460869c1df0e`, `cumulative=true`, `size=release`, `suites=vector,criterion,browser,tier0`.
