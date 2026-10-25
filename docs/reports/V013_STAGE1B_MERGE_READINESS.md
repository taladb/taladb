# v0.13.0 Stage 1B — merge readiness

Date: 2026-10-25. Branch: `feat/v013-exact-cosine-norms` → `dev` (PR opened; **not merged**). Details: [V013_STAGE1B_IMPLEMENTATION_REPORT.md](V013_STAGE1B_IMPLEMENTATION_REPORT.md), [V013_STAGE1B_BENCHMARK_RESULTS.json](V013_STAGE1B_BENCHMARK_RESULTS.json).

## 1. Verdict

**Correct and safe to merge.** The primary target is met on x86_64 and missed by 0.1 point on arm64 against `dev`. Two decisions are yours (§3).

| Requirement | Result |
|---|---|
| Bit-for-bit equal exact cosine scores | Yes: `to_bits()` against streaming and the pre-change kernel at 1–40, 128, 383–385, 768, 1536 dimensions, with zero, subnormal, underflowing, overflowing and tied vectors; all fingerprints equal against `dev` and v0.12.1 on both runners |
| Multiple blocks, budget boundaries, cold/warm, writes, reopen, concurrency, generation invalidation | Yes: `tests/vector_cosine_norms.rs` (9 tests) plus 2 unit tests. Mutation checks show the tests catch wrong norms, misaligned norms and uncharged bytes |
| Existing exact and ANN fingerprints | Yes: Tier 0 (77), Criterion (all cases, 4 jobs), browser, HNSW recall@10 identical everywhere |
| Every norm byte charged (reservation, retained, eviction, pinned, peak) | Yes: tested, and the real heap of a retained block equals the accounted bytes plus a constant 113 B of entry overhead |
| Streaming path, Dot, Euclidean unchanged | Yes: same code paths and codegen; neutral in CI |
| No public API, C ABI, storage-format, default, graph, ANN-selection, FTS or planner change | Yes: `vector.rs`, `search_cache.rs`, `collection.rs` only; public `VectorBlock`/`CachedVectors` untouched |
| Workspace tests, rustfmt, clippy, WASM build and tests | Yes: 791/791; clean; wasm32 build and 22 tests pass |
| Tier 0, Criterion A/B, vector CI at 8 and 64 MiB, browser CI, cumulative vs v0.12.1 | Yes: run 40177478711, all 17 jobs green, 0 gate failures |
| ≥ 35% less time on VEC-2 | **x86_64: yes** (−39.6% vs `dev`, −41.2% vs v0.12.1). **arm64: −34.9% vs `dev` (missed by 0.1 point), −36.0% vs v0.12.1.** Local i5-9400F: 19–27% (memory-bound) |
| No significant regression | Yes on every CI runner. One local-only artefact explained and disproved (§2) |

## 2. Things to know

- **The local `score_one/Euclidean` failure is not this change.**
  - The local Criterion gate on the i5-9400F flags `vector/score_one/Euclidean` +31–46%.
  - `euclidean_similarity` is byte-identical in both binaries. Its loop back-edge landed across a 32-byte boundary (Intel JCC erratum).
  - Building both sides with LLVM's erratum padding makes it neutral (0.97–1.01×).
  - CI's EPYC and arm64 runners show 0.94–1.01×.
  - No exception entry is needed: the CI gates pass.
- **Why the target is borderline on arm64 and missed locally.**
  - Cached cosine now costs exactly what dot product costs over the same block: same instruction count, same cache misses.
  - The remaining time is one pass over the vectors. Where the block does not fit the last-level cache, part of the removed work had been hidden behind memory stalls.
  - More speed requires reading fewer bytes per vector, which is out of scope.
- **4 bytes per vector.** Cosine blocks need +0.26% of budget at 384-d, +8.3% at 8-d. In the band where vectors fit but norms do not, the block is retained without norms, exactly as before. Two cosine indexes whose blocks sum to exactly the budget can now evict each other.

## 3. Decisions for you

1. **Acceptance on arm64.** The roadmap target is "≥ 35% less time". arm64 measured −34.9% against `dev` and −36.0% against v0.12.1, in single 3-pair runs; the x86_64 margin is comfortable. Options:
   - **(a) Accept as met within measurement precision** (recommended). The change is at its floor: cosine equals dot.
   - **(b) Re-dispatch the arm64 Criterion suite for more pairs** before deciding.
2. **The fit-boundary fallback** (`7194732`) goes slightly beyond the literal brief. It keeps vectors without norms when only the vectors fit, instead of streaming. Without it, collections in a narrow band would run ~2.5× slower at the phone/WASM default (5,392–5,405 vectors at 384-d and 8 MiB). Keep it (recommended), or drop the commit to retain only blocks that fit with norms.

## 4. Before merging

- The PR's own CI (`ci.yml` incl. `ci-pass`, PR-size performance gates against `dev`) must be green.
- Release notes must not claim mobile or Safari/Firefox gains. Measured on Linux x86_64 and arm64 runners, Chromium, and one local x86_64 machine.
- Stage 1C (sorted-page optimisation) and Stage 2 were not started.
