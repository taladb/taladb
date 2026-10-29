# Changelog

## 0.13.0 — 2026-10-29

### Performance

- **Two-sided indexed ranges** (`{ n: { $gte: x, $lt: y } }`) on a field that has never held an array now scan only the index keys inside the window. Before, they collected the IDs of both open-ended halves and intersected them. With 100 matching documents, the cost no longer grows with the collection, and the memory the query allocates falls by 81% at 10k documents and 97% at 100k. Fields that hold arrays keep the previous plan, because different array elements can satisfy each bound.
- **Exact cosine search** on a vector index whose decoded vectors are retained in the cache no longer recomputes each stored vector's norm on every query. The norm is computed once, when the vectors are decoded, and scores are bit-for-bit identical. At 10,000 × 384-d vectors, an exact search takes 35–41% less time on Linux CI runners (x86_64 and arm64). Dot-product and Euclidean indexes, and indexes too large for the cache, are unchanged. The cached norms add 4 bytes per vector to the cache: a cosine block whose norms would not fit keeps its vectors without them and scores as before.
- **Sorted pages on an indexed field** (`find` with `sort` and `limit`, or an aggregation that starts with a bounded `$sort`) no longer copy the whole index into memory. Results are unchanged.
  - The engine checks with a few index reads that the index order is the sort order. It no longer walks every entry to check.
  - An ascending page stops reading the index at its end: a 10-row page of a 100,000-document collection went from 10 ms to 19 µs (local x86_64).
  - A descending page still reads every index key but keeps only what can reach the page: the same page takes 26–37% less time.
  - Memory is bounded by `skip + limit`, even for a run of equal values spanning the collection.
  - With `find`, documents before `skip` are no longer decoded.
  - Fields the index cannot order — missing values, arrays, mixed integers and floats, NaN or zero floats, field encryption — still scan and sort.
- **Selective unindexed queries** (`find`, `findOne`, paged or sorted `find`, and an aggregation that starts with `$match`, when no index serves the filter) now decode only the fields the filter reads, and decode the rest of a document only when it matches. On documents carrying a 1 KiB string, a query that matches nothing takes 44–52% less time on Linux CI runners (x86_64 and arm64) and allocates 2 instead of 12 times per document. The scan watches its match rate, and goes back to decoding whole documents when most of them match or when the filter reads most of each one, so broad scans cost what they did. Results, their order, and the errors returned for damaged records are unchanged.
- **Unindexed `count`** already decoded only the filtered fields. It is now 20–27% faster, because field names are no longer hashed and the document id is no longer copied. The matching step of `updateMany`, `deleteMany` and filtered vector search uses the same decoder; their total time, dominated by other work, did not measurably change.
- **ANN vector search** takes its candidates' vectors from the decoded vectors that an unfiltered exact search has cached for the field, when they come from the same data, instead of reading each candidate from storage. Scores and results are unchanged. With such vectors cached, a search over 10,000 × 384-d vectors at the native 64 MiB budget took 8–16% less time on arm64 Linux CI runners across the release benchmark runs, depending on `efSearch` (100 to 400). x86_64 runners, which are noisier, measured 7–19% less, except for one run in which the `efSearch` 100 comparison was lost in noise. The cache is usually empty when ANN runs: only an unfiltered exact search fills it, and any write or graph build makes it unusable. Searches without it are unchanged.
- **Exact vector search on indexes larger than the cache** no longer decodes every stored vector on every query. When a field's decoded vectors do not fit the cache budget, the first unfiltered exact search keeps as many of them as fit in otherwise unused cache memory. Later searches read those from memory and decode only the rest. Scores and results are bit-for-bit identical. These partial blocks never displace other cached data. They are not kept while an HNSW graph is cached, and they are given up first when the cache needs room, so ANN search is unchanged. Indexes that fit the cache, and indexes of which less than an eighth would fit, behave as before. Default budgets are unchanged. Whether this helps depends on how often the vectors change between searches:
  - **Read-mostly workloads** gain. At 10,000 × 384-d vectors and the 8 MiB budget used on phones and in the browser, a repeated exact search takes 38–51% less time than in 0.12.1 on Linux CI runners (x86_64 and arm64), and 17–19% less in the browser (headless Chromium) at a 1 MiB budget.
  - **The search that fills the cache costs more.** The first unfiltered exact search after the database is opened, or after memory pressure has emptied the cache, builds the kept vectors in fresh memory. In local x86_64 measurements it took 18–34% longer than before after opening, and up to about twice as long after a memory-pressure cycle or for a second vector field at 8 MiB.
  - **Write-heavy workloads** pay for it. Any vector write makes the kept vectors stale, and the next unfiltered exact search rebuilds them, reusing the stale memory. When every vector write is followed by one exact search, that search takes about 15–17% longer at 64 MiB (50,000 × 384-d) and 10–14% longer at 8 MiB (10,000 × 384-d) than it would without partial retention (local x86_64). With two searches per write, total search time is lower at 64 MiB and about even at 8 MiB; with five, it is 21–44% lower.

