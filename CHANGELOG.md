# Changelog

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