### Fixes

- **Indexed queries on strings and byte arrays containing NUL characters** now return what an unindexed scan returns. In the index, `'a\0b'` is stored inside the key range of `'a'`. As a result, `{ s: { $gt: 'a' } }` missed every `'a\0…'` value, and `count({ s: 'a' })` counted them as well. With document ids starting with byte `0xFF`, which `deriveDocId` produces for about 1 in 256 keys, `$lt`/`$lte` bounds containing NUL could also miss shorter values, and a sorted page served from the index could put `'a'` after `'a\0b'`. `find`, `count`, `updateMany` and `deleteMany` were affected alike, as far back as 0.12.1 at least. The index format is unchanged and no rebuild is needed.
- A sorted page on a string field now falls back to scanning and sorting when the index walk reaches a value containing NUL in a collection where some `_id` starts with `0xFF`. Pages that never reach such a value, and every other collection, are still served from the index.
- **Sorting a field that holds NaN no longer panics.** The sort comparator called NaN equal to every number, which is not a consistent order, and the standard library's sort can detect that and panic ("user-provided comparison function does not correctly implement a total order"). It also compared integers with floats through a rounding conversion, so integers beyond 2^53 could sort out of numeric order depending on insertion order. Both affected `find` with `sort`, `$sort`, and `$min`/`$max`, and both date back to 0.12.1 at least.
- **Text and hybrid search no longer panic on NaN scores.** A very large `k1` (such as `3e38`) or opposite infinite RRF weights produce NaN scores, which the rankers compared the same inconsistent way. NaN scores now rank after every other; finite and infinite scores rank as before.

### Behavior changes

- Unsorted results of such a range now come back in index order (by value, integers before floating-point numbers, then `_id`) instead of an order that differed between runs. An unsorted `limit` therefore selects a different, now stable, subset. Sorted queries return exactly the same results as before.
- **Where NaN sorts.** Every NaN, of either sign, now sorts after every number (`Infinity` included) and before strings, equal to every other NaN and ordered by `_id`. Before, its position depended on the input order, or the sort panicked. `$max` of a group holding NaN is now NaN, and `$min` passes over it; before, the result depended on which document came first.
- **Integers beyond 2^53 against floats.** Numbers compare by exact value: `2^53 + 1` now sorts after the float `2^53` instead of tying with it, and `i64::MAX` before the float `2^63`. Orders among other numbers, and the tie of `-0`, `0` and integer `0`, are unchanged.

### Known regressions

- **Indexed equality `count` at 100,000 documents is up to 6% slower on arm64 Linux** than in 0.12.1, about 2.5 µs per call (`doc/count_indexed_eq/100000`). The code on this path is not slower: the difference follows how the compiler lays out the binary. 0.13.0's engine source measured 2% slower, and the version-number change alone added 3%. x86_64 is unchanged. The release performance gate accepts this case on arm64 up to 8% slower than 0.12.1 (a benchmark exception in `benchmarks/gates/exceptions.json`). Every other case still has to pass the gate as before.

Existing public operations, document/index/snapshot/vector formats, cache defaults, and Rust MSRV 1.90 are preserved. 0.12.1 opens databases, indexes, graphs and snapshots written by 0.13.0, so going back to 0.12.1 needs no migration or index rebuild; it brings back 0.12.1's results for the fixes above.

## 0.12.1 — 2026-10-22

- Validate bulk mutation counts before commit in Node, WASM, and direct C bindings. An unrepresentable result rolls back document, index, statistics, and audit changes without emitting a watch event.
- Correct FTS statistics for tokenless documents, reject malformed statistics, and validate snapshot boundaries during restore.
- Bound index-backfill and bulk-mutation intermediate buffers, reduce query result retention, and preserve single-transaction atomicity and existing query ordering.
- Share filter/update parsing across bindings and preserve useful worker parser error messages.
- Update the locked QUIC/TLS dependencies to patched versions and replace the unmaintained YAML parser dependency chain with a configuration-compatible maintained fork.
- Add regression coverage for count overflow/rollback, concurrent candidate invalidation, parser parity, index/query consistency, snapshots, and YAML configuration compatibility.

Existing public operations, document/index/snapshot/vector formats, cache defaults, and Rust MSRV 1.90 are preserved. Platform runtime validation and constrained-memory deployment limits remain release prerequisites.

## 0.12.0 - 026-10-03

### Performance

- **Shared cache budget:** one configurable budget now covers exact vectors and HNSW graphs (8 MiB on Android, iOS and WASM; 64 MiB elsewhere), with clock eviction within graphs and LRU between indexes.
- **Adaptive budgets:** Node, WASM and FFI can size budgets from host memory hints and react to moderate or critical memory pressure, recovering afterwards. Fixed budgets still take precedence.
- **Bounded search memory:** ANN walks reserve memory before allocating. A walk that cannot fit falls back to exact search on the same snapshot (`execution.reason: "memoryBudget"`).
- **Filtered vector search:** filters resolve from secondary-index keys where possible, choose the cheapest intersection strategy, and stream large matches instead of holding every ID. Grouped results keep only the requested page.
- **Binary-quantized HNSW:** queries keep full precision during traversal, improving candidates without a rebuild, and construction compares packed codes directly.
- **Fewer HNSW allocations:** traversal reuses buffers, visited sets and heaps; resumable builds reuse decoded nodes between steps.

### Search and queries

- **Stopwords in ranked full-text search:** `searchText` and the text side of `hybridSearch` drop common English function words ("to", "the", "and", …) from the query. Under OR semantics a query like "kind to a classmate" matched every document containing "to"; every app had to strip these itself. Filtering happens at query time only, so existing FTS indexes need no rebuild, and a query made only of stopwords is searched as typed. Turn it off with `stopwords: false` in the BM25 options (Node, web, FFI, Kotlin) or `Bm25Params { stopwords: false, .. }`. Rankings for queries that contain stopwords change.
- **Projection can exclude fields:** `FindOptions` gains `exclude`, the complement of `fields`, for leaving out one large field such as an embedding. `_id` is always returned.
- **Projected live queries:** `Collection::watch_with_options` and the C FFI's `taladb_watch_with_options` apply `FindOptions` to every snapshot, so a screen watching documents that carry embeddings no longer receives every vector on every write. `taladb_watch` is unchanged and now delegates to it. The FFI `find` operation accepts the same `{"fields", "exclude"}` options. The additions are backwards compatible, so the ABI version stays 2.
- Rust callers that build `FindOptions` or `Bm25Params` with struct literals and no `..Default::default()` must add the new fields.

### Other

- The engine is now the `taladb` crate on crates.io, usable as the local database of any Rust application.
- Fixed retained HNSW nodes surviving graph replacement.
- Fixed indexed two-sided ranges on arrays and inclusive bounds at signed zero.
- Node now rejects malformed opening configs instead of ignoring them.
- Added browser worker/OPFS benchmarks (phone-sized budgets, reopen, latency, filtered recall, memory pressure) and native CI comparisons for filtered search, quantized recall and builds.

## 0.11.9 — 2026-10-02

Groundwork for first-party Kotlin and Swift packages that wrap the C FFI directly, without React Native. Those packages live in their own repositories (`taladb/taladb-kotlin`, `taladb/taladb-swift`) and consume prebuilt libraries from the GitHub release, so the C interface now carries an explicit version.

- **Breaking for C callers (ABI version 2):** `taladb_create_index`, `taladb_drop_index`, `taladb_create_compound_index`, `taladb_drop_compound_index`, `taladb_create_fts_index` and `taladb_drop_fts_index` return `int32_t` (1 on success, -1 on error) instead of `void`. Four of them discarded the core's error outright and the compound pair failed silently on a malformed field list, so dropping an index that did not exist "succeeded" through the FFI while the same call threw from Node.js and from the binding's own async path. The React Native HostObject now throws on these failures, which brings its synchronous index methods in line with every other binding.
- Added `taladb_ffi_abi_version()` and the `TALADB_FFI_ABI_VERSION` constant. A wrapper built in another repository compiles against one release's header and may load another release's library; a changed signature links fine and corrupts the stack at run time, so each wrapper checks this once at load.
- Added `taladb_call(handle, op, args_json)`, the synchronous twin of `taladb_call_start`. It runs the same operation table on the calling thread, which gives callers that do their own threading every operation — including `listIndexes` and `vectorCommand`, which had no direct export — without a job handle to poll.
- Added live queries to the C FFI: `taladb_watch`, `taladb_watch_next` and `taladb_watch_close`, built on the core's event-driven `WatchHandle`. `taladb_watch_next` waits with a timeout, so a caller can stop a subscription without a write arriving; writes since the last call coalesce into one snapshot. No binding used `WatchHandle` before — the TypeScript client polls `find` every 300 ms — so the Kotlin and Swift packages are its first consumers. Added `WatchHandle::next_timeout` to the core for this.
- Fixed live queries ending silently after a burst of writes. When more than 64 writes landed before a subscriber read, `notify` dropped the subscriber, and its next read failed with `WatchClosed`. Events carry no payload and every read re-runs the query, so a full channel already guarantees the subscriber wakes to the latest state; it is now kept, which is what the module documentation already promised.
- Fixed `taladb_find_nearest`, `taladb_search_text` and `taladb_hybrid_search` treating a filter that was not valid UTF-8 as no filter, matching every document instead of failing. A NULL filter — which does mean "no filter" — also left a "required string argument is null" message in `taladb_last_error` after a call that succeeded.
- Moved the FFI crate from `packages/bindings/react-native/rust` to `packages/bindings/ffi`, since it now has three consumers. The canonical header is `packages/bindings/ffi/include/taladb.h`; the React Native package keeps a byte-identical copy because an npm package cannot reach outside its own directory. `packages/bindings/ffi/scripts/gen-header.sh` writes both, and CI fails if either is stale.
- Added a compile-time assertion that the FFI handle is `Send + Sync`, and a test driving one handle from eight threads. React Native only ever called a handle from the JS thread, so nothing enforced what the native wrappers depend on.
- The release now builds `TalaDBFFI-<version>.xcframework.zip` for the Swift package — iOS device, iOS simulator and macOS slices, with `taladb.h` and a `module.modulemap` so Swift can `import TalaDBFFI` — attaches it with its SwiftPM checksum, and notifies `taladb/taladb-swift`. The React Native xcframework is unchanged.
- The release now attaches `taladb-ffi-android-<version>.zip` (the per-ABI `libtaladb_ffi.so` files and the header) with its SHA-256, and notifies `taladb/taladb-kotlin` when `NATIVE_PACKAGES_DISPATCH_TOKEN` is configured. The 16 KB page size link flag for the Rust Android library is now set in `.cargo/config.toml` rather than left to the cargo-ndk and NDK defaults, and the release build asserts the alignment with `readelf`.
- Gave `libtaladb_ffi.so` a SONAME on Linux and Android, through a `build.rs` that applies only to the FFI crate's own cdylib. rustc links a cdylib without one, so a library linked against it records the path it was handed: CMake passes an absolute path for an `IMPORTED` target, and the Kotlin package's JNI shim came out depending on the build machine's `/home/.../libtaladb_ffi.so`, which no device can load. The React Native glue links by name and was not affected.
- Stopped shipping unused `libredb-<hash>.so` files in the React Native package's `jniLibs`. `cargo ndk -o` copies every cdylib in the build, and redb 2.x — the legacy-migration dependency — declares one; `libtaladb_ffi.so` links redb statically and never loads it, so each Android app carried roughly 500 KB of dead code per 64-bit ABI.
- Fixed the remaining cause of the flaky `starting_more_jobs_than_the_cap_is_refused_not_piled_up` test. The 0.11.8 fix serialised the tests that spawn jobs through `spawn_job`, but three more start real jobs through `taladb_call_start` and still ran unserialised; they now take the same lock.

## 0.11.8 — 2026-09-20

- Roughly halved cosine vector scoring by splitting the fused dot-product-and-norm pass into two loops. The two `[f32; LANES]` accumulators shared one loop body, which exceeded what LLVM's SLP vectoriser would hold in registers, so it emitted the whole reduction scalar — on aarch64 and wasm it produced almost no vector instructions at all. Splitting the passes costs one extra walk over data already in L1 and restores vectorisation on every target. `vector/score_one/Cosine` improved 53–62% across 128–1536 dimensions and end-to-end `vector/find_nearest` improved 45% at 1,000 vectors and 50% at 10,000, measured on x86_64; aarch64 and wasm were affected more severely and should gain at least as much. Dot and Euclidean scoring were already single-accumulator and are unchanged.
- Retained decoded HNSW nodes across queries instead of rebuilding the cache per query, cutting ANN latency by ~4.6x at 10,000 vectors (`efSearch` 100: 8.31 ms to 1.80 ms; `efSearch` 400, the 99.5%-recall setting: 25.00 ms to 5.92 ms). Recall is unchanged at every setting — this removes repeated storage reads and postcard decodes, which measured at ~97% of ANN query time against under 3% for scoring itself. The cache is shared by every collection handle from one database, pinned to the vector revision its graph was built against, and evicted explicitly on index create, drop and rebuild. A query borrows it out of the shared map rather than holding the lock for the duration, so concurrent queries on one graph do not serialise; the borrow is returned by `Drop`, which covers the several fallible steps inside the retry loop and an unwind alike. Returning it also checks the revision, so a rebuild that evicts and repopulates the slot while a query is still holding the older nodes is not undone by that query finishing. The per-graph budget is 64 MiB on native and stays 8 MiB on WASM, where holding tens of megabytes is not an option; the old 8 MiB bound was sized for a cache that lived for a single query and made a 10,000-vector graph fill, flush and refill.
- Fixed a flaky test in the React Native FFI crate. `starting_more_jobs_than_the_cap_is_refused_not_piled_up` fills the process-wide job cap to prove the next spawn is refused, but three other tests in the same binary also spawn jobs — run in parallel, another test holding a single permit meant the cap never filled, the job past it was accepted, and the assertion failed. It failed roughly two runs in three here while passing every time in isolation. The four tests that spawn jobs now share a mutex, which keeps the rest of the crate parallel; `--test-threads=1` would have made every test pay for one test's need for exclusivity. The job cap itself was never wrong, and this predates 0.11.8 — it is fixed here because it surfaced while verifying this release.

- No behaviour change to scoring: both totals are bit-identical to the fused loop, including on the length-mismatch path, and are pinned by tests against the previous implementation.
- Built the browser module with WebAssembly SIMD (`-C target-feature=+simd128`). The lane-split scoring loops had no vector instructions to compile into on the wasm baseline, so the browser was running them scalar and getting none of the benefit the native targets get for free. The feature is set in `.cargo/config.toml`'s `rustflags` array rather than a `RUSTFLAGS` environment variable, because the environment variable replaces that array instead of merging with it and would silently drop the `getrandom_backend` cfg the wasm build depends on. `wasm-opt` is given `--enable-simd` to accept the result.

  **This raises the browser floor.** A WASM module that uses SIMD does not instantiate on an engine without it, so the minimum is now Safari 16.4, Chrome 91 and Firefox 89 — SIMD support, not OPFS, now sets the floor for every storage backend. The support tables in the web guide and the `@taladb/web` README previously disagreed with each other (16.4+ versus 15.2+) and have both been corrected.

## 0.11.7 — 2026-09-15

- Removed `android.permission.INTERNET` from the React Native binding's manifest. The manifest merger granted it to every consuming application, so an app that never opens a socket still shipped — and had to justify on its store listing — a network permission it did not use. The sync feature that once needed it was removed in 0.11.0; the binding opens no socket and the FFI crate pulls in no network dependency. Apps that need the permission can declare it themselves.

## 0.11.6 — 2026-09-14

- Fixed `libtaladb_jsi.so` being linked with 4 KB page alignment, which made any app bundling the React Native binding fail Google Play's 16 KB page size requirement for Android 15 and later, and prevented it loading at all on a 16 KB device. The C++ glue now links with `-Wl,-z,max-page-size=16384` explicitly; the prebuilt Rust libraries were already aligned.
- Stopped the Android binding pinning `ndkVersion` to r27 unconditionally. It now honours `ext.ndkVersion` from the consuming app's root `build.gradle` — which the React Native template sets — and falls back to r27.1 only when the app declares none. Pinning it forced every consumer onto an NDK whose default page alignment is 4 KB, regardless of what the rest of their application was built with.

- Added a CI check that links the Android glue against NDK r27 and asserts the result is 16 KB aligned. The existing job compiled both translation units but never linked them, so the defect above passed every check it had.

No API, storage-format or behaviour change: this is a link-time fix. Android consumers should rebuild to pick it up; a clean build of the `:taladb_react-native` module is needed because the object is cached per CMake configuration.

## 0.11.5 — 2026-09-13

- Raised HNSW recall by building layer 0 with `M_max0 = 2M` links, matching the algorithm and the pruning limit the same function already applied; recall@10 rose from 91.5% to 97.5% at efSearch 100 over 2,000 vectors, and from 93.5% to 99.5% at efSearch 400 over 10,000.
- Cut HNSW graph construction time by roughly 12%: 12.0s to 10.8s at 2,000 vectors and 155s to 136s at 10,000, both at 384 dimensions.
- Removed a heap clone of the stored vector on every distance computation by borrowing from the graph's decoder instead of returning an owned copy.
- Cached each node's squared norm so cosine scoring no longer recomputes it per comparison, and replaced SipHash with a multiplicative hash for internal graph node ids.
- Shared one node cache across an insert's two readers, across its back-linking pass, and across ANN retry attempts, removing repeated storage reads and decodes of nodes already in memory.
- Carried the full candidate set between layers during insertion instead of only the nearest, per Malkov/Yashunin algorithm 1.
- Added `cargo run -p taladb-core --example hnsw_profile` for measuring build time, query latency and recall against exact ground truth, with a cluster-spread argument for separating index quality from data difficulty.
- Documented the remaining graph-traversal cost and recall-at-scale gaps in the roadmap, and corrected a stale entry that described HNSW as available only in the Node binding.

No storage-format change: existing graphs open and search without a rebuild. Rebuilding an index built by an earlier version picks up the layer-0 improvement.

## 0.11.4 - 2026-09-12

- Added full-text, vector, hybrid, and aggregation queries to the React Native TypeScript surface.
- Replaced the memory-only `instant-distance` layer with portable, transactional HNSW graphs that persist and update on browser, Node.js, and React Native.
- Added configurable graph construction and query-time `efSearch`, exact rescoring, scalar/binary quantization, explicit exact/ANN modes, score thresholds, pagination, and grouping.
- Added persistent index status plus resumable, cancellable batch rebuilds with progress reporting; `db.rebuildVectorIndexes()` remains available for maintenance and legacy migration.
- Added filtered ANN as an explicit opt-in while keeping filtered queries exact by default, plus recall measurement against exact ground truth.
- Fixed the `build:cbindgen` script and config, which regenerated an unusable FFI header.
- Documented persistent HNSW lifecycle, mobile-safe rebuild scheduling, hybrid search, and package choice in the React Native guide.
- Added a CI job that compiles the React Native C++ glue and checks the generated FFI header.

## 0.11.3 — 2026-09-06

- Added authoritative single-owner browser storage and multi-tab RPC.
- Propagated IndexedDB and OPFS failures and bounded browser snapshots.
- Made schema, index, full-text, and vector reads snapshot-consistent.
- Bounded flat vector search memory and added exact fallback for stale HNSW indexes.
- Hardened mutation invariants, vector validation, and React Native async jobs.
- Expanded cross-runtime reliability tests and CI coverage.

## 0.11.2 — 2026-08-28

- Raised the Rust baseline to 1.90 and upgraded to redb 4.2 storage format v3.
- Added legacy storage migration and JSON-depth validation.
- Hardened vector limits, snapshots, passphrase handling, regex filters, and the C FFI.
- Improved array-index maintenance, OPFS validation, and React Native job limits.
- Added MSRV, fuzz, Wasm, and Miri checks.

## 0.11.1 — 2026-08-16

- Fixed Node adapter resolution in browser builds.
- Fixed shared database handling under React StrictMode.
- Fixed concurrent and duplicate hydration.
- Added document-ID validation and shared-handle reset helpers.

## 0.11.0 — 2026-08-15

- Removed sync, replication, conflict-resolution, and sync-backend APIs.
- Added change webhooks, queryable arrays, caller-supplied IDs, and `isPrimary()`.
- Renamed React `useMutation` to `useWrite` and config `sync` to `webhook`.
- Fixed browser multi-tab writes, live queries, owner handoff, and vector dimensions.
- Reduced unnecessary index rewrites.

## 0.10.2 — 2026-08-02

- Accelerated vector scoring, range scans, indexed queries, counts, and browser startup.
- Fixed LWW timestamp abuse, invalid regex handling, vector dimensions, and snapshots.
- Fixed large-number ranges, pagination overflow, React Native errors, and OPFS fallback.
- Improved storage error context, value APIs, benchmarks, lints, and crate documentation.

## 0.10.1 — 2026-08-01

- Added a decoded-vector cache for faster repeated flat search.

## 0.10.0 — 2026-07-25

- Added BM25 full-text search and ranked hybrid search.
- Added Node full-text APIs and richer schema downgrade controls.
- Upgraded full-text index storage.
- Fixed browser fallback values and migration write-back safety.

## 0.9.4 — 2026-07-13

- Added batched ID-based replication writes and deletes.
- Added live aggregation, derived IDs, cursor sync, coverage, and REST replication helpers.
- Added React and Next.js replication APIs.
- Fixed replication echo, configured collection resolution, and API documentation.

## 0.9.3 — 2026-07-12

- Added index-backed pagination and bounded top-K sorting.
- Added projection exclusion and deterministic sort ordering.
- Improved sort performance and union-field filter types.
- Fixed React hooks bypassing collection configuration.

## 0.9.2 — 2026-07-12

- Added import validation, quarantine, document versions, renames, and migrations.
- Added configurable durability, explicit flush, and native migration accessors.
- Applied migrations and validation consistently to reads and subscriptions.
- Fixed quarantine persistence and migration write-back behavior.

## 0.9.1 — 2026-07-12

- Added scoped React replication hooks.
- Added background prefetch for local replicas.

## 0.9.0 — 2026-07-11

- Added browser encryption, compound indexes, browser sync, and React Native sync plumbing.
- Added first-party Next.js sync support, React integration, examples, and end-to-end tests.
- Doubled flat vector-search throughput and bounded two-sided range scans.
- Fixed browser locking, encrypted cross-tab handling, cursors, Node opening, and bundling.

## 0.8.4 — 2026-07-11

- Added bidirectional Node sync and an HTTP sync adapter.
- Added aggregation across all runtimes.
- Added the MongoDB sync adapter.

## 0.8.3 — 2026-07-09

- Enabled HNSW in published Node binaries.
- Added Node and browser benchmark suites.
- Published benchmark results and tuning guidance.

## 0.8.2 — 2026-06-12

- Secured Studio binding, React Native filter parsing, encryption AAD, and ULID entropy.
- Fixed LWW, tombstone, CRDT, numeric-index, index-cache, and mutation races.
- Made audit writes atomic and rekey operations resumable.
- Added core live queries, atomic ID replacement, async Node writes, and primary-key plans.
- Aligned operators, errors, and index metadata across bindings.

## 0.8.0 — 2026-05-14

- Added field-level CRDT sync with deterministic clocks.
- Added grow-only set fields, incremental export, tombstones, and multi-replica merge.

## 0.7.10 — 2026-04-19

- Fixed React Native platform detection and Metro/Hermes bundling.
- Aligned the TypeScript adapter with the native JSI API.

## 0.7.9 — 2026-04-19

- Fixed Android native-library loading and Kotlin compilation.

## 0.7.8 — 2026-04-19

- Fixed React Native C++ header compilation.

## 0.7.7 — 2026-04-19

- Fixed the Android C++ runtime configuration.

## 0.7.6 — 2026-04-19

- Added the missing Android Gradle, codegen, and manifest configuration.

## 0.7.5 — 2026-04-19

- Moved runtime adapters to optional peer dependencies.
- Added a dedicated React Native export for Metro.

## 0.7.4 — 2026-04-18

- Added zero-copy `Float32Array` vector queries on React Native and Node.
- Added background vector and full-scan queries.
- Added React Native vector-index management.

## 0.7.3 — 2026-04-17

- Added database compaction across runtimes.
- Added Cloudflare Durable Objects and Bun support.
- Added the local Studio UI.
- Added Zod and Valibot collection validation.

## 0.7.2

- Added database rekeying and per-field encryption.
- Added append-only mutation audit logs and readers.

## 0.7.1

- Added query timeouts and tracing spans.
- Added snapshot and filter fuzz targets.

## 0.7.0

- Made collection creation fallible with eager name validation.
- Added encrypted-format migration and a snapshot size guard.
- Added structured tracing and bounded webhook workers.

## 0.6.1 — 2026-04-13

- Added automatic change timestamps, tombstones, and indexed change export.
- Added bidirectional Wasm changesets and collection listing.
- Added secondary-tab write propagation and debounced IndexedDB snapshots.
- Added tombstone compaction and sync type exports.

## 0.6.0 — 2026-04-12

- Added HTTP push sync.
- Fixed React Native Android and iOS native packaging.
- Fixed browser bundle resolution and pinned the Android NDK.

## 0.5.0 — 2026-04-12

- Added first-party React live-query hooks.
- Expanded package and release infrastructure.

## 0.4.0 — 2026-04-11

- Added full-text and HNSW vector indexes.
- Added index introspection and query-planner strategies.
- Fixed full-text parsing, index idempotency, and multi-tab OPFS locking.

## 0.2.1 — 2026-04-05

- Fixed ID-based sync, corrupted key handling, and encryption errors.
- Added index metadata caching and faster OR and full-text plans.
- Aligned adapter error reporting and watch backpressure.

## 0.2.0 — 2026-04-05

- Added flat vector indexes with cosine, dot-product, and Euclidean search.
- Added filtered nearest-neighbor search across browser, Node, and React Native.

## 0.1.2 — 2026-03-30

- Fixed release publishing being blocked by crates.io.

## 0.1.1 — 2026-03-30

- Fixed adapter bundling and a Rust lint.

## 0.1.0 — 2026-03-30

- Released the Rust core, CLI, and TypeScript packages.
- Added document queries, updates, indexes, full-text search, and live subscriptions.
- Added browser, Node, React Native, encryption, migration, and snapshot support.
