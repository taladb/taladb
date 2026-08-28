# Changelog

All notable changes to TalaDB will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.11.2] - 2026-08-28

A security and correctness pass over the Rust workspace, plus the move to redb 4.
Ten findings from an audit of the engine, the FFI layer and the browser backend,
each with a regression test confirmed to fail against the unfixed code.

Two of them changed shape once tested rather than reasoned about, and the notes
below say so where it matters — including one finding whose severity was
overstated in the original report.

### Breaking

- **Minimum supported Rust version is now 1.90**, up from 1.88. redb 4.2 requires
  it and no 4.x release accepts less than 1.89, so this is the cost of the
  storage engine bump rather than a choice about language features.

- **`Filter::matches` and `Filter::matches_with_cache` now return
  `Result<bool, TalaDbError>`.** See the regex fix under *Fixed*. Callers using
  the engine as a Rust library need a `?` or an explicit match; the JavaScript,
  wasm and React Native bindings are unaffected, as they never called these
  directly.

- **`set_durability(true)` no longer persists on its own.** redb 4 removed the
  durability level this mapped to, leaving only "immediate" and "none". Batched
  writes now reach disk when `flush()` is called and not before — which is what
  the API already documented, but redb 2 used to persist them eventually anyway.
  Anything relying on that undocumented safety net needs an explicit `flush()`.

### Storage format

- **Upgraded to redb 4.2 (on-disk format version 3).** Existing databases are
  **migrated automatically on open** and are not left behind:

  - *Native* — the file is copied to `<name>.v2.bak`, converted in place, and
    opened. The backup is kept because the conversion being crash-safe is not the
    same as it being correct, and a database is not something a user can
    regenerate.
  - *Browser (OPFS)* — converted in place through the same file handle, with no
    change to the JavaScript API. No backup is taken, because there is nowhere to
    put one without a second OPFS file; export a snapshot first if the data is
    irreplaceable.

  The upgrade is one-time and a no-op afterwards. It is carried by the new
  `legacy-migration` feature, on by default; see *Added*.

### Fixed

- **Two arithmetic-overflow panics on an unvalidated `top_k`.** A vector search
  with an extreme `top_k` overflowed while sizing its working set, aborting the
  process rather than returning an error.

- **A panicking background job left React Native polling forever.** Completion
  was signalled by the last statement of the worker closure, so a panic in the
  engine skipped it: `taladb_job_poll` reported "still running" indefinitely and
  the JavaScript side — which polls until done before it will collect a result —
  span on `setImmediate` with no way to learn the job had died. Completion is now
  signalled by a `Drop` guard, on every exit path including an unwind, and the
  panic message is surfaced instead of an empty result.

- **No `catch_unwind` on any of the 37 `extern "C"` entry points.** A panic
  unwinding across the FFI boundary is undefined behaviour. All of them now
  contain a panic and report it, except `taladb_last_error`, which is a
  thread-local pointer read that cannot panic and whose guard would be circular —
  it writes the slot it reads.

- **A mistyped passphrase key silently opened an unencrypted database.** An
  unrecognised key in the encryption config block was ignored, so
  `passphrase`/`passPhrase` — or any typo — produced a working, readable,
  completely unencrypted database with no warning. Unknown keys in that block are
  now an error.

- **A crafted snapshot could abort the process.** `restore_from_snapshot` passed a
  table name straight from the buffer to redb, which documents "name must not be
  empty" and enforces it with an assertion — so a 48-byte snapshot with an empty
  name panicked inside the one function whose entire contract is to return an
  error for untrusted bytes. Found by the snapshot fuzz target the first time it
  was ever able to run (see *Tooling*).

- **Index maintenance was quadratic in the length of an indexed array.** Key
  de-duplication used a linear scan per element, so indexing a large array field
  cost time proportional to its length squared. Now a hash set, with element
  order unchanged: **16,000 elements went from 7.96 s to 0.107 s**.

- **A regex that failed to compile could invert `$not` into "match everything".**
  Two separate filter evaluators had drifted apart on how they handled a pattern
  that would not build; one returned `false`, which `$not` turned into a filter
  matching every document. Reachable only through the public `Filter::matches` —
  every query path already validated the whole filter tree up front, so no
  binding could hit it, and **the original report overstated this**. The two
  evaluators are now one, which is also why they could disagree in the first
  place, and the 1 MiB pattern ceiling is declared once instead of twice at
  different values.

- **The OPFS backend trusted the byte count JavaScript handed back.** A
  non-numeric, negative or over-long return became a silent short read, a
  heap-exhausting allocation, or a panic on a length mismatch. It is now validated
  against the request. The `RefCell` borrow is also no longer held across the call
  into JavaScript, where a re-entrant handle would have turned it into a panic.

- **No depth limit on caller-supplied JSON.** Nested `$and`/`$or`/`$not` drove
  unbounded recursion in the filter, update and document parsers — a stack
  overflow, which aborts and cannot be caught. Nesting is now capped at 64 levels
  by an iterative check (a recursive one would overflow on exactly the input it
  exists to reject). In the browser the check runs on the JavaScript value
  *before* deserialisation, which also bounds the deserialiser's own recursion and
  the recursive drop on the error path. **The Node binding is not yet covered**:
  napi converts at the function signature, before any TalaDB code runs.

- **Unbounded OS-thread spawning in the React Native async API.** Every
  `*_start` call spawned a thread with no ceiling, so a caller issuing a query per
  animation frame spawned one per frame. Concurrency is now capped (twice the core
  count, 4–16), released even when a job panics, and a start call past the cap
  fails with a readable message rather than blocking the JavaScript thread.

- **`unsafe impl Send`/`Sync` on the OPFS backend is now enforced by the target.**
  The impls cover a `RefCell`, and their soundness argument is that wasm is
  single-threaded — previously asserted in a comment that nothing checked. They
  are now gated on `wasm32` without the `atomics` feature, and enabling wasm
  threads is a compile error naming the reason.

### Changed

- **`serde_yaml` replaced with `serde_yaml_ng`.** The original was deprecated by
  its author and would never receive a fix. Note that `serde_yaml_ng` still
  depends on the unmaintained `unsafe-libyaml`, so this replaces the wrapper and
  not the parser; `serde_norway` is the alternative that forked both. YAML config
  parsing is unchanged.

- **`reqwest` moved to `[dev-dependencies]` in the CLI.** Every use of it was
  inside `#[cfg(test)]`, behind a comment claiming `taladb studio` proxied
  requests through it, which it had stopped doing. It and its TLS stack are no
  longer in the shipped binary's dependency graph.

- Dependency tree reduced from 290 to 282 crates by the audit's dependency pass;
  the redb 2 reader needed for the migration brings it to 283.

### Added

- **`legacy-migration` feature** (`taladb-core`, `taladb-web`), on by default. It
  carries the redb 2 reader needed to open pre-0.11.2 databases. In the browser it
  costs **506 KB of wasm** (2.68 MB → 3.17 MB), so an application with no existing
  installs can build `--no-default-features` and reclaim all of it.

- **`json_depth::check_json_depth`** and `MAX_JSON_DEPTH`, for callers that accept
  untrusted JSON and want the same ceiling the bindings apply.

- **`migrate::upgrade_legacy_backend`** for custom `StorageBackend`
  implementations. It must only be called after an ordinary open has failed with a
  legacy-format error — redb 2 panics on a database redb 4 wrote, rather than
  declining it.

### Tooling

Three classes of test in this repository looked like coverage and were not
running at all. All three now run in CI and gate the build.

- **`cargo fuzz` had never built.** The fuzz crate was misconfigured as part of
  the parent workspace, so every invocation failed before reaching a target. Fixed,
  and the first run found the snapshot panic above. Three new targets drive the
  JSON parsers — where untrusted structure actually enters — from a committed seed
  corpus.

- **Twelve `#[wasm_bindgen_test]` tests had never run.** They were configured to
  require a browser, and no job ran them. None of them need one; all seventeen now
  run under node.

- **The declared MSRV was never checked.** The pinned toolchain compiles anything,
  so the floor moving from 1.88 to 1.90 was invisible until checked by hand.

- **Miri** now runs over the FFI's raw-pointer layer under both aliasing models.
  It is scoped to that layer deliberately: redb is far too much work for an
  interpreter, and a job that always times out is the same as no job.

## [0.11.1] - 2026-08-16

Fixes from the first real application migration onto `@taladb/react/query`. All
three problems below cost debugging time in an app where the layer otherwise
worked, and none of them failed at compile time.

### Fixed

- **`@taladb/node` no longer breaks web builds.** The adapter is selected at
  runtime, so the browser bundle contains an `import('@taladb/node')` on a
  branch it never takes. Bundlers resolve dynamic specifiers statically, and
  that optional peer dependency is not installed in a web app — Next.js failed
  with `Module not found: Can't resolve '@taladb/node'` during Client Component
  SSR, which the `browser` export condition does not cover because that bundle
  renders in Node. The specifier now carries `webpackIgnore` and
  `turbopackIgnore` comments, so webpack, Turbopack, Vite and Metro all resolve
  it without an alias. Node is unaffected.

- **`TalaDBProvider` no longer loses writes under React StrictMode.** `openDB`
  is async, so a StrictMode mount-unmount-mount left two opens of the same
  database in flight at once. Only one can hold the OPFS lock; the other
  concluded it was a second tab, fell back to an IndexedDB snapshot, and
  forwarded its writes to a primary that the teardown then closed — so the
  writes vanished, with no error and nothing to see but missing data. Next.js
  enables StrictMode in development by default, which made this the common case
  rather than an edge one. Handles are now shared and reference-counted per
  `(name, options)`, with the close deferred so a remount in the same tick
  reclaims the live handle. Two providers naming one database now share it too,
  which is what a client-side navigation does between pages.

- **Concurrent hydrations of one collection no longer collide.** `hydrate`
  reads which ids exist and then writes, with no transaction spanning the two,
  so two overlapping calls both read "absent", both inserted, and the loser
  failed the whole response with `duplicate document id`. Calls are now
  serialised per collection handle.

- **Hydration no longer fails on a document repeated inside one response.**
  `hydrate` de-duplicated against what was already stored but not within the
  incoming array, so two copies of a row both reached `insertMany` and the whole
  call died with `duplicate document id` — taking every other document in that
  response with it. Repeats are ordinary: shelves merged for one write,
  overlapping pages, a feed joined across categories. Copies now collapse by
  `_id`, last one winning, at the position the id first appeared.

- **Hydration reports an unusable `_id` properly.** `hydrate` validated only
  that `_id` was a non-empty string, then let the engine reject it with a
  message written for someone inserting a document by hand. Passing a natural
  key straight through from a REST endpoint — `"265"`, a slug, a UUID — is the
  single most likely first mistake in a migration, so it now fails in the query
  layer with the collection, the offending value, and the `deriveDocId` call to
  make instead.

### Added

- **`isDocId(value)`** — whether a value is storable as an `_id`. Mirrors the
  engine's decoder exactly, including its case-insensitivity, so it never
  rejects an id the database would accept.
- **`resetSharedDatabases()`** — drops every shared handle. For test suites,
  which would otherwise inherit the previous case's instance.

### Documentation

- The query and migration guides said `_id` need only be **a string**. Both now
  state the ULID requirement up front and show the `deriveDocId` mapping.
- The query guide claimed the provider "works in Next.js without touching
  server rendering". The opposite is true: nothing beneath `TalaDBProvider` is
  server-rendered, and there is no `dehydrate`/`HydrationBoundary` equivalent.
  Both consequences are now documented, along with the advice not to wrap a
  root layout or an SEO-critical page.

### Internal

- Unit and parity fixtures used ids like `'a'` against mock collections that
  enforced nothing, so every test passed with values the real engine refuses.
  Fixtures now derive real ULIDs — the gap that let the `_id` bug ship.

## [0.11.0] - 2026-08-15

0.11.0 narrows TalaDB to its core: an embedded document + vector database, and
the work to harden it. Dropping replication is the first step — it frees the
surface area that hardening has to cover.

### Removed — BREAKING

All sync and replication, across every layer: the changeset APIs, conflict
resolution, the replication coordinator and its React hooks, the sync-backend
adapter packages, and the CLI's sync command.

A half-built sync engine is worse than none. It put TalaDB on the most crowded
shelf in the ecosystem while taking time from the vector core, where it is the
only embedded database serving browser, React Native, and Node.js from one API.

**Migrating:** use the change webhook for the push direction. There is no
replacement for the pull direction — a database that does not replicate cannot
pull. Hydrating from a server is now ordinary application code: fetch, then
`insertMany`. In React, use `useFind` / `useFindOne` / `useAggregate`;
`useWrite` (formerly `useMutation`) remains as a plain local-write hook.

### Added

**Change webhook.** Every committed mutation fires one HTTP request — `POST` on
insert, `PUT` on update, `DELETE` on delete.

```ts
const db = await openDB('app.db', {
  webhook: {
    enabled: true,
    endpoint: 'https://api.example.com/taladb',
    headers: { Authorization: `Bearer ${token}` },
    exclude_fields: ['embedding'],
  },
})
```

Per-document ordering is preserved, the bounded queue drops rather than blocking
the writer, and failures retry with backoff on 5xx and network errors but never
on 4xx. Delivery is best effort: retries reuse an `event_id` / `Idempotency-Key`
so receivers can deduplicate, while a crash can still lose queued events. Adds
`db.webhookStats()` and `db.flushWebhook()`; `db.close()` drains.

`document` is the stored post-image on insert/update and the last pre-image on
delete — `_changed_at` and `_v` included — and the key exists on all verbs.
`timestamp` is captured immediately when the commit returns, before payload
reads, so a backlog or retry does not move it. Each mutating call costs one extra
batched document read, not one read per affected document.

It replaces the previous Rust implementation, which existed in three variants
across the runtimes and was entirely absent from the browser's in-memory
fallback. The new one is a single `fetch`-based implementation in the TypeScript
client, so all three runtimes behave identically. See [/api/webhook](https://taladb.dev/api/webhook).

**Array fields are queryable.** `$push` and `$pull` have always maintained
arrays, but nothing could look inside one — a comparison ran against the array as
a single value, so a field holding `['rust', 'db']` matched nothing but an
identical list, silently.

```ts
await posts.find({ tags: 'rust' })                     // any element matches
await posts.find({ tags: { $in: ['rust', 'swift'] } }) // any element, any candidate
await posts.find({ scores: { $gte: 90 } })             // any element ≥ 90
await posts.find({ tags: ['rust', 'db'] })             // exact list, as before
```

Every operator reaches into the list — `$eq`, `$in`, `$ne`, `$nin`, the range
comparisons, and `$regex` — and a document is returned once however many of its
elements match. `createIndex('tags')` now writes one entry per element, so these
are point lookups rather than scans; an indexed and an unindexed collection are
required to answer identically, which is what the new `array_containment` suite
tests. Nested arrays are not flattened, and a compound index covers at most one
array field (two would put the product of the lists in the index, so the write is
refused). See [/api/filters](https://taladb.dev/api/filters#array-fields).

**`insert` accepts an `_id`.** It used to be discarded — the web and React Native
bindings filtered it out, Node passed it through as a field the read path then
overwrote. A document inserted as `{ _id: … }` came back under a fresh ULID and
`find({ _id: … })` matched nothing.

This matters for the hydration path this release recommends in place of
replication: fetch, then `insertMany`. Without a caller-controlled id that code
duplicates every row on a second run.

```ts
const rows = await fetch('/api/products').then((r) => r.json())
await products.insertMany(
  rows.map((row) => ({ ...row, _id: deriveDocId('products', row.sku) })),
)
```

The id must be a ULID — ids are 16 raw bytes on disk and every index key ends
with one — so `deriveDocId(collection, key)` maps a natural key to a stable ULID,
and a non-ULID `_id` is refused with a message pointing there. Inserting an id
that already exists is a `DuplicateId` error, never an overwrite: re-running a
seed cannot clobber what the application has since written.

**`db.isPrimary()`** — whether this tab's writes land authoritatively, or are
forwarded to the tab that owns the storage.

```ts
if (await db.isPrimary?.() ?? true) {
  await drainOutbox();
}
```

The primary/secondary split already governed every browser write; there was no
way to ask which side you were on. Anything that must run in exactly one tab, or
that needs to read its own writes back immediately — a queue drainer, a cleanup
pass, an outbound sync loop — had no way to tell whether it was safe to run. On a
secondary tab both assumptions fail: other tabs' writes arrive up to ~500 ms
late, and its own writes are only visible to everyone once the primary applies
them.

Returns `true` on Node.js and React Native, and on the in-memory Safari fallback
where each tab holds an isolated database. Primary status changes when the owning
tab closes, so re-check it rather than caching. See
[Web › Multi-tab behaviour](https://taladb.dev/guide/web).

### Changed — BREAKING

- **`insert` and `insertMany` accept an optional `_id` in their types.** The
  runtime has honoured a caller-supplied id since this release, but the
  parameter was typed `Omit<T, '_id'>`, so the `deriveDocId` hydration pattern
  the documentation recommends did not compile. Both now take
  `InsertDoc<T>` (`Omit<T, '_id'> & { _id?: string }`), which is exported.
  Widening only — no call site breaks.
- **`@taladb/react`: `useMutation` is now `useWrite`.** The hook is unchanged
  apart from its name and the two callbacks it returns — `mutate` → `write`,
  `mutateAsync` → `writeAsync`. Types follow: `UseMutationOptions` →
  `UseWriteOptions`, `MutationResult` → `WriteResult`; `WriteOp` is unchanged.

  ```diff
  -const { mutate, mutateAsync } = useMutation<Order>({ collection: 'orders' })
  -mutate({ type: 'update', where: { _id }, set: { status: 'shipped' } })
  +const { write, writeAsync } = useWrite<Order>({ collection: 'orders' })
  +write({ type: 'update', where: { _id }, set: { status: 'shipped' } })
  ```

  No deprecated alias is kept. `useMutation` promises a network round-trip that
  this hook does not perform, and an alias would leave the misleading name in
  scope for exactly as long as it took someone to depend on it. The hook is also
  documented for the first time, under
  [React › useWrite](https://taladb.dev/guide/react).
- **`_changed_at` is no longer auto-indexed.** The index was created behind the
  caller's back to serve changeset exports. The field is still stamped; the index
  is now opt-in via `createIndex('_changed_at')`. Create it explicitly if you
  filter or sort on that field.
- **Config: the `sync` block is now `webhook`,** read by the TypeScript client
  rather than the engine. On browser and React Native, pass `openDB({ webhook })`.
- **React Native:** webhook settings move from `TalaDBModule.initialize` to
  `openDB(name, { webhook })`.
- **`taladb-core`: `WriteTxn` gains `list_tables` and `delete_table`.** Required
  methods, so an out-of-tree implementation of the trait must add them. Both are
  thin passthroughs on the two in-tree backends; `delete_table` reclaims a
  table's storage rather than emptying it, which is what the v1 → v2 migration
  needs to actually free the space.
- **Browser multi-tab:** cross-tab write propagation no longer goes through
  changesets. A tab without the OPFS lock forwards its writes to the tab that
  holds it, which applies them by id. Behaviour is unchanged for callers;
  concurrent edits now resolve by arrival order rather than timestamp, which is
  strictly safer between tabs sharing one clock.

### Fixed

**The browser binding did not compile.** `Arc` was used but never imported on
the encrypted OPFS open path. The path is `wasm32`-only, so the workspace
`cargo check` CI runs on Linux never reached it and `@taladb/web` could not be
built at all.

**Multi-tab: single-document inserts from a non-primary tab were lost.** A tab
that does not hold the OPFS lock forwards every write to the tab that does. The
forwarding code parsed the write's result as JSON — correct for `insertMany`,
which returns a JSON array, but `insert` returns a bare ULID, so the parse threw
and the handler swallowed it. The document lived in that tab's memory until its
next snapshot reload and then vanished. `insertMany`, `update*` and `delete*`
were unaffected.

**Multi-tab: live queries in a non-primary tab went permanently silent.** Live
queries skip re-running when the collection's write generation is unchanged. The
generation belongs to the in-memory `Database`, so reloading the IndexedDB
snapshot — which happens precisely *because* another tab wrote — reset it to
zero, and the poller read the value it had already seen. The first snapshot was
delivered and nothing after it. The generation is now continuous across the
instance swaps a reload performs.

**Multi-tab: closing the tab that owned the file silently stopped persistence.**
Tabs that lost the OPFS lock never asked for it again, so once the owner closed,
the survivors kept accepting writes that nothing would ever persist — and
reported success for every one. A tab that loses the lock now queues for it in
the background and takes over when it frees: it opens the OPFS file, adopts it
as authoritative, and replays its own writes that the departed owner never
acknowledged. Acknowledgement is what makes the replay safe — a write that *was*
applied is never replayed, so a forwarded `$inc` cannot double-count.

**A vector whose length disagreed with its index was silently dropped from it.**
The write path skipped the index entry and kept the document, so the row existed,
matched ordinary queries, and could never be returned by `findNearest` — a result
set with a hole in it and nothing to indicate one. Such a write is now rejected
with `VectorDimensionMismatch`. A missing field and an explicit `null` are still
legal: "not embedded yet" is an ordinary state. Backfill at `createVectorIndex`
stays lenient, since documents written before the index existed are not a caller
error.

**`findNearest(field, query, 0)` returned the entire collection.** A `top_k` of
zero fell past the top-k partition untouched. `searchText` and `hybridSearch`
already guarded this.

### Testing

A browser end-to-end suite lives in `packages/bindings/web/e2e` and runs with
`pnpm --filter @taladb/web test:e2e`. It drives real Chrome against the built
WASM over OPFS, through the worker — including two and three tabs on one
database, which is where every multi-tab bug above was found and where the Node
tests, which mock the browser, cannot reach.

### Performance

One fewer table write per delete, and — on databases created by this release —
one fewer index maintained per write, both reclaimed from bookkeeping that
existed only for replication.

**Updates no longer rewrite indexes on fields they did not touch.** Index
maintenance emitted a delete and a put of the same key for every index on the
collection, whether or not the update changed that index's field. The cost was
carried by whatever else the collection was indexed for: a `$set` of one boolean
re-tokenized a full-text field, read back each document's recorded length from
storage, and rewrote every embedding. Unchanged fields are now skipped —
`updateMany` over 2500 documents in a browser collection carrying a BM25 index
went from **2216 ms to 134 ms (16×)**, and the saving grows with the width of the
document rather than the size of the change.

### Migrating an existing database

Opening a database written by an earlier release runs a **v1 → v2 schema
migration** that drops the tombstone and quarantine tables replication left
behind. On a delete-heavy collection the tombstone table can rival the documents
it shadows, so this is space, not tidiness. It runs once, at open, and needs no
configuration.

The `_changed_at` index is deliberately **not** dropped. Earlier releases created
it behind the caller's back, but by now an application may equally have created
it on purpose, and the two are indistinguishable on disk — dropping an index
somebody's queries depend on is the worse failure. Run
`dropIndex('_changed_at')` if you do not want it.

### Security & supply chain

`reqwest` is gone from the dependency tree, and with it `rustls`, `hyper`, and
the blocking HTTP stack — the engine makes no network calls at all. Nothing
remains that accepts, validates, or merges data from a remote peer.

## [0.10.2] - 2026-08-02

### Fixed

- **A remote peer could freeze a document permanently.** Last-Write-Wins trusted
  the `changed_at` on an incoming change without bound, and read a document's
  stored `_changed_at` — a `Value::Int(i64)` — with a plain `as u64`. Both let a
  single change mint an ordering token no local write could ever beat: `u64::MAX`
  outranks every timestamp the clock can produce, and `-1` cast to `u64` *is*
  `u64::MAX`. Once such a document propagated, every replica kept re-sending it
  and no edit on any device could displace it.

  Incoming changes whose `changed_at` sits further ahead of the local clock than
  `MAX_CLOCK_SKEW_MS` (24 h — far beyond real skew, far below the values that do
  the damage) are now skipped and counted in `ImportReport::skipped` rather than
  applied. Skipping rather than erroring is deliberate: this is the tolerant
  import path, and letting one malformed row abort a whole changeset would trade
  a data-integrity bug for a denial-of-service one. Stored timestamps that can't
  be ordered now floor to the epoch, so they always *lose* instead of always
  winning — including on the export side, which previously handed peers the same
  saturated value. Ordinary clock skew is unaffected.

- **`$match` disagreed with `find()` on invalid regexes.** The aggregation
  pipeline evaluated `$match` through `Filter::matches`, which recompiles the
  pattern for every document and swallows compile failures into
  `unwrap_or(false)`. An invalid or over-size-limit pattern therefore returned an
  empty result set — indistinguishable from a legitimate no-match — while the
  identical filter through `find()` returned `InvalidFilter`. Patterns are now
  compiled once per pipeline, as the query executor already did, and a bad one is
  an error on both paths.

- **A wrong-dimension vector could rank into `findNearest` results.** The scoring
  reductions walk the query and stored vectors in lockstep and stop at the
  shorter, so a truncated entry was scored over its common prefix — and a short
  prefix can out-score a full-length vector. The write path enforces the index
  dimension, so this was only reachable through `restore_from_snapshot` (which
  writes raw table bytes) or on-disk corruption; the flat scan now skips
  mismatched entries outright.

- **`export_snapshot` could write a corrupt snapshot instead of failing.** Length
  prefixes were encoded with `u32::try_from(..).unwrap_or(u32::MAX)`, which
  claims `u32::MAX` bytes while the real, shorter bytes still follow — the reader
  then walks into the next field and desynchronises the whole remaining stream.
  Unrepresentable lengths are now an `InvalidSnapshot` error at write time, so
  the two sides agree on what is encodable.

**A faster engine, same API.** Exact vector search is ~63% quicker at 100k
vectors, scans are ~30% quicker, and two query shapes — `findOne` and `$and`
over several indexed fields — stopped doing work proportional to the whole
collection. No API changes; no migration.

All Node figures below come from `pnpm bench` run back-to-back against the
0.10.1 and 0.10.2 binaries on the same machine (2018 MacBook Pro, Intel
i5-8259U, 8 GB). The 0.10.1 pass reproduced the previously published table
within a few percent, so the deltas are like-for-like rather than a
different-day artefact.

### Performance

- **Vector scoring in a single pass.** The flat `findNearest` loop computed the
  query vector's L2 norm once *per candidate*, and made three separate passes
  over each stored vector. The norm is now hoisted out of the loop and the dot
  product and stored-vector norm are accumulated together in one pass — much
  friendlier to cache and autovectorisation. At 384 dimensions:
  **100k vectors 174 ms → 65 ms (−63%)**, 50k 85 ms → 33 ms, 10k 18.2 ms →
  7.7 ms, 1k 3.0 ms → 1.9 ms. Filtered search 199 ms → 171 ms (it gains less
  because much of its time is the pre-filter query, not scoring).
- **Vector scoring is now lane-parallel.** Even after the single-pass change the
  reductions were scalar: floating-point addition is not associative, so the
  compiler could not split a `+=` chain into vector lanes — every element waited
  on the previous partial sum. Each reduction now accumulates into 8 independent
  lanes, which authorises that reordering in the source. No `unsafe`, no
  `core::arch` intrinsics, no nightly: it vectorises on x86 AVX, aarch64 NEON,
  and the wasm128 SIMD every shipping browser has. Measured with the new
  `cargo bench` harness, per-candidate scoring at 384 dimensions:
  **dot 602 ns → 116 ns (−82%)**, **euclidean 618 ns → 124 ns (−83%)**,
  **cosine 633 ns → 346 ns (−57%)**. Cosine gains less because it does twice the
  work per element; precomputing stored-vector norms in the decoded-vector cache
  would close most of the remaining gap and is the obvious next step.

  Reassociating the sums changes results in the last bits or two of an `f32`.
  Ranking is unaffected, and tests pin the new results against a scalar
  reference at every length, including the non-multiple-of-8 remainder.
- **One storage-table resolution per batch, not per key.** Fetching *N*
  documents opened the underlying table *N* times, at roughly 2.3 µs each —
  enough to make an index-backed lookup returning many rows *slower* than a
  full scan. Reads now resolve a table once per transaction and fetch in
  batches; index maintenance groups its writes per table. **Bulk ingest
  33.6k → 39.2k docs/s (+17%)**, indexed equality 194 → 174 µs.
- **Range scans stream instead of materialising.** A range walk copied every key
  and value into a freshly allocated pair of `Vec`s and built the entire result
  set before the caller saw the first entry. Entries are now handed to the query
  engine borrowed from the page cache. **Unindexed scan 452 ms → 317 ms
  (−30%)**, unindexed `count` 477 ms → 324 ms (−32%).
- **`findOne` and bounded `find` stop when they have enough.** Both previously
  materialised every match and then discarded all but the first rows, so an
  unindexed `findOne` cost a full scan. The limit is now pushed into the storage
  walk. On a 20k-document collection: unindexed `findOne` 93 ms → **125 µs**;
  indexed-equality `findOne` 109 ms → **28 µs**; `find` with `limit: 20` and no
  sort 50 ms → **53 µs**. A limit is deliberately *not* pushed down when a
  `$sort` is present, because every candidate can still reach the page.
- **`$and` over multiple indexed equalities intersects the indexes.** The
  planner previously picked the first indexed conjunct and post-filtered the
  rest, so `status = "active" AND city = "x"` scanned every active row. It now
  intersects id sets smallest-first and decodes only documents satisfying every
  conjunct: **109 ms → 4.9 ms** on a 20k collection whose first conjunct matched
  everything. Range conjuncts are excluded on purpose — a wide range can cost
  more to intersect than to post-filter.
- **`count` can answer from the index alone.** When an index range exactly
  reproduces the filter, `count` no longer decodes documents: 109 ms → 2.87 ms
  for an indexed equality over 20k rows.
- **Corpus statistics written once per batch.** A batched write updated the
  full-text `FtsStats` record once *per document*; it is now folded in memory
  and persisted once.

### Cold start (browser)

- **Smaller payload.** `taladb_web_bg.wasm` 2,009,406 → **1,694,846 B** raw
  (−15.7%); 748,583 → 635,567 B gzip; 548,354 → 471,161 B brotli. The browser
  bundle no longer carries a YAML config parser or an HTTP client crate — the
  single JSON `POST` used by push sync now calls the platform `fetch` directly,
  with an `AbortSignal` timeout that actually cancels the request — and endpoint
  validation no longer pulls in a URL parser's Unicode tables for what is a
  scheme-and-host check.
- **Less serial work before the first query.** The worker begins fetching and
  compiling the WASM at module scope rather than waiting for the page's `init`
  message, and WASM compilation now runs concurrently with OPFS setup instead of
  before it. The OPFS capability probe — which created, opened, closed and
  deleted a scratch file on every start — is gone; the real open reveals the same
  thing. *(Payload sizes are measured; the wall-clock effect of the sequencing
  changes has not yet been measured in a browser.)*
- **Fewer JS round-trips per storage operation.** The OPFS backend resolved
  `read`/`write` by property lookup and allocated a fresh options object and
  buffer on every call — on every redb page access. These are now resolved once
  and reused.
- **Cheaper live queries.** `subscribe()` polled every 300 ms by re-running the
  query and `JSON.stringify`-ing the whole result set to detect change. It now
  compares a write-generation counter first and only re-runs when something has
  actually been committed.

### Fixed

- **`count` disagreed with `find` for `NaN` equality.** `Filter::Eq(field, NaN)`
  matches nothing (`NaN != NaN`), but `NaN` encodes to a real index key whose
  range contains the stored row — so the new index-only `count` path would have
  returned 1 where `find` returns 0. The fast path now declines `NaN`. Signed
  zero is unaffected (the planner already emits both encodings, and `0.0 ==
  -0.0`). Both cases are covered by tests.
- **Browsers where `createSyncAccessHandle` exists but throws** (Firefox without
  storage access, cross-origin iframes) failed to open instead of degrading.
  The removed OPFS probe had caught this implicitly; both open paths now fall
  back to the IndexedDB-backed engine explicitly. Encrypted databases still
  refuse to degrade, because the IndexedDB snapshot is plaintext.
- **React Native reported the *previous* call's error.** `taladb_last_error()`
  was only ever written on failure and never cleared, and roughly a third of the
  failure paths returned `NULL`/`-1` without setting a message at all. A caller
  checking the error after a success — or after one of those silent failures —
  read a stale message describing an unrelated operation, which is the opposite
  of the documented contract. Every entry point now clears the slot on entry and
  every failure path sets a message.
- **React Native `insertMany` silently dropped malformed documents.** Elements
  that failed to parse were filtered out and the call returned a *shorter* id
  array with no indication which inputs had vanished, so a caller zipping ids
  back onto its input mis-associated every document after the first bad one. The
  call is now all-or-nothing and names the offending index.
- **Range queries were wrong for integers above 2^53.** `$gt`/`$gte`/`$lt`/
  `$lte` compared a mixed `Int`/`Float` pair by casting the integer to `f64`,
  which rounds: `Int(9007199254740993)` compared *equal* to
  `Float(9007199254740992.0)` instead of greater. The comparison is now exact in
  both directions, including floats outside the `i64` range, where the reverse
  cast used to saturate.
- **`skip` and `limit` past 2^32 wrapped on 32-bit targets.** Both are `u64` in
  the API but index into a `Vec`, and `usize` is 32 bits on `wasm32` — the
  primary target. A `skip` of 2^32 + 5 truncated to 5 and returned the *first*
  page. They now saturate, so an out-of-range page is empty.

### Changed — breaking

- **`TalaDbError` now carries the originating storage error as a
  [`source`](https://doc.rust-lang.org/std/error/trait.Error.html#method.source).**
  Every `redb` failure used to be flattened into `Storage(String)`, so an
  application could not tell a full disk from a corrupt page from a contended
  table — exactly the distinction an on-device database needs in order to decide
  retry-versus-fail. Storage failures are now separate variants carrying their
  `redb` error, and `redb` is re-exported for downcasting. `Display` output is
  unchanged, so error text crossing into JavaScript is byte-identical.
- **`TalaDbError` is `#[non_exhaustive]`.** Adding a variant had been a silent
  breaking change for any downstream exhaustive `match` — `QueryTimeout` and
  `ChangesetTooLarge` both were. Rust callers matching on it need a `_` arm; use
  the new `TalaDbError::code()` for a stable machine-readable discriminant
  instead of matching variant by variant. The JavaScript-visible `error.code`
  values are unchanged.
- **`VectorMetric` is now `Copy`.** Only affects code that explicitly `.clone()`d
  it, which still compiles.
- **`vector::score_from_bytes` was removed.** It had no callers: flat search
  decodes the vector table once into a cache and scores against `&[f32]`, so
  nothing reached the raw bytes. Its doc comment claimed to be the fast path,
  which is worse than not existing.

### Added

- **`Value` conversions and accessors.** `From` impls for the Rust types that
  map onto it (`bool`, the integer and float types, `String`/`&str`, `Vec<u8>`,
  `Option<T>`), `FromIterator` for arrays, a `Display` impl for logs and error
  messages, and the four missing accessors — `as_bytes`, `as_array`,
  `as_object`, and `get` — plus `is_null`. Building a document no longer means
  naming the variant for every field.
- **`cargo bench -p taladb-core`.** Criterion benchmarks for vector scoring,
  filtered scans, insert throughput, and snapshot export. Performance is a
  headline claim for this project, and until now the only numbers came through
  the Node binding, which measures the N-API boundary alongside the engine.
- **Crate metadata and documentation.** `taladb-core` is a published crate that
  had no `repository`, `keywords`, `categories`, or `readme`, and a docs.rs
  landing page with no prose at all. It now has all of them, plus a crate-level
  guide with runnable examples.

### Internal

- `StorageBackend` gained batch (`get_many`, `apply_batch`) and streaming
  (`scan`) operations. Both have default implementations in terms of the
  existing methods, so third-party backends keep working unchanged — they simply
  do not get the speedup until they override them.
- The `release-wasm` Cargo profile was removed. `wasm-pack --release` uses
  `[profile.release]` and cannot be pointed at a custom profile, so its
  `opt-level = "z"` had never taken effect; WASM size is now controlled by
  `wasm-opt` flags that wasm-pack does honour.
- `serde_yaml` is now behind a `config-yaml` feature (on by default; off for the
  browser build). JSON config is unaffected.
- Release builds use `codegen-units = 1`, which improves the generated code at
  the cost of slower release compilation.
- **`create_vector_index` no longer builds a decoded copy of every vector on the
  default build.** The copy exists only to seed an HNSW graph, but it was
  populated unconditionally — including when `vector-hnsw` is off, which is the
  default and therefore the browser and React Native builds. At 100k documents ×
  384 dimensions that was ~150 MB of WASM heap held for the whole backfill and
  never read.
- **`export_snapshot` no longer holds the whole database twice.** It collected
  every table into an intermediate before writing, even though the table count —
  the only reason given for collecting — is known from `list_tables` alone. Each
  table now streams straight into the output buffer, which is sized up front
  rather than grown entry by entry. The byte format is unchanged.
- **`TalaDbError` stays 48 bytes.** Attaching the `redb` sources inline would
  have taken it to 176 (`redb::Error` alone is 160), widening every
  `Result<T, TalaDbError>` in the engine — a cost paid on the success path. The
  three oversized sources are boxed; a test pins the size.
- The four `Database` constructors each repeated the same six-field struct
  literal, so adding a cache field was a four-site edit that compiled fine if you
  missed one. They now funnel through a private `from_backend`.
- **Workspace lints and an MSRV.** `[workspace.lints]` is declared once and
  inherited by every crate, `rust-version = "1.88"` is now stated rather than
  implied, and the tree is clean under `cargo clippy --workspace --all-targets`.
  `unsafe_op_in_unsafe_fn` is denied, which is what forces the React Native FFI
  to mark each raw-pointer dereference at its call site: the four helpers that
  hand out references derived from caller-supplied pointers are now `unsafe fn`
  with documented safety contracts, rather than safe functions returning an
  unbounded lifetime.

## [0.10.1] - 2026-08-01

**Faster repeated vector search.** The flat search path — the default, and the
only vector path on the browser and React Native — no longer re-reads and
re-decodes the entire vector table from storage on every query.

### Performance

- **Decoded-vector cache for flat `findNearest`.** Vectors are decoded once and
  held in memory, so repeated similarity queries (and the vector half of
  `hybridSearch`) are memory-bound instead of storage-bound. The cache is shared
  across every handle from the same database and tagged with the collection's
  write generation: any committed insert, update, or delete invalidates it
  automatically, so the flat path stays exact and current — it is never served
  stale. No API change. Measured on Node at 100k × 384-dim vectors: **filtered
  search 346 ms → 199 ms (~42% faster)**, unfiltered 198 ms → 176 ms; small
  collections (1k–10k) are unchanged within run-to-run noise.

## [0.10.0] - 2026-07-25

**Search grows up.** Full-text search is now **relevance-ranked** with BM25 —
the model behind Lucene and Elasticsearch — via `searchText`, and TalaDB gains
`hybridSearch`, which fuses keyword and vector rankings with reciprocal rank
fusion (RRF) so an exact SKU and a semantic paraphrase both surface from one
query. This is the on-device RAG retrieval recipe, running across the browser,
Node.js, and React Native.

Schema evolution across a **version-drifted fleet**. The rule TalaDB now enforces:
*a replica must preserve fields it does not model.* Under whole-document LWW, an old
client that silently narrows a newer peer's document does not merely fail to see a
field — it **deletes** that field, for everyone.

### Fixed

- **Full-text search was entirely missing from `@taladb/node`.** The Node binding
  never exposed `createFtsIndex` / `dropFtsIndex` / `listIndexes`, so calling them
  through the unified `taladb` client threw on Node. All are now bound.
- **Browser in-memory fallback returned JS `Map`s instead of plain objects.** On the
  no-SharedWorker path, `find` / `findOne` / `findNearest` serialized results as
  `Map`, so `result.document` was `undefined` (you needed `.get('document')`).
  They now return ordinary objects, matching the primary worker path.
- **Read-time write-back no longer deletes a newer peer's fields.** With
  `persistMigrations: true`, the write-back computed `$unset` for every field absent
  from `migrateDocument`'s output. But a migration written today cannot mention a
  field a newer peer will add tomorrow, so an innocent `(doc) => ({ _id, name })`
  emitted `$unset: { fullName: true }` — deleting a v2 field a v1 client had simply
  never heard of, and replicating that deletion to the fleet. The write-back is now
  **additive-only** (`$set`); declare intentional removals with `retiredFields`.
- **`validateOnRead` no longer strips unknown fields from future-version documents.** `z.object({...})` and
  `v.object({...})` drop undeclared keys, and `parse()`'s output was returned
  verbatim — so a v1 client read a v2 document back minus v2's fields, and the next
  read-modify-write wrote the truncation home. Reads now parse for the check and its
  coercions, then restore keys the parse dropped when the stored `_v` is newer
  than this client's `syncSchema.version`. At-version/local documents retain the
  validator's normal strictness.

### Added

- **`searchText(field, query, topK, filter?, options?)`** — BM25 relevance-ranked
  full-text search over an FTS index, most relevant first. Unlike the `$contains`
  filter (every token required), it uses OR semantics — matching more of the query
  scores higher. Accepts a metadata pre-filter and `{ k1, b }` tuning. Available on
  every runtime.
- **`hybridSearch(text, vector, topK, filter?, options?)`** — fuses BM25 keyword
  ranking with vector similarity using reciprocal rank fusion. Returns per-retriever
  ranks (`textRank` / `vectorRank`); the optional filter applies to both retrievers
  before ranking. Tunable via `{ rrfK, textWeight, vectorWeight, candidates }`.
- **FTS indexes now store BM25 statistics** — term frequencies, per-document token
  counts, and corpus stats — so `createFtsIndex` powers ranking as well as the
  boolean `$contains` filter.
- **`@taladb/node` now exposes full-text search** — `createFtsIndex`, `dropFtsIndex`,
  `listIndexes`, `searchText`, and `hybridSearch` are available on the Node binding.
- **`CollectionOptions.downgradeDocument(doc, fromVersion)`** — the **downcast**, the
  missing mirror of `migrateDocument`. Projects a document whose `_v` is *above*
  `syncSchema.version` into the shape this build reads, so a client that was offline
  for three weeks on v1 can still read what the v2 fleet wrote. Always a **view**:
  never persisted (not even under `persistMigrations`), and the returned document
  keeps its true, higher `_v` — this replica still has to hand that document on to
  other peers intact.
- **`CollectionOptions.retiredFields`** — explicitly lists fields an upcast may
  remove during persist-on-read. This is the preferred, narrow retirement control.
- **`CollectionOptions.allowFieldRemoval`** (default `false`, deprecated) — treat
  every field absent
  from `migrateDocument`'s output as an intentional removal rather than as one the
  migration never heard of. For collections that never sync, or the **retire** step
  of an add → backfill → dual-read → retire rollout.

### Changed

- **FTS index storage upgraded to v1** to carry BM25 statistics. Indexes created
  before 0.10 remain readable through `$contains`, but must be dropped and recreated
  to use `searchText` / `hybridSearch` — those calls return a clear `InvalidOperation`
  error against a pre-0.10 index rather than ranking incorrectly.
- `persistMigrations` write-backs are additive-only by default (see Fixed). A
  migration that relied on `$unset` firing now needs `retiredFields`, or the
  deprecated broad `allowFieldRemoval: true` escape hatch.

## [0.9.4] - 2026-07-13

`useQuery` is rebuilt around **coverage**: once a collection is fully replicated for
its scope, filtering, sorting and paging run entirely on-device and touch the network
**zero times**. Pagination stops being a network concern.

The previous design — cache each API page and reuse it — is deliberately *not* what
shipped. A replica assembled from whichever pages a user happened to visit is an
arbitrary partial subset: it cannot answer a query nobody has asked yet ("products
under ₱500" may live on page 43), so every new filter still goes to the network and
the local database buys almost nothing.

### Added

- **`Collection.replaceManyWithIds(docs, origin?)`** — batched upsert **by
  caller-supplied `_id`**, one commit per batch. The first write path that honours an
  id you chose; every other one discards it and mints a ULID.
- **`Collection.deleteManyWithIds(ids, origin?)`** — batched delete by id.
- **`Collection.subscribeAggregate(pipeline, cb)`** — a **live** aggregation. Needed
  because `aggregate()` alone returns a dead snapshot, so a paged read built on it
  would sit frozen while rows landed underneath it.
- **`deriveDocId(collection, key)`** — a stable `_id` derived from an origin's primary
  key, so repeated fetches of the same row converge on one document. Implemented
  identically in Rust and TypeScript, with shared test vectors.
- **`CursorSyncAdapter.pullWithCursor(cursor)`** — an **opaque, origin-issued** resume
  cursor. `runSync` feature-detects it. Author wall-clock timestamps are not safe
  cursors (a write can commit after an export yet carry an earlier timestamp), which
  is why the old `pull(sinceMs)` path replays from zero on every pass; letting the
  origin issue the token sidesteps the clock entirely.
- **`ReplicationCoordinator`, `CoverageStore`, `createRestSource`** — snapshot-
  consistent bootstrap, resumable walks, delta refresh, and a cold-start bridge.
- **`@taladb/react`: `useCoverage`, `useAggregate`**, and
  `<ReplicationProvider replicate={…}>`.
- **`@taladb/next/server`: `createReplicationHandlers`** — bootstrap + delta endpoints
  for an ordinary REST origin (framework-neutral `Request` → `Response`).
- New system collection `__taladb_replica` for coverage state.

### Fixed

- **Replicated rows were pushed back at the origin they came from.** `exportChanges`
  scans the collection directly, and when the cursor is `0` — which it always is
  today, since cursors were stubbed — it exported **everything** via
  `find(Filter::All)`. A hydrated 100k-row catalog would have been pushed straight
  back to the server on the next `db.sync()`, as though the user had typed it all in.
  Suppressing the sync hook was not sufficient; there were three escape routes
  (the push hook, the export scan, and tombstones). Rows written with
  `origin: 'remote'` are now marked in the engine, skipped by `export_changes`, and
  deleted without a tombstone — so "remote rows never replicate outward" is an
  invariant of the storage layer rather than something a caller must remember.
- **`docs/api/collection.md` documented `findWithOptions`, which exists in no
  binding.** It is Rust-only and unreachable from JavaScript. The section now
  documents `aggregate` / `subscribeAggregate`, which is how you actually sort, page
  and project.
- **`useQueries` resolved collections without the registry**, silently skipping schema
  validation and migrations for every query it ran.
- Coverage rows are now isolated by origin and authorization scope, including in
  their derived ids and every local query, so switching tenants cannot expose or
  overwrite the previous tenant's replica.
- Cold-start page bridges no longer apply the remote page offset a second time
  locally; page 2 now renders the page that was fetched instead of an empty set.
- Authoritative writes compare the server's monotonic row revision inside the
  write transaction, preventing a delayed stale response from replacing newer data.
- Bootstrap checkpoints persist the first-page delta cursor, so crash/resume cannot
  finish with an empty cursor and miss changes made during the walk.
- REST sources preserve absolute origins, distinguish page-number from offset
  pagination, honor explicit `nextPage: null`, and only advertise delta support
  when it is configured.

### Changed — breaking

- **`useQuery` adds** `{ sort, page, limit, skip, enabled }` and returns coverage,
  bridge progress, total count, and bridge errors. The existing sync-contract
  `{ endpoint, source: 'local-first' | … }`, `syncing`, and `syncError` surface is
  retained for compatibility; coverage-first replication can instead be declared
  once on `<ReplicationProvider replicate>`.
- **Documents written by replication get a *derived* `_id`**, which is a hash — so its
  ULID timestamp prefix is not chronological and such rows do **not** come back in
  insertion order from an unsorted `find()`. Always pass an explicit `$sort` when
  reading a replicated collection. Rows written via `insert`/`insertMany` are
  unaffected and keep their monotonic ULIDs.

Everything else is additive: `find`, `insert`, `aggregate`, indexes, encryption,
migrations, `db.sync()` with existing adapters, `useFind`, `useFindOne`,
`useMutation` and the existing `prefetch` config are unchanged.

## [0.9.3] - 2026-07-12

Paging a large collection stops being a full-collection operation, and three
API-surface bugs that silently did the wrong thing are fixed.

### Added

- **Sorted pages are served from the index — a paged `$sort` no longer reads the whole collection.** `[{ $sort }, { $skip }, { $limit }]` over an unfiltered collection used to decode **every** document, sort all of them, then throw away everything but the page. `aggregate` now walks the sort field's secondary index in key order (touching no documents at all), takes only the page — plus the run of ties it lands in — and decodes just those. Paging a 10,000-document catalog for 24 rows went from **88.9 ms to 6.4 ms (13.8×)**, and is now faster than a bare full scan because it never performs one. Applies when the leading `$sort`'s **first** key is indexed; multi-key sorts qualify (later keys order within the tie-run). Falls back to the old path when the field is unindexed, or when any document lacks it — such a document has no index entry, and serving from the index would silently drop it.
- **Bounded (top-K) sort.** When a `$sort` is followed by a `$skip`/`$limit` that makes the reachable set finite, only those documents are ordered (`O(n + k log k)`) instead of the whole matched set (`O(n log n)`). This is the fallback path's counterpart to the index-served page above, and it applies to filtered queries too.

### Fixed

- **`$project` exclusion silently returned near-empty documents.** The engine implemented *inclusion* only, but the type (`Record<string, 0 | 1>`) advertised Mongo-style exclusion. `{ description: 0 }` parsed to an **empty inclusion list**, so every document came back stripped of every field except `_id` — no error, no warning, just missing data. Exclusion is now implemented (`{ a: 0 }` keeps everything but `a`), inclusion is unchanged, and **mixing the two now throws** rather than resolving to something arbitrary (`_id: 0` alongside an inclusion remains legal, as in MongoDB).
- **Sort order is now total, so paging cannot repeat or drop a document.** The comparator returned `Equal` for documents tied on every sort key, leaving their relative order to the sort algorithm. Under a bounded sort (`find` with sort+limit, and now `$sort`+`$limit`) ties could be resolved differently per call, so a growing `$skip` could show the same document on two pages — or on none. Ties now break on the unique, monotonic `_id`, which makes a partial sort's prefix provably identical to a full sort's.
- **Sorting is ~7× faster.** The comparator re-resolved every sort field by scanning the document's field list and comparing key strings — on each of `O(n log n)` comparisons — and every swap moved a whole `Document`. Sort keys are now extracted once and the sort shuffles those instead (decorate-sort-undecorate).
- **`$in` was unusable on a union-typed field.** `FieldOps<T>` was a *distributive* conditional (`T extends null | undefined ? … : …`), so for a field typed `'Cabin' | 'Villa' | …` it spread over the union and inferred `$in?: 'Cabin'[] | 'Villa'[] | …` rather than `$in?: ('Cabin' | 'Villa' | …)[]` — every such filter needed an `as` cast. `FieldOps` is no longer a conditional at all: `$exists` is always available (it was the only thing the conditional bought) and the value operators use `NonNullable<T>`, so optional fields keep both.
- **The React hooks bypassed collection configuration.** `useCollection(name)` called `db.collection(name)` with **no options**, and `useQuery`/`useMutation` build on it — so a write through `useMutation` skipped the `schema` validation and the `_v` stamp that `db.collection(name, { … })` applies. An app following both the hooks guide and the [Schema & Sync Standards](https://taladb.dev/guide/schema-and-sync-standards) silently lost strict local validation. `<TalaDBProvider collections={{ bookings: { schema, syncSchema } }}>` now registers per-collection options and `useCollection` (hence every hook below it) resolves a configured handle; `useCollection(name, options)` overrides per call. The registry is read through a stable resolver, so an inline `collections={{…}}` object cannot invalidate live-query subscriptions.

## [0.9.2] - 2026-07-12

Schema evolution for local-first data: tolerant **validate-on-import** and
application **schema migrations**, both wired on **browser (OPFS worker) and
Node.js**. Ships rebuilt `.node` and WASM binaries.

### Added

- **Validate-on-import — "validate, never cast" inside `db.sync()`** — attach a tolerant `syncSchema` to a collection and every document pulled by `db.sync()` is checked in the engine before the Last-Write-Wins merge, instead of being cast in blindly. The core `import_changes` path now consults an `ImportValidator` whose decision is one of *accept / coerce / skip / **quarantine*** — it **never hard-rejects** (a local-first, LWW replica that dropped or threw on a foreign-shaped document would lose a legitimately newer write or diverge peers). Rejected documents are set aside in a per-collection quarantine table, recoverable via `db.quarantined(collection)` (`{ document, reason, changedAt }`), never dropped and never aborting the batch. `SyncSchema` is structural — `{ version, required, types, defaults }` — the tolerant safety net on the boundary you don't control, distinct from the strict Zod/Valibot `schema` that still runs on the local `insert` path. Wired on **browser, Node, and React Native** (the RN JSI glue is in place but not yet exercised on-device); a binding whose native module predates 0.9.2 falls back to unvalidated import. See [Schema & Sync Standards](https://taladb.dev/guide/schema-and-sync-standards).
- **Per-document schema version (`_v`)** — a document tagged `_v` **below** its collection's `syncSchema.version` is upgraded in place on import (field **renames** applied, missing `defaults` filled, `_v` stamped) rather than rejected — additive-only migration that travels *with the data*, so peers on different app versions converge. A document `_v` **ahead** of the local version is accepted untouched (the peer is ahead, not wrong). The counter is per-document and independent of the engine's own storage-schema version.
- **`syncSchema.renames` — structural field renames on import** — `{ syncSchema: { version, renames: { oldName: 'newName' } } }` moves a value from an old field name to a new one when upgrading a below-version document, in the engine, before defaults. The deterministic, structural half of schema evolution (browser + Node); pairs with `migrateDocument` for renames that need computation.
- **`migrateDocument` — lazy read-time normalization (all runtimes)** — a per-collection `migrateDocument(doc, fromVersion)` on `db.collection(name, { syncSchema: { version }, migrateDocument })`. Every document returned by `find` / `findOne` whose `_v` is below `syncSchema.version` is passed through the callback and stamped to the current version before the app sees it — so code always reads the current shape (computed/derived fields, splits/merges), even for documents that predate the schema. It's a pure client-side transform, so it works on **every runtime** with no binding support, and complements the structural, import-time `syncSchema` (the RxDB lazy-migration model). By default the transform applies to the returned value only; set `persistMigrations: true` to write the upgraded shape back to storage on first read (best-effort `$set`/`$unset` diff), after which filters and indexes on the new shape match it. For an eager whole-collection rewrite instead, use `openDB({ migrations })`.
- **React Native — validate-on-import and migration accessors wired (pending on-device verification)** — `importChangesValidated` / `quarantined` and `userVersion` / `setUserVersion` are now exposed through the full RN stack: Rust FFI (`taladb_import_changes_validated`, `taladb_quarantined`, `taladb_user_version`, `taladb_set_user_version`), the C header, and the JSI HostObject (C++). The TS client feature-detects them, so `db.sync({ syncSchema })` and `openDB({ migrations })` light up on RN once the native module ships them, and degrade gracefully otherwise. The Rust and TS layers compile/typecheck; the JSI native glue has not yet been exercised on a device/simulator (same status as RN bidirectional sync).
- **Application schema migrations — `openDB({ migrations })`** *(browser + Node)* — pass an ordered `migrations: [{ version, description?, up }]` array and TalaDB runs the pending ones (version greater than the stored counter) at open, in ascending order. The stored version — a new `meta::user_version` counter, kept separate from the engine's storage-schema version — advances **after each `up` resolves** (checkpoint per version): a migration that throws stops the run and the error propagates, and the next open resumes from the one that failed. This is checkpoint-per-version, **not** whole-batch-atomic (a single `up` runs through the normal collection API, so write bodies must be idempotent — `createIndex` already is). Node runs it via the napi binding; the browser runs it in the OPFS worker. React Native throws a clear "not available on this binary" error until its JSI HostObject exposes the version accessors (the Rust FFI is in place). See [Migrations](https://taladb.dev/api/migrations).
- **`SyncResult.skipped` / `SyncResult.quarantined`** — a validated sync pass now reports how many pulled documents an import validator skipped (a collection this client doesn't model) or quarantined (failed structural validation). Both optional and additive — unset when no `syncSchema` applied.
- **Configurable durability + `db.flush()`** — `openDB(name, { durability: { flush_every_write, flush_ms } })` (also under `config.durability`). `flush_every_write: false` switches the storage engine from redb *Immediate* to *Eventual* durability — commits are batched instead of fsync'd one-by-one, a real write-throughput lever on Node (file) and the browser OPFS engine — and `await db.flush()` forces a durable sync at "save now" moments (before checkout, on `visibilitychange`). `flush_ms` tunes the browser IndexedDB-fallback snapshot debounce (default 500 ms). Wired on Node and browser; React Native applies `flush_every_write` from its init config and exposes `flush()` through the JSI stack (pending on-device verification). Default is unchanged (`flush_every_write: true` — fsync every commit).
- **Native binding accessors** — `@taladb/node` and `@taladb/web` expose `userVersion()` / `setUserVersion(v)` (backing the migration runner), `importChangesValidated(changeset, schemas)` / `quarantined(collection)` (backing validated sync), and `flush()` / `setDurability(eventual)` (backing configurable durability). The React Native Rust FFI gains the matching `taladb_*` exports (JSI glue pending).

### Fixed

- **`_v` is now stamped on local `insert`** — a document written locally carried no `_v`, so it read back as version 0 and was fed through `migrateDocument` as if it were legacy data: a brand-new, current-shape document could be *corrupted* by a migration written for an older shape (a v1→v2 migration deriving `fullName` from `first`/`last` produced `"undefined undefined"` on a v2 document). Declaring `syncSchema.version` now stamps that `_v` on `insert` / `insertMany`, so locally-created documents are recognisable as already-current. An explicitly supplied `_v` is preserved.
- **Live queries (`subscribe`) now apply `migrateDocument` and `validateOnRead`** — only `find` / `findOne` were wrapped, so every `@taladb/react` hook (`useFind`, `useFindOne`, `useQueries`, all of which read through `subscribe`) delivered the **un-migrated** shape while a direct `find()` returned the migrated one. `subscribe` is now wrapped on the same terms. `aggregate`, `findNearest`, and FTS `search` still match and project against the *stored* shape — they run inside the engine — so use `persistMigrations` or `openDB({ migrations })` if you query un-migrated documents that way.
- **React Native no longer silently skips import validation on a bad schema** — the RN binding returned an empty schema map when the descriptor failed to parse, and silently dropped fields whose type name it didn't recognise (a typo like `'integer'` left the field unconstrained). Node and the browser both hard-error on the same input, so the same schema accepted a document on RN that the other runtimes quarantined — precisely the replica divergence the `ImportValidator` determinism contract forbids. All three bindings now reject a malformed descriptor.
- **A `syncSchema` with `renames`/`defaults` but no `version` is now rejected** — the import migration step only runs for documents whose `_v` is below `version`, so with no version the renames never fired while `required`/`types` still applied: a rename schema quarantined every document it was written to upgrade. Both the client (`db.collection()`) and the engine (`SchemaValidator::try_new`) now refuse the combination with an explanatory error.
- **Quarantine-only import batches are no longer lost in the browser IndexedDB fallback** — the worker scheduled its IDB snapshot only when a pull *applied* at least one document, so a batch in which every document was quarantined wrote the quarantine table but never persisted it; the rows vanished on reload, contradicting the "never dropped on the floor" guarantee. The snapshot is now scheduled when a batch applies **or** quarantines.
- **Rejected documents are written in one transaction** — quarantining committed once per document, which under the default `flush_every_write` durability meant an fsync per reject, and left an arbitrary prefix of the batch behind if the process died midway. The whole batch's rejects now land in a single commit.
- **`persistMigrations` no longer fans out concurrent writes from inside `find`** — the write-back issued one `updateOne` per migrated document via `Promise.all`, turning a single `find()` over a large un-migrated collection into an N-way concurrent write storm (each its own fsync'd transaction, each firing live-query and sync-push notifications). Write-backs are now sequential; `openDB({ migrations })` remains the right tool for rewriting a whole collection.
- **`persistMigrations` no longer rewrites documents on nested key-order alone** — the `$set`/`$unset` diff compared values with `JSON.stringify`, which is key-order sensitive, so a migration that rebuilt a nested object with the same values in a different order rewrote the document on every read. The comparison is now structural.
- **Docs — the Migrations API page documented a feature that did not exist** — `openDB({ migrations })` was fully documented (with a false "single atomic transaction" guarantee) but never implemented; passing `migrations` was silently ignored. It is now implemented (see above) and the page rewritten to the real per-version-checkpoint semantics, with an idempotency note.
- **Docs — the browser durability model was described incorrectly** — the roadmap and benchmarks page claimed the browser engine was "memory-resident with a 500 ms debounced OPFS snapshot," implying a hard crash could lose recent writes. In fact the OPFS engine is redb running directly on the OPFS file, `fsync`-flushing every commit (durable per commit, same as Node); the 500 ms debounce is only the auxiliary IndexedDB snapshot on the fallback path. Corrected, and the trade-off is now a real knob (`flush_every_write`).

## [0.9.1] - 2026-07-12

### Added

- **`@taladb/react` — scoped replication hooks (`useQuery`, `useQueries`, `useMutation`)** — a react-query-shaped surface that binds a component to a *slice* of a remote origin, on demand, instead of the global-and-imperative `db.sync()`. The local store is a durable **replica, not a cache**: a pull writes into the real local collection and the existing live query re-renders off it (one-way data flow — no `queryKey`, no `invalidateQueries`, because writing to the collection *is* the invalidation). Built on the existing sync-contract transport (`db.sync()` / `HttpSyncAdapter`), so it inherits tombstones, cursors, and Last-Write-Wins. `useQuery` supports `source` modes (`local-first` / `remote-first` / `local-only`) and a `pollMs` refresh interval; `useQueries` runs several slices in parallel; `useMutation` writes local-first then replicates out through a bounded-retry drain (write-behind — the local write is never rolled back). Writes are origin-authoritative by default. Authorization is a provider-level async resolver (`getAuth`) resolved at **send time**, so an offline write flushed later carries a current token. Strictly typed end to end — remote data is validated against the collection schema, never cast. Configured via a new `<ReplicationProvider>` composed inside `<TalaDBProvider>`. Verified by 28 unit tests plus a real end-to-end run (two Node databases + a live sync server proving the pull → local → live-query loop). Sync is Node-wired today; the browser and React Native bindings ride the same code as they land. See [Scoped Replication](https://taladb.dev/guide/scoped-replication).
- **`@taladb/react` — `prefetch` (background first-run warming)** — a `<ReplicationProvider prefetch={['products', …]}>` option that warms slices into the local replica in the background, so a later `useQuery` reads local instead of waiting on the network. Off the critical path: deferred to browser idle (`requestIdleCallback`), run in the sync Worker on web, bounded by `prefetchConcurrency` (default 2), and coalesced with any concurrent `useQuery` for the same collection via in-flight dedup. First-run only by default (`prefetchMode: 'once'`, gated on the sync cursor so returning users don't re-warm); best-effort and silent on failure. See [Scoped Replication → Warming the replica on first run](https://taladb.dev/guide/scoped-replication).

## [0.9.0] - 2026-07-11

Bidirectional sync lands in the browser — a local-first web app can now
`db.sync()` against any backend — and five bugs found while wiring it are
fixed, including two that broke 0.8.4's Node sync and `openDB` entirely.
Ships rebuilt `.node` and WASM binaries.

### Performance

- **Flat vector search is ~2× faster** — the brute-force `findNearest` scan was rewritten: it scores directly from stored bytes (removing one `Vec<f32>` heap allocation per vector per query — ~100k/query at scale), hoists the query's cosine norm out of the per-candidate loop, selects the top-k with `select_nth_unstable` instead of a full sort, and resolves a hybrid pre-filter to an id set *before* scanning so filtered-out vectors are never scored. Measured on the benchmark laptop: 100k × 384-dim from 369 ms → ~197 ms, 10k from 40 ms → ~18 ms, 100k hybrid from 448 ms → ~326 ms. No API or result change.
- **Two-sided range queries use a bounded index scan** — `find({ field: { $gte: a, $lt: b } })` on an indexed field previously scanned from the lower bound to the end of the index and post-filtered the upper bound; the planner now emits a single bounded range scan. A ~100-doc `publishedAt` window at 100k docs dropped from ~463 ms to **0.76 ms** (~600×). The bounded plan covers both Int- and Float-typed index entries, with a cross-type parity test asserting it matches the unindexed result exactly.
- Next perf levers (in-memory decoded-vector cache; SIMD — a `+simd128` WASM build was measured to ~halve browser vector search) are on the [roadmap](https://taladb.dev/roadmap).

### Added

- **Encryption at rest on all three runtimes** — AES-GCM-256 encryption, previously Node.js/React-Native only, now works in the **browser** too. Pass `openDB(name, { passphrase })` and the OPFS-backed storage is wrapped in the same `EncryptedBackend` used natively (PBKDF2-HMAC-SHA256, 600k iterations; 16-byte salt in an OPFS sidecar managed by the worker). Verified end-to-end in real headless Chrome: the on-disk OPFS bytes contain no plaintext, the correct passphrase round-trips, and a wrong passphrase is rejected. Browser encryption **fails closed** — if OPFS is unavailable it refuses to open rather than silently using a plaintext fallback — and is single-tab (the multi-tab IndexedDB-snapshot fallback stores decrypted plaintext, so it's refused for encrypted databases). Document IDs and index keys remain unencrypted by design (they must stay comparable).
- **Compound (multi-field) indexes — `collection.createCompoundIndex(fields)` / `dropCompoundIndex(fields)`** — the core engine's compound-index support (storage, planner, write-time maintenance, migration rebuild) is now exposed through the bindings. The query planner uses a compound index for an `$and` where every indexed field is constrained by equality — one index scan instead of a full-collection scan. Available on **Node.js and the browser** (OPFS worker + in-memory), verified by the core test suite and a native e2e; **React Native** is wired through the FFI + JSI HostObject but pending on-device verification (same status as RN sync). Ascending, equality-on-all-fields today; partial-prefix / trailing-range / per-field-descending are on the [roadmap](https://taladb.dev/roadmap).
- **`db.sync()` in the browser** — both browser adapters (OPFS worker and the in-memory fallback) now wire the full bidirectional sync loop. All engine work (change export, LWW merge) runs inside the Dedicated Worker, off the main thread, so a sync pass never blocks rendering regardless of changeset size. Verified end-to-end against a real Chrome instance: browser ↔ HTTP sync server ↔ Node.js peer, with changeset format parity in both directions.
- **`db.sync()` on React Native — implemented, pending on-device verification** — the changeset primitives (`exportChanges` / `importChanges` / `listCollectionNames`) are now exposed through the full RN stack: Rust FFI (`taladb_export_changes` / `taladb_import_changes` / `taladb_list_collection_names`), the C header, and the JSI HostObject (C++). The TS adapter feature-detects them and enables the same `db.sync()` used on Node/web, falling back to a clear error on native modules that predate them. The Rust FFI and TS layers are compiled/typechecked here; the JSI native glue has **not** yet been built or exercised on a device or simulator — that verification (iOS + Android) is required before RN sync is considered shipped.
- **`@taladb/web` — `TalaDBWasm.listCollectionNames()`** — user collection names (reserved names excluded), backing the sync orchestration's "sync all" default on the in-memory browser path.
- **`@taladb/next` — first-party Next.js integration** — `@taladb/next/server` exposes `createSyncHandlers({ store, authorize })`: a complete sync backend as a pair of fetch-style route handlers implementing the `HttpSyncAdapter` contract, with per-caller scope partitioning via `authorize` (401 on rejection) and changeset validation. Two built-in stores: `memorySyncStore()` (dev/tests) and `taladbSyncStore(db)` — a server-side TalaDB as the change hub, same LWW layout as `@taladb/sync-mongodb`. `@taladb/next/client` exposes `<SyncProvider endpoint interval>`: the start/interval/reconnect/tab-focus sync cadence as a drop-in component composing with `<TalaDBProvider>`. Handlers are framework-agnostic (standard `Request`/`Response`), verified end-to-end from a real Chrome client through the built package.
- **`@taladb/react` — drop-in Next.js support** — the build output now ships the `'use client'` directive (SWR/react-query convention), and `<TalaDBProvider name="myapp.db" fallback={…}>` is a new name-based form where the provider owns the `openDB` lifecycle: opens lazily on the client (never during SSR), renders `fallback` until ready so hooks never observe a missing instance, closes on unmount (StrictMode-safe). The existing `db`-prop form is unchanged.
- **`examples/nextjs-sync`** — a complete local-first Next.js app: on-device notes with live queries, `<TalaDBProvider name>` + `<SyncProvider>` on the client, and a one-call `createSyncHandlers` route as the backend. CI now runs a real `next build` over it, so bundler-level regressions across the whole client stack fail the build. Release workflow publishes `@taladb/next` alongside the other packages.
- **End-to-end sync test suite (`tests/sync.e2e.test.ts`)** — exercises `openDB` → native engine → cursor persistence → `HttpSyncAdapter` against a real HTTP server, with LWW-convergence and incremental-cursor assertions. Skips automatically when the native module isn't built. The existing unit tests mock the engine and could not catch any of the bugs below.

### Fixed

- **Browser — a failed `openDB` locked the OPFS database until page reload** — when the worker's open threw (wrong encryption passphrase, storage error), the exclusive `FileSystemSyncAccessHandle` was never closed and the zombie worker was never terminated, so every retry failed with "Access Handles cannot be created". The worker now closes the handle on a failed open (releasing the Web Lock with it) and the client terminates the worker when `init` rejects — so a user who mistypes a passphrase can simply retry. Verified in headless Chrome (wrong passphrase → correct passphrase retry succeeds in the same page). Latent in all released versions for any failed browser open; made routine by encryption's retryable wrong-passphrase path.
- **Browser — encrypted databases now ignore cross-tab `secondary-write` messages** — the BroadcastChannel handler that merges secondary-tab changesets accepted any nonempty token; since encrypted databases are single-tab by design, this was pure injection surface (a same-origin script could add documents to an encrypted DB without the passphrase). Encrypted mode now drops these messages. Also, the key-derivation salt sidecar is now read/created only after the tab wins the exclusive lock, removing a rare two-tab race on the salt file's access handle.
- **`taladb-core` — the sync cursor collection was rejected as a reserved name** — `db.sync()`'s first pass threw `InvalidName`: the cursor store `__taladb_sync` starts with `_`, which collection-name validation reserves, and no exemption existed. The validator now has an explicit addressable-system-collection allowlist (`__taladb_sync` only); `_audit` and all other `_`-prefixed names stay blocked, and the cursor store stays hidden from `listCollectionNames()`. **This broke 0.8.4 bidirectional sync on every runtime.**
- **`taladb` — `openDB()` was broken on Node.js** — the client destructured `TalaDBNode` from `@taladb/node`, but napi-rs normalizes the Rust struct name to the JS class `TalaDbNode`, so the published 0.8.4 client read `undefined` and crashed on open. A hand-written type alias in the generated `.d.ts` masked the mismatch from the type checker. The client now accepts both names and fails with a clear message if neither exists.
- **Sync cursors were never found after being written** — the cursor document used a caller-supplied `_id: target`, but the engine assigns ULIDs and ignores caller-supplied ids, and the `_id` fast path treats non-ULID strings as matching nothing. Every pass therefore read `sinceMs = 0` (full re-sync, safe but O(all-data) forever) and inserted a fresh orphan cursor document. Cursors are now keyed by a regular `target` field.
- **Pull cursor could permanently skip late-arriving remote changes** — a single local-wall-clock watermark filtered remote changes by their author-time `changed_at`: a change authored before your last sync but arriving at the server after it was never fetched again. Cursors are now split — `pushMs` (local clock, for exports) and `pullMs` (the newest remote `changed_at` actually received) — so late arrivals stay fetchable. Clock skew between peers still affects LWW conflict resolution itself; a server-assigned sequence cursor is on the roadmap as the fully robust design.
- **`taladb` — the browser bundle broke webpack/Next.js builds** — `dist/index.browser.mjs` contained the unreachable `import('@taladb/node')` from the Node adapter; webpack resolves every literal dynamic import, so builds either chased the native `.node` binary into the browser graph (workspace/monorepo setups) or failed with "Can't resolve '@taladb/node'" (apps that rightly don't install it). The browser build now inlines a stub for the specifier — the same treatment the React Native build already applied for Metro. Caught by the new `next build` CI job over the example app.
- **`HttpSyncAdapter` threw `Illegal invocation` in browsers** — it stored `globalThis.fetch` detached; browsers require `fetch` to be called on its global. The adapter now binds it (Node tolerated the detached call, which is why only browser use broke).

## [0.8.4] - 2026-07-11

Bidirectional sync arrives: a local TalaDB can now pull remote changes and push
local ones with Last-Write-Wins merge, plus a first-party MongoDB adapter and a
MongoDB-style aggregation pipeline API on every runtime.

### Added

- **Bidirectional sync — `db.sync(adapter, { collections, direction })`** *(Node.js)* — pulls remote changes into the local database and pushes local ones, tracked by a persisted incremental cursor, with automatic Last-Write-Wins conflict resolution. `direction` is `'both'` (default), `'push'`, or `'pull'`. Built on two new low-level primitives exposed across the bindings — `db.exportChanges(collections, sinceMs)` and `db.importChanges(changeset)` (idempotent under LWW, so replays and at-least-once transports are safe). The local changeset is snapshotted before the remote import, so a change just pulled is never echoed back. Cursors live in a reserved `__taladb_sync` collection (hidden from `listCollectionNames`, never itself synced). See [Bidirectional Sync](https://taladb.dev/guide/bidirectional-sync). *Browser (WASM) and React Native share the same engine and API; their binding wiring is in progress and `db.sync()` throws a clear error there until it lands.*
- **`SyncAdapter` interface + reference `HttpSyncAdapter`** — any transport becomes a sync peer by implementing `push(changeset)` / `pull(sinceMs)`. `HttpSyncAdapter` (a `POST /push` + `GET /pull?since=` REST client) ships inside the `taladb` package with zero extra dependencies — the batteries-included default.
- **Aggregation API — `collection.aggregate(pipeline)`** *(all runtimes)* — MongoDB-style pipeline stages `$match`, `$group`, `$sort`, `$skip`, `$limit`, `$project` with `$sum`, `$count`, `$avg`, `$min`, `$max`, `$push`, `$addToSet`, `$first`, `$last` accumulators. Summaries are computed inside the Rust engine in a single pass — a leading `$match` uses an index — instead of materialising every document in JavaScript. Fully typed via `AggregatePipeline<T>`. See [Aggregation](https://taladb.dev/api/aggregation).
- **`@taladb/sync-mongodb` — MongoDB sync adapter** — syncs directly to a MongoDB collection with no intermediate API. Push does a Last-Write-Wins conditional upsert (a `$cond` pipeline update — correct even when several peers push the same document out of order); pull returns changes newer than the caller's cursor. Doubles as a lightweight sync hub for a fleet of peers. Document bodies are stored as opaque JSON, so field names containing `$` or `.` never collide with MongoDB operators. **Server-side only** — it holds a database credential, so run it on a Node.js backend; browser/mobile apps sync through your own API. See [Bidirectional Sync → MongoDB adapter](https://taladb.dev/guide/bidirectional-sync#mongodb-adapter).

## [0.8.3] - 2026-07-09

Published benchmarks and a Node.js vector-index fix. Ships a rebuilt `.node`
binary — `@taladb/node` gains HNSW support.

### Fixed

- **`@taladb/node` — HNSW vector indexes were silently unavailable** — the prebuilt native module was compiled without the `vector-hnsw` feature, so `createVectorIndex(..., { indexType: 'hnsw' })` and `upgradeVectorIndex()` were silent no-ops and every `findNearest` ran the flat (brute-force) path regardless of the requested index type. The published binary now compiles with `vector-hnsw`; measured at 50k × 384-dim vectors, HNSW answers in 14.6 ms vs 188 ms flat, with 100% recall@10 on clustered (embedding-like) test vectors — see the benchmarks page for the recall and graph-build-cost caveats. (Web and React Native binaries still ship flat-only; enabling HNSW there is tracked separately — WASM size and mobile memory need evaluation first.)

### Added

- **Benchmark suite — `scripts/bench.mjs` (`pnpm bench`)** — reproducible benchmarks against the release `@taladb/node` build: document write throughput (single vs batched), query latency at 100k documents (point get, indexed equality/range, full scan), and vector search at 1k–100k × 384-dim vectors (flat and HNSW, including hybrid pre-filtered search and recall measurement). Deterministic seeded data, medians after warmup.
- **Browser benchmark suite — `scripts/bench-web.mjs` (`pnpm bench:web`)** — the same workload against `@taladb/web` in real headless Chrome with OPFS active, driven over the worker message protocol with no automation dependency (the page reports results back over HTTP). Confirms WASM vector search at parity with native and documents the browser's debounced-snapshot durability model.
- **Docs — `/benchmarks` page** — full result tables for both runtimes, methodology, and tuning guidance (batch your writes, index your hybrid filter fields, prefer one-sided indexed ranges), plus a performance summary in the README.

### Known limitations (documented, not new)

- Two-sided indexed ranges (`$gte` + `$lt` on the same field) use the index for the lower bound only and post-filter the rest; the greedy planner does not yet emit bounded range plans.
- `findNearest` pre-filters materialise full documents (including embedding fields) to collect matching ids; an id-only path would make hybrid queries cheaper.

## [0.8.2] - 2026-06-12

Hardening: sync data-loss fixes, query
correctness on mixed numeric types, security fixes in Studio and the bindings,
and a real live-query API. Ship the npm packages and the prebuilt `.node` /
WASM artifacts together — the JS↔native surface changed in this release.

### Security

- **`taladb-cli` — Studio bound to `0.0.0.0` with no authentication** — anyone on the local network could browse and **delete** documents. Studio now binds `127.0.0.1` by default (new `--host` flag opts out, with a loud warning) and validates the `Host` header against DNS-rebinding attacks, returning `403` for requests carrying a foreign hostname.
- **`@taladb/react-native` — malformed filters silently became match-all** — `parse_filter` degraded any unparseable filter (e.g. a typo'd operator like `{"status":{"$qe":"x"}}`) to `Filter::All`, so a malformed filter passed to `deleteMany`/`updateMany` destroyed or rewrote the **entire collection**. Invalid filters now error through the FFI `taladb_last_error` channel at every call site, and a malformed `findNearest` pre-filter errors instead of silently running an unfiltered search.
- **`taladb-core` — field-level encryption AAD now bound to the document** — ciphertexts were bound to the field name only, so an attacker with write access to the file could transplant doc A's encrypted `ssn` into doc B undetected. New ciphertexts use `field:<doc_id>:<field>` AAD; reads fall back to the legacy field-only AAD so existing data stays readable and upgrades on its next write.
- **`taladb-core` — ULID generation no longer ignores entropy failure** — a failing system RNG previously produced ULIDs with a zeroed random field (predictable, collision-prone); it now panics with a clear message.

### Fixed

- **`taladb-core` — sync: updates propagated to peers as deletions** — applying a remote upsert (LWW), a CRDT merge, or `CrdtSyncAdapter::update_fields` replaced documents via `delete_by_id` + `insert_with_id`, leaving a delete tombstone newer than the document itself. The next export emitted a `Delete` that destroyed the updated document on every peer. All three paths now use the new atomic `Collection::replace_with_id`, which maintains indexes against the previous version and clears the tombstone. Covered by new update → export → import → export → import round-trip tests for both adapters.
- **`taladb-core` — LWW `Delete` import ignored timestamps** — a stale remote tombstone unconditionally deleted a newer local document. Deletions now apply only when `changed_at` is at least as new as the local `_changed_at` (deletes win exact ties), and stale upserts can no longer resurrect a more recently deleted document (tombstone timestamp is checked before re-inserting).
- **`taladb-core` — LWW equal-timestamp conflicts permanently diverged** — the tie-break compared `change.id > local_doc.id`, which is the *same* ULID, so it was always false and each replica kept its own version. Ties now break on the serialized document bytes — a symmetric comparison every replica resolves identically.
- **`taladb-core` — indexed numeric range queries missed cross-type values** — index keys are type-prefixed (`Int` sorts below `Float`), but filters compare Int↔Float numerically. `$lt`/`$lte` with an Int bound never scanned the Float block (and `$gt`/`$gte` with a Float bound never scanned Ints), so `{price: {$lt: 11}}` on an indexed field silently missed `10.5` — the default situation from JavaScript, where `10` maps to Int and `10.5` to Float. The planner now unions a conservatively-widened range of the other numeric type; indexed and unindexed results are asserted identical by new parity tests. Also fixes `$eq` on `0.0` missing stored `-0.0` (and vice versa).
- **`taladb-core` — stale per-handle index cache corrupted indexes** — each `Database::collection()` call got its own index-definition cache, so creating an index through one handle left every other live handle unaware: their writes skipped maintaining the new index forever. The cache is now shared per `Database`, keyed by collection.
- **`taladb-core` — TOCTOU race in filtered mutations** — `update_one/many` and `delete_one/many` gathered candidates in a read snapshot, then mutated in a separate write transaction using the stale documents, losing concurrent updates and leaking stale index entries. Every candidate is now re-fetched and re-checked against the filter inside the exclusive write transaction.
- **`taladb-core` — storage errors during index-definition loading were swallowed** — `unwrap_or_default()` treated transient read errors as "no indexes", silently skipping index maintenance on writes. Errors now propagate (only a genuinely missing table maps to empty).
- **`taladb-core` — encryption: v0→v1 migration could permanently destroy ~1/256 of values** — a v0 ciphertext whose random nonce happens to start with `0x01` was misdetected as already-v1 and skipped; once the rest of the table migrated, it became undecryptable. Migration now verifies "looks like v1" values with an actual decrypt before skipping.
- **`taladb-core` — `find_by_id` and `find_nearest` returned ciphertext for encrypted fields** — both now decrypt configured fields like `find` does; `replace_with_id` re-encrypts symmetrically.
- **`taladb-core` — audit log was not atomic with its mutation** — audit rows were written in a separate transaction after commit: a crash could persist the mutation without its audit row, and an audit error returned `Err` for an already-committed write. Audit rows now commit inside the mutation's own transaction.
- **`taladb-core` — `$in` with duplicate values returned duplicate documents** — adjacent-only `Vec::dedup` missed interleaved ULIDs from identical ranges; now deduplicated with a set.
- **`taladb-core` — `$inc` arithmetic** — i64 overflow now errors instead of wrapping in release builds; `Float += Int` works (previously a `TypeError` while `Int += Float` succeeded); a non-numeric `$inc` delta is rejected.
- **`taladb-core` — `rekey` could not recover from an interrupted run** — each table commits separately, so a partial run left mixed keys and a re-run failed on its own progress. Values that already decrypt under the new key are now skipped, making `rekey` safely re-runnable.
- **`taladb-core` — `Document` accepted duplicate field names** — `get`/`set` only ever saw the first occurrence, but all duplicates serialized to storage. Duplicates are now dropped at construction (first occurrence wins).
- **`taladb-core` — HNSW results shrank below `top_k` after deletions** — deleted ids linger in the in-memory graph and were filtered out of results without replacement. The HNSW path now over-fetches to compensate, and the staleness contract (rebuild via `upgrade_vector_index` after bulk writes) is documented on `find_nearest`.
- **`taladb` — `useFind` stuck on `loading: true` for empty collections** — the browser `subscribe` initialized its change-detection state to `'[]'`, so an initially-empty result was never delivered. First snapshot now always fires.
- **`taladb` — WorkerProxy promises hung forever if the worker died** — in-flight requests are now rejected on `worker.onerror` and on `close()`, and new requests fail fast once the proxy is dead.
- **`@taladb/web` — HTTP sync events could arrive out of order** — one concurrent `fetch` per event let retries reorder deliveries (an update could reach the endpoint before its insert). Events now drain through a strict FIFO queue.
- **`taladb-core` — watch notifications could be silently skipped under lock contention** — `notify` used `try_lock`; a committed write that lost the race never woke its watchers. Now takes the lock.
- **`@taladb/react-native` — `listIndexes()` returned a malformed empty object**; now returns the correct `{ btree: [], fts: [], vector: [] }` shape.

### Added

- **`taladb-core` — `Collection::watch(filter)`: live queries in the Rust core** — the previously-unwired `watch` module is now connected to every write path. A `WatchHandle` yields a fresh snapshot of matching documents after each insert/update/delete, across all handles of the same `Database` (one registry per collection). Rapid writes coalesce; the query re-runs at receive time so no state is ever skipped.
- **`taladb-core` — `Collection::replace_with_id(doc)`** — atomic insert-or-replace preserving the ULID: maintains all indexes against the previous version, clears any delete tombstone, and re-encrypts configured fields. The building block used by both sync adapters.
- **`taladb-core` / `@taladb/web` — explicit `removed_fields` in update sync payloads** — `SyncEvent::Update` now carries a `removed` list and HTTP payloads include `"removed_fields": [...]`, disambiguating "field removed" from "field set to null". Nulls are still emitted in `changes` for older receivers.
- **`@taladb/node` — `close()` and async write variants** — `close()` releases the database file handle/lock; `insertAsync`, `insertManyAsync`, `updateOneAsync`, `updateManyAsync`, `deleteOneAsync`, `deleteManyAsync` run on the libuv thread pool instead of blocking the event loop. The universal `taladb` adapter routes through them automatically, falling back to the sync calls on older prebuilt binaries.
- **Operator parity across bindings** — `$contains` and `$regex` on Node, `$regex` on web (both previously unreachable from those platforms despite core support).
- **`taladb-core` — `_id` primary-key fast path** — `$eq`/`$in` filters on `_id` (including inside `$and`) now resolve to direct point lookups instead of a full collection scan.
- **`taladb-cli` — `taladb studio --host <addr>`** — explicit opt-in for non-loopback binding (see Security).

### Changed

- **Breaking: `{field: {}}` (empty operator object) is now an error on every binding** — previously it errored on Node but silently matched **all documents** on web and React Native. `{}` / `null` as the whole filter still mean match-all.
- **Breaking: collection names starting with `_` are reserved** — `db.collection("_audit")` now returns `InvalidName`, enforcing the audit log's append-only guarantee. System collections are also excluded from `list_collection_names()` / `listCollections()` / Studio.
- **Breaking: an audit-log write failure now fails (rolls back) the mutation** — previously the mutation committed and the API returned an error anyway, inviting duplicate retries.
- **Performance** — `count(filter)` no longer decrypts field contents; mutations no longer open an extra write transaction per call to ensure the `_changed_at` index; `HttpSyncHook` builds its exclude-field set once instead of per event; `useFind`-style pollers are unchanged but cross-tab writes still nudge immediately via BroadcastChannel.

## [0.8.0] - 2026-05-14

### Added

- **`taladb-core` — `CrdtSyncAdapter`: conflict-free multi-device sync with per-field logical clocks** — new `crdt` module implementing bidirectional CRDT-based sync that merges concurrent writes at field granularity rather than whole-document LWW. Two devices can independently write different fields of the same document; both changes survive after sync with no coordination required.

  - **`FieldClock { ts_ms, node_id }`** — per-field logical clock. `dominates()` compares timestamp first; `node_id` lexicographic order breaks ties deterministically across any number of replicas.
  - **`FieldMutation { field, value, clock }`** — single field-level change exported from one replica. `value: None` signals field removal.
  - **`CrdtChange { collection, id, mutations, delete_clock }`** — document-level change record. Either a set of field mutations or a delete (mutually exclusive). Deletions are propagated as tombstones via the same mechanism used by `LastWriteWins`.
  - **`CrdtChangeset = Vec<CrdtChange>`** — the serialisable unit exchanged between peers; no transport is prescribed.
  - **`CrdtAdapter` trait** — `export_crdt_changes(db, collections, since_ms)` / `import_crdt_changes(db, changeset)`.
  - **`CrdtSyncAdapter`** — concrete implementation of `CrdtAdapter`:
    - `new(node_id)` — construct with a stable per-device string identifier.
    - `with_g_set_fields(fields)` — opt specific array fields into grow-only set (G-Set) semantics: merges by union rather than LWW, so concurrent adds from any replica are always preserved.
    - `stamp_insert(fields)` / `stamp_insert_at(fields, ts_ms)` — prepare fields for a CRDT-tracked insert; stamps every non-system field with a per-field clock under `_crdt_clocks` and adds `_changed_at`.
    - `update_fields(col, id, changes)` / `update_fields_at(col, id, changes, ts_ms)` — load a document, advance clocks only for the changed fields, and write back atomically. Required for CRDT tracking on updates (replaces `col.update_one` in CRDT-aware write paths).
  - **Clock storage** — clocks are stored inline in each document as `_crdt_clocks: Object({ field: { t: ts_ms, n: node_id } })`. No schema migrations, no new storage tables. Fully compatible with databases that also use `LastWriteWins`.
  - **Export** — uses the existing `_changed_at` secondary index for an O(log N) range scan. Only exports field mutations whose individual clock is newer than `since_ms`, so incremental sync transfers the minimum necessary data.
  - **Import / merge** — for each incoming `CrdtChange`: loads the local document (or creates a new one if absent), compares per-field clocks, applies only the fields where the remote clock dominates, updates `_crdt_clocks` and `_changed_at`, then writes the merged document back. Documents not touched by the changeset are left completely untouched.
  - All public types re-exported from the crate root: `CrdtAdapter`, `CrdtChange`, `CrdtChangeset`, `CrdtSyncAdapter`, `FieldClock`, `FieldMutation`, `CRDT_CLOCKS_FIELD`.
  - 21 new integration tests in `tests/crdt.rs` covering: `stamp_insert` metadata shape, `update_fields_at` partial clock advance, concurrent writes to different fields, same-field conflict (newer wins / older loses), timestamp-tie tiebreaking by `node_id`, field removal, G-Set union, G-Set deduplication, delete ordering, export `since_ms` filtering, export → import round-trip, and three-way merge.

## [0.7.10] - 2026-04-19

### Fixed

- **`taladb` — `import.meta` crash in Metro/Hermes** — `dist/index.react-native.mjs` contained `import.meta.url` references from `createBrowserDB` and `createInMemoryBrowserDB`, which are dead code on React Native but were still included in the bundle. Hermes does not support `import.meta` syntax and threw `SyntaxError: import.meta is not supported in Hermes`. Added an esbuild `define` in the react-native tsup build to replace all `import.meta.url` occurrences at compile time.

- **`taladb` — Metro bundling failure: `@taladb/node` could not be resolved** — `createNodeDB` contains a dynamic `import('@taladb/node')` that is dead code on React Native, but Metro resolves every `import()` specifier statically even in unreachable branches. Because `@taladb/node` is an optional peer dependency, tsup automatically externalised it, leaving the specifier in the output for Metro to fail on. Fixed by adding `noExternal: ['@taladb/node']` to the react-native tsup config and using an esbuild `onResolve` plugin to redirect `@taladb/node` to an inline stub that Metro can bundle without error.

- **`taladb` — `openDB` routed to browser adapter on React Native New Architecture** — `detectPlatform()` checked `nativeCallSyncHook` first to detect React Native, but this global is no longer set in RN New Architecture (0.71+). Because React Native also exposes `window` and `navigator`, the function fell through to the browser branch and called `createBrowserDB`, which immediately crashed on the missing `Worker` global. Fixed by checking `navigator.product === 'ReactNative'` first — the canonical, version-stable React Native detection.

- **`taladb` — `createNativeDB` called non-existent `collection()` method on JSI HostObject** — the `NativeHostObject` interface modelled the JSI HostObject as a nested API (`native.collection('notes').insert(doc)`), but the actual C++ HostObject (`TalaDBHostObject.cpp`) exposes a flat API where every method takes the collection name as its first argument (`native.insert('notes', doc)`). Every collection operation threw `native.collection is not a function`. Rewrote `createNativeDB` and its `NativeDB` interface to match the actual flat JSI surface.

## [0.7.9] - 2026-04-19

### Fixed

- **`@taladb/react-native` — Android `dlopen` crash: `library libtaladb_ffi.so not found`** — `cargo ndk` builds Rust cdylib files without a SONAME. CMake's `SHARED IMPORTED` + `IMPORTED_LOCATION` recorded the full build-time path in `DT_NEEDED`, so the dynamic linker searched for a path like `../../../../src/main/jniLibs/arm64-v8a/libtaladb_ffi.so` at runtime instead of the simple `libtaladb_ffi.so` soname. Fixed by replacing the `SHARED IMPORTED` target with `link_directories()` pointing at the ABI-specific jniLibs folder and linking by name (`-ltaladb_ffi`), which produces `DT_NEEDED: libtaladb_ffi.so` and lets the Android linker find the library via its standard search path.

- **`@taladb/react-native` — `TalaDBModule.kt` was missing `apply plugin: "kotlin-android"`** — without the Kotlin plugin, Gradle could not compile the module's Kotlin source and the package class (`TalaDBPackage`) was never available. Added the plugin to `android/build.gradle`.

## [0.7.8] - 2026-04-19

### Fixed

- **`@taladb/react-native` — C++ compilation crash in `taladb.h`** — a nested `/* ... */` block comment on line 195 caused the compiler to terminate the outer comment early and attempt to parse the remaining documentation as C++ code, producing `expected unqualified-id` and `unknown type name` errors. Changed the inner comment to a `//` line comment.

## [0.7.7] - 2026-04-19

### Fixed

- **`@taladb/react-native` — Android STL mismatch** — React Native 0.76+ requires the shared C++ runtime (`c++_shared`). Without `ANDROID_STL=c++_shared` in the CMake arguments, the build system defaulted to the static STL, which is incompatible with Hermes and the TurboModule infrastructure. Added `arguments "-DANDROID_STL=c++_shared"` to `android/build.gradle`.

## [0.7.6] - 2026-04-19

### Fixed

- **`@taladb/react-native` — Android module never compiled into the app** — `android/build.gradle` was missing. Without it, Gradle did not treat `android/` as a library module — `TalaDBModule.kt` was never compiled and CMake was never invoked to build `libtaladb_jsi.so`. This was the root cause of the `TurboModuleRegistry.getEnforcing('TalaDB'): 'TalaDB' could not be found` crash on Android.

- **`@taladb/react-native` — TurboModule codegen never ran** — `codegenConfig` was missing from `package.json`. React Native's Gradle plugin reads this field to generate `NativeTalaDBSpec` — the base class `TalaDBModule.kt` extends. Without it, the Kotlin code could not compile even when `build.gradle` was present.

- **`@taladb/react-native` — missing `AndroidManifest.xml`** — added `android/src/main/AndroidManifest.xml`, required for Gradle to recognise the `android/` directory as a valid Android library module.

## [0.7.5] - 2026-04-19

### Fixed

- **`taladb` — React Native installs pulled in `@taladb/web` and `@taladb/node`** — both packages were declared as `optionalDependencies`, which package managers (npm, yarn, pnpm) install by default. Moved to optional `peerDependencies` so only the adapter the consumer actually needs is installed. **Breaking:** web and Node.js users must now explicitly install `@taladb/web` or `@taladb/node` alongside `taladb` (the docs have always shown this; the auto-install was the anomaly).

- **`taladb` — Metro bundler resolved the wrong entry point** — added a `react-native` export condition pointing to a dedicated build (`dist/index.react-native.mjs`). Metro now resolves this entry instead of the default Node.js ESM build, preventing it from statically analysing or bundling `@taladb/web` or `@taladb/node` imports.

## [0.7.4] - 2026-04-18

### Added

- **`@taladb/react-native` — Float32Array zero-copy fast path** — `findNearest` now accepts a `Float32Array` directly via JSI `ArrayBuffer` introspection, bypassing JSON serialisation entirely. Eliminates the per-call f64→f32 conversion loop for large embeddings (768 / 1024 / 1536 dimensions). Falls back to `number[]` automatically for callers that pass plain arrays.

- **`@taladb/react-native` — async vector search + full-scan** — two new JSI methods dispatched to background OS threads via a `TalaDbJob` handle:
  - `findNearestAsync(collection, field, query, topK, filter?)` → `Promise<Array>` — runs HNSW / flat ANN search off the JS thread; avoids dropping frames during large unfiltered scans.
  - `findAsync(collection, filter?)` → `Promise<Array>` — full collection scan on a background thread; keeps the JS thread responsive for paginated or unbounded queries.
  - Both return native Promises polled via `setImmediate` — no CallInvoker plumbing required, compatible with Hermes and JSC without changes to the Android or iOS module layer.

- **`@taladb/node` — Float32Array zero-copy fast path** — new `findNearestF32(field, query: Float32Array, topK, filter?)` method on `CollectionNode`. Passes the underlying `f32` slice directly to `Collection::find_nearest` without allocating a conversion buffer.

- **`@taladb/node` — async variants via napi `AsyncTask`** — two new methods that run on the libuv thread pool:
  - `findNearestAsync(field, query: Float32Array, topK, filter?)` → `Promise<Array<{ document, score }>>` — offloads ANN search to a worker thread; resolves on the JS thread.
  - `findAsync(filter?)` → `Promise<Array>` — offloads full collection scan; useful for large datasets where blocking the event loop would be noticeable.

- **Vector index management — React Native** — `createVectorIndex`, `dropVectorIndex`, and `upgradeVectorIndex` are now exposed through the JSI HostObject and declared in the TurboModule Codegen spec (`NativeTalaDB.ts`). Supports `flat` and `hnsw` index types; `opts` accepts `{ metric, m, efConstruction }`.

[0.7.4]: https://github.com/taladb/taladb/compare/v0.7.3...v0.7.4

## [0.7.3] - 2026-04-17

### Added

- **`db.compact()`** — on-demand WAL compaction, exposed across every platform layer. Call during idle periods after bulk deletes or tombstone pruning to reclaim disk space.
  - **`taladb-core`** — `StorageBackend::compact()` default no-op + `RedbBackend` implementation calling `redb::Database::compact()`. `Database::compact()` public method re-exported from the crate root. `Arc<Mutex<Database>>` wrapping ensures safe `&mut self` access without holding the lock across transaction lifetimes.
  - **`@taladb/web`** — `WorkerDB.compact()` wasm-bindgen method; `'compact'` op dispatched in the SharedWorker. OPFS-specific imports (`FileSystemSyncAccessHandle`, `openWithOpfs`, `openWithConfigAndOpfs`) gated behind new `cf-workers` Cargo feature so the Cloudflare Workers WASM build is free of browser-only web-sys bindings.
  - **`@taladb/node`** — `TalaDBNode.compact()` napi binding.
  - **`@taladb/react-native`** — `taladb_compact(handle)` C FFI function; `"compact"` property dispatched from `TalaDBHostObject`; declared in `taladb.h`.
  - **`taladb`** — `compact(): Promise<void>` added to the `TalaDB` interface and wired in all four adapter return objects (browser worker, Node.js, React Native, in-memory fallback).

- **`@taladb/cloudflare`** — new package: TalaDB adapter for Cloudflare Workers Durable Objects.
  - Runs the existing `@taladb/web` WASM core in in-memory mode (no OPFS required); state is persisted as a binary snapshot via Durable Objects `storage.put('__taladb_snapshot__')` between requests.
  - `openDurableDB(storage)` — open a TalaDB-compatible database from `this.ctx.storage`; restores from the last saved snapshot on cold-start / hibernation wake-up.
  - `CloudflareDB` — full TalaDB-compatible handle with `collection()`, `flush()`, `compact()`, and `close()`.
  - `TalaDBDurableObject` — base class; extend and export from your Worker. Provides `getDB()` (lazy init, cached for isolate lifetime) and a default `fetch()` override point.
  - `createVectorIndex` throws a clear error when `indexType === 'hnsw'` (requires native threads unavailable in Workers).
  - Full TypeScript declarations (`index.d.ts`).

- **Bun native module support** — `@taladb/node` now works on Bun out of the box via Bun's built-in N-API compatibility layer. No separate `bun:ffi` package needed — install `@taladb/node` and use it identically to Node.js. Added `"bun": ">=1.0"` to `engines`. Added Linux ARM64 (`aarch64-unknown-linux-gnu`) and Intel Mac (`x86_64-apple-darwin`) prebuilt targets alongside the existing ones.

- **`taladb studio` — local web UI** — new `taladb studio <file>` CLI command that starts a local HTTP server and opens a browser-based database explorer.
  - Collections listed in the sidebar with live document counts.
  - Paginated document table (50 per page) with dynamic column detection across all unique fields in the current page.
  - Client-side search bar — filters the current page by full-document JSON substring match with no round-trip.
  - Click any row to open a detail panel showing the full pretty-printed document JSON.
  - Delete documents from the row action or the detail panel (with confirmation).
  - `--port <n>` (default `4321`) and `--no-open` flags.
  - Built with `tiny_http` (sync, no Tokio runtime) and a single embedded HTML file — no external assets, no bundler. The binary is fully self-contained.

- **Zod / Valibot schema validation** — optional runtime type safety for `Collection<T>`. Pass a `schema` option to `db.collection()` to validate documents before insert.
  - Compatible with Zod (`z.object(…)`), Valibot, or any object with `parse(data: unknown): T`.
  - Validates on `insert` and `insertMany`; optionally on `find` / `findOne` via `validateOnRead: true`.
  - Throws `TalaDbValidationError` (exported from `taladb`) with the context label (`insert`, `insertMany[2]`) and the original schema library error as `cause`.
  - Collections without a `schema` option are unchanged — zero overhead.
  - `@taladb/cloudflare` exposes the same `schema` option and exports its own `TalaDbValidationError`.

[0.7.3]: https://github.com/taladb/taladb/compare/v0.7.2...v0.7.3

## [Unreleased] — 0.7.2

### Added

- **`rekey(backend, old_key, new_key)`** (`encryption` feature) — rotate the AES-GCM-256 encryption key on a live database without a full export/import cycle. Iterates every table in the raw backend, decrypts each value with `old_key`, and re-encrypts with `new_key` in an atomic transaction per table. Returns the count of re-encrypted values. Passing the wrong `old_key` fails immediately with `TalaDbError::Encryption`. Re-exported from the crate root as `taladb_core::rekey`.

- **`Collection::with_field_encryption(fields, key)`** (`encryption` feature) — per-field AES-GCM-256 encryption. Listed field values are encrypted before storage (using `field:<name>` as AAD so ciphertexts cannot be transplanted between fields) and decrypted transparently on all read paths (`find`, `find_with_options`, `find_one`). All other fields remain in plaintext and fully indexable. Encrypted fields are stored as `Value::Bytes` and cannot be queried by value.

- **`Collection::with_audit_log(caller)`** — opt-in append-only audit log. After every successful mutation (`insert`, `insert_many`, `update_one`, `update_many`, `delete_one`, `delete_many`) an entry is written to the `_audit` table containing: `collection`, `op` (`"insert"` | `"update"` | `"delete"`), `doc_id`, `ts` (ms since Unix epoch), and the `caller` identity string supplied by the application. No update or delete API exists for audit records. Read with `taladb_core::read_audit_log(backend, collection_filter, op_filter)`.

- **`read_audit_log(backend, collection_filter, op_filter)`** — scan the `_audit` table and return `Vec<AuditEntry>`, optionally filtered by collection name and/or operation type. Returns entries in ULID insertion order.

- **`AuditEntry`**, **`AuditOp`** — public types for working with audit log entries, re-exported from the crate root.

## [Unreleased] — 0.7.1

### Added

- **Query timeouts** — `FindOptions` has a new optional `timeout: Option<std::time::Duration>` field. When set, `find_with_options` checks elapsed time between candidate documents and returns `Err(TalaDbError::QueryTimeout)` if the deadline is exceeded. No-op when `None` (default). Bindings for WASM, Node.js, and React Native surface this as a `"QueryTimeout"` error code.

- **`TalaDbError::QueryTimeout` variant** — new error variant returned when a query exceeds its configured timeout.

- **`tracing` spans on heavy operations** — `Collection::find`, `Collection::find_with_options`, `Collection::find_nearest`, and `Database::export_snapshot` are now annotated with `#[tracing::instrument]`. Operators using an OpenTelemetry or Jaeger subscriber will see per-call spans with `collection` and `top_k` fields automatically.

- **Fuzz targets** (`packages/taladb-core/fuzz/`) — two `cargo-fuzz` targets added:
  - `fuzz_snapshot`: feeds arbitrary bytes into `Database::restore_from_snapshot` to catch panics in the snapshot parser.
  - `fuzz_filter`: deserializes arbitrary bytes as `Document` and runs filter evaluation, catching deserialization and matching panics.
    Run with `cargo fuzz run fuzz_snapshot` from `packages/taladb-core/`.

## [Unreleased] — 0.7.0

### Breaking Changes

- **`Database::collection()` now returns `Result<Collection, TalaDbError>`** instead of `Collection`. Call sites must handle the error with `?` (in functions returning `Result`) or `.unwrap()` (in tests). Returns `TalaDbError::InvalidName` if the name is empty, longer than 128 characters, or contains `"::"`.

- **Encrypted data format v0 is unreadable by `>= 0.6.2`**. Any database encrypted before `0.6.2` (without the version byte and AAD binding) must be migrated with the new `migrate_encrypted_v0_to_v1` helper before the upgrade can be completed.

### Added

- **`migrate_encrypted_v0_to_v1(backend, key)`** — one-time migration helper for databases encrypted before `0.6.2`. Reads every stored value using the old format (`[12-byte nonce][ciphertext]`, no AAD), re-encrypts it with the current v1 format (version byte + AAD binding), and writes the result back atomically per table. Returns the count of values re-encrypted. Requires the `encryption` feature.

- **Collection name validation at handle construction** — `Database::collection()` now validates the name eagerly (empty, >128 chars, contains `"::"`), surfacing errors at handle creation time rather than silently at index creation time. Also adds a 128-character maximum length check.

- **`tracing` crate integration** — all internal `eprintln!` calls replaced with structured `tracing::warn!` / `tracing::error!` calls. Operators can now route TalaDB diagnostics through any `tracing` subscriber (JSON stdout, OpenTelemetry, Datadog). Zero overhead when no subscriber is installed.

- **HTTP sync bounded thread pool** — `HttpSyncHook` no longer spawns a new OS thread per write event. A fixed pool of 4 background workers shares a bounded channel (capacity 256). Events are dropped with a `tracing::warn!` when the pool is saturated, rather than spawning unbounded threads. Each worker creates its own `reqwest::blocking::Client` to avoid the "cannot drop runtime in async context" panic in tokio test environments.

- **Snapshot size guard** — `Database::restore_from_snapshot()` now rejects inputs larger than 10 GiB, preventing OOM conditions from corrupted or crafted snapshots.

## [0.6.1] - 2026-04-13

### Added

- **Auto `_changed_at` stamping** — every `insert`, `insertMany`, `updateOne`, and `updateMany` now automatically sets `_changed_at` to the current wall-clock time. Developers no longer need to call `stamp()` manually. `insert_with_id` (used by the sync adapter to import remote documents) is intentionally exempt so remote timestamps are never overwritten.

- **`_changed_at` secondary index** — the first mutating call on any collection automatically creates a secondary B-tree index on `_changed_at`. `export_changes` now uses an index range scan (`Filter::Gt`) instead of a full table scan, reducing export cost from O(N) to O(log N + results).

- **Delete tombstones** — `delete_one`, `delete_many`, and `delete_by_id` now write a timestamped tombstone entry to a per-collection `tomb::<name>` table after every hard delete. `export_changes` includes tombstone entries in the returned changeset so deletions propagate correctly to remote replicas that poll after the delete occurred. `import_changes` writes tombstones locally on receipt so they can be forwarded to further downstream peers.

- **`tomb_table_name(collection)` helper** — new public function in `taladb-core::index` returning the `"tomb::<name>"` table name; used internally by the sync adapter and tombstone compaction.

- **`Collection::compact_tombstones(before_ms)`** — prune tombstones older than `before_ms` (milliseconds since Unix epoch) from a collection. Returns the count of entries removed. Call periodically once all replicas are known to have received older deletions.

- **`WorkerDB::compactTombstones(collection, beforeMs)`** — Wasm binding for tombstone compaction, dispatched as the `compactTombstones` op in the Worker.

- **Bidirectional sync changeset API** — `WorkerDB::exportChangeset(collectionsJson, sinceMs)` and `WorkerDB::importChangeset(changesetJson)` are now exposed as `#[wasm_bindgen]` methods and as `exportChangeset` / `importChangeset` Worker ops. Enables pull-based sync over any transport (fetch polling, WebSocket, SSE) without baking a transport into the library. Uses `LastWriteWins` with ULID tie-breaking for deterministic merge.

- **`WorkerDB::listCollections()`** — returns a JSON array of all collection names in the database. Used internally by the secondary-tab write relay; also available to application code via the `listCollections` Worker op.

- **Secondary-tab write propagation** — writes on a non-primary (IndexedDB-fallback) tab are now relayed to the primary (OPFS) tab automatically. After every mutating op, a fallback tab exports a delta changeset and posts it on the `BroadcastChannel`. The primary tab calls `importChangeset`, merges the changes into OPFS via Last-Write-Wins, and broadcasts `taladb:changed` so all tabs stay consistent. No application code required.

- **Debounced IDB snapshot** — the IndexedDB fallback path previously flushed a full snapshot after every single write. Flushes are now debounced: the write fires after 500 ms of idle or at most every 5 s under continuous load. `db.close()` performs a synchronous flush before releasing the Web Lock so no writes are lost on tab close.

- **`Changeset`, `LastWriteWins`, `SyncAdapter` re-exported** — these types are now re-exported from the `taladb-core` crate root alongside the existing `SyncHook` / `SyncEvent` re-exports.

- **`Database::backend()`** — new `pub(crate)` accessor returning the raw `&dyn StorageBackend`; used by the sync adapter to read/write tombstone tables directly without going through the collection API.

### Fixed

- **`import_changes` applied count for missing deletes** — `ChangeOp::Delete` previously incremented `applied` unconditionally, causing `import_delete_nonexistent_returns_zero` to fail. The tombstone is still written (so the deletion can be forwarded) but `applied` only increments when the document actually existed locally.

- **`export_changes_returns_docs_after_since_ms` test** — the test constructed documents with controlled `_changed_at` values by calling `stamp()` then overriding the field before passing to `col.insert()`. Auto-stamping inside `insert()` silently discarded the override. The test now uses `Document::new` + `col.insert_with_id` (the import path) to plant documents with precise timestamps.

### Docs

- **`web.md` fully rewritten** — restructured to reflect all new capabilities: multi-tab behaviour section with ASCII flow diagram, bidirectional sync section with fetch-polling example, tombstone management with `compactTombstones` usage, conflict resolution explanation. Limitations section updated: secondary-tab writes no longer listed as a hard limitation; snapshot size ceiling and HNSW threading constraint documented accurately.

## [0.6.0] - 2026-04-12

### Added

- **HTTP push sync** — event-driven, webhook-style sync that pushes mutation events from the database to a remote server over HTTP:
  - `taladb.config.json` / `taladb.config.ts` support — configure `sync.pushEndpoint`, `sync.authToken`, and `sync.retryPolicy` per project
  - `SyncHook` trait implementation — every `insert`, `updateOne`, `updateMany`, `deleteOne`, `deleteMany` emits a typed `SyncEvent` to a background dispatcher
  - **Browser / WASM** — `SyncConfig` wired into the WASM worker; mutations are dispatched from `worker_db.rs` via `fetch()` on the worker thread; no main-thread blocking
  - **Node.js** — `loadConfig` / `validateConfig` read the project config file at startup and wire the sync hook into the native module
  - **React Native** — `TalaDBModule.initialize({ pushEndpoint, authToken })` passes sync config to the JSI layer at runtime; no config file needed
  - Configurable retry policy: exponential back-off with jitter, configurable `maxRetries` and `initialDelayMs`
  - `SyncEvent` payload includes `collection`, `operation`, `documentId`, `timestamp`, and the full document diff
  - CLI: `taladb sync status` shows pending event queue depth and last-push timestamp

### Fixed

- **Android `TalaDBModule` — wrong library loaded** — `System.loadLibrary("taladb_ffi")` loaded the raw Rust crate which has no JNI entrypoints; corrected to load `taladb_ffi` then `taladb_jsi`
- **Android `TalaDBModule` — wrong JSI runtime pointer** — `jsCallInvokerHolder.nativeCallInvoker` returns a `CallInvoker*`, not a `jsi::Runtime*`; fixed to `reactContext.javaScriptContextHolder!!.get()`
- **iOS podspec — static library replaced with XCFramework** — `s.vendored_libraries = "libtaladb_ffi.a"` replaced with `s.vendored_frameworks = "TalaDBFfi.xcframework"` to support both device and Apple Silicon simulator slices; CI now builds three targets (`aarch64-apple-ios`, `aarch64-apple-ios-sim`, `x86_64-apple-ios`) and packages them via `xcodebuild -create-xcframework`
- **Clippy: wasm32-only imports in `worker_db.rs`** — `HashMap`, `Arc`, `SyncEvent`, `SyncHook`, and related types were declared at crate level but only used inside `#[cfg(target_arch = "wasm32")]` blocks; moved behind matching `#[cfg]` guards to fix unused-import warnings in the CI `--target x86_64-unknown-linux-gnu` check
- **Browser bundle — `js-yaml` import error** — `taladb`'s `config.ts` pulled in `js-yaml`, `node:fs`, and `node:path`; Vite statically analysed these even behind runtime guards and threw at startup. Fixed with a separate browser build entry (`index.browser.mjs`) that substitutes a no-op `config.browser.ts` stub via an esbuild plugin; the `browser` export condition in `package.json` ensures Vite resolves this path automatically
- **Playground `browser` export condition not applied** — `dev-playground.sh` copied `dist/` but not `package.json`, so the playground's `node_modules/taladb` still resolved `index.mjs` (the Node build) instead of `index.browser.mjs`; fixed by also copying `package.json`

### Improved

- **`web.md` browser support table** — updated to reflect the actual DedicatedWorker + Web Locks architecture: OPFS is the primary fast path; IndexedDB snapshot fallback activates immediately via `{ ifAvailable: true }` (no blocking wait); secondary tabs are live-synced via `BroadcastChannel`
- **`react-native.md`** — HTTP push sync section added; removed stale "not yet published to npm" limitation (package is live at `@taladb/react-native@0.5.0`); dropped `x86` ABI from Android build matrix (no current-generation device or emulator requires it)
- **Android NDK version pinned** — CI now installs NDK `27.2.12479018` explicitly via `setup-android`

## [0.5.0] - 2026-04-12

### Added

- **`@taladb/react` — React hooks package** — first-party hooks for live-updating queries in React and React Native:
  - `TalaDBProvider` / `useTalaDB` — context provider for the `TalaDB` instance
  - `useCollection<T>(name)` — returns a stable, memoised `Collection<T>` from context; safe to pass directly to `useFind` / `useFindOne` without wrapping in `useMemo`
  - `useFind<T>(collection, filter?)` — subscribes to a live query; returns `{ data: T[], loading: boolean }` and re-renders automatically on every matching write
  - `useFindOne<T>(collection, filter)` — same as `useFind` but returns the first matching document (`data: T | null`)
  - All hooks are backed by `useSyncExternalStore` for zero-tearing snapshots in concurrent React
  - Inline filter objects (e.g. `{ active: true }`) are serialised to a string key internally — no re-subscription on every render
  - Works in React (browser + Node.js) and React Native with identical API; no platform-specific code required
  - Full TypeScript generics — `data` is correctly typed as `T[]` / `T | null` from the document interface
  - Install: `pnpm add taladb @taladb/web @taladb/react` (browser) or `pnpm add taladb @taladb/react-native @taladb/react` (React Native)

### Infrastructure

- CI `ts-check` job now type-checks `@taladb/react` alongside `taladb`
- CI `build-taladb-react` job runs unit + integration tests (52 cases) then builds and verifies `dist/` before publish
- Release workflow publishes `@taladb/react` to npm as part of the standard release pipeline

## [0.4.0] - 2026-04-11

### Added

- **Full-text search index** — `collection.createFtsIndex(field)` builds an inverted token index on any string field. `$contains` queries now use a `FtsScan` plan (O(1) token lookup) instead of a `FullScan` (O(n) document scan). Drop with `dropFtsIndex(field)`. Backfills existing documents on creation.
- **HNSW vector index** — `createVectorIndex(field, { indexType: 'hnsw', hnswM?, hnswEfConstruction? })` builds a Hierarchical Navigable Small World graph for approximate nearest-neighbour search. Significantly faster than flat brute-force on large collections. Upgrade an existing flat index in-place with `upgradeVectorIndex(field)`. Available on Node.js and React Native; browser WASM now throws a clear error (requires native threads via `rayon`).
- **`listIndexes()` API** — `await collection.listIndexes()` returns `{ btree: string[], fts: string[], vector: string[] }` describing every index currently on the collection. Available on all three runtimes.
- **Query planner plans** — the query engine now selects from four execution strategies: `FullScan` (no usable index), `IndexScan` (B-tree, O(log n)), `FtsScan` (inverted FTS index, O(1)), and `IndexOr` (sorted-merge union of index scans, zero duplicates).

### Fixed

- **`createFtsIndex` idempotency** — previously returned `Err(IndexExists)` when called on a collection that already had the index. Now returns `Ok(())` (no-op), matching the behaviour of `createIndex` and `createVectorIndex`.
- **`$contains` missing from WASM filter parsers** — the `$contains` operator was not handled in `worker_db.rs` or `lib.rs`, causing every `find({ field: { $contains: ... } })` call in the browser to return `Error: invalid filter`. Added to both parsers.
- **`Update` enum missing `Clone` derive** — `updateMany` called `update.clone()` internally but `Update` did not derive `Clone`, causing a compile error surfaced by a full incremental rebuild. Fixed with `#[derive(Clone)]`.
- **Multi-tab OPFS deadlock** — a second browser tab would block indefinitely waiting for the OPFS Web Lock held by the first tab. Fixed with `{ ifAvailable: true }` — the second tab now falls back to an IndexedDB-backed in-memory database immediately and stays live-synced via `BroadcastChannel`.

## [0.2.1] - 2026-04-05

### Fixed

- **Sync correctness** — `LastWriteWins` adapter was using `Filter::Eq("_id", ...)` to look up existing documents, which never matched because `_id` is not a document field. Every sync operation incorrectly inserted duplicates instead of updating. Fixed by adding `find_by_id`, `delete_by_id`, and `insert_with_id` methods and routing sync through them.
- **Panic on corrupted storage keys** — vector search and snapshot parsing used `.unwrap()` on byte slice conversions after bounds checks. Replaced with proper error propagation (`InvalidSnapshot`) and graceful `continue` on malformed vector table keys.
- **Encryption stubs panicked instead of returning `Err`** — calling any encryption function without the `encryption` feature compiled in caused a `panic!`. Now returns `Err(TalaDbError::Encryption(...))` so callers can handle it.
- **Watch backpressure indistinguishable from disconnect** — `WatchRegistry::notify()` silently dropped slow subscribers. Added `TalaDbError::WatchBackpressure` variant; full-channel drops now log a warning and use the new variant, distinguishable from `WatchClosed` (channel disconnected).
- **Unnecessary `unwrap` in f32 vector decode** — `decode_f32_vec` used `c.try_into().unwrap()` inside `chunks_exact(4)`. Replaced with direct byte indexing `[c[0], c[1], c[2], c[3]]`.

### Improved

- **Index metadata cache** — `insert`, `update_one`, `update_many`, `delete_one`, `delete_many` previously loaded index metadata (regular, FTS, vector) from redb on every call (3 table scans per write). Metadata is now cached in a `Mutex<Option<CachedIndexes>>` on the `Collection` and invalidated only when indexes are created or dropped.
- **`IndexOr` query plan** — previously loaded full documents from each OR branch before deduplicating by ID. Now collects ULIDs first, deduplicates, then fetches documents once.
- **FTS `$contains` filter** — query string was tokenized once per document evaluated. Tokens are now computed once before the document loop.
- **Consistent error shapes across adapters** — WASM now returns `{error, code}` JSON objects; Node.js errors are prefixed with the variant name; React Native FFI exposes `taladb_last_error()` to retrieve the last error message from C/C++.
- **`apply_update` key allocations** — `$set`, `$push`, `$pull` operations now move field name strings instead of cloning them.

## [0.2.0] - 2026-04-05

### Added

- **Vector index (flat search)** — hybrid document + vector database support
  - `Collection::create_vector_index(field, dimensions, metric?)` — register a vector index on any numeric-array field; backfills existing documents automatically
  - `Collection::drop_vector_index(field)` — remove a vector index and all stored vectors
  - `Collection::find_nearest(field, query, top_k, pre_filter?)` — flat (brute-force) cosine / dot / euclidean similarity search; optional metadata pre-filter lets you combine regular NoSQL filtering with vector ranking in one call
  - `VectorMetric` enum: `Cosine` (default), `Dot`, `Euclidean`
  - `VectorSearchResult { document, score }` return type
  - Vector data stored in dedicated `vec::<collection>::<field>` redb tables (raw f32 LE bytes, keyed by ULID); kept in sync with insert / update / delete
  - Full bindings across all three runtimes: `@taladb/web` (WASM + SharedWorker), `@taladb/node` (napi-rs), `@taladb/react-native` (JSI)
  - TypeScript: `createVectorIndex`, `dropVectorIndex`, `findNearest` on `Collection<T>`; new exported types `VectorMetric`, `VectorIndexOptions`, `VectorSearchResult<T>`
  - New error variants: `VectorIndexNotFound`, `VectorDimensionMismatch`

## [0.1.2] - 2026-03-30

### Fixed

- CI: removed `publish-crates` job so GitHub Release is no longer blocked by crates.io

## [0.1.1] - 2026-03-30

### Fixed

- tsup build: mark `@taladb/web`, `@taladb/node`, `@taladb/react-native` as external to avoid bundle-time resolution errors
- clippy: `thread_local!` initializer made `const` in `document.rs`

## [0.1.0] - 2026-03-30

### Added

- Initial public release of TalaDB
- `taladb` — unified TypeScript package with platform auto-detection
- `@taladb/web` — browser WASM bindings via wasm-bindgen + OPFS SharedWorker
- `@taladb/node` — Node.js native module via napi-rs
- `@taladb/react-native` — full iOS + Android TurboModule integration via JSI HostObject; universal `.a` via `lipo` for iOS, Gradle AAR packaging for Android
- `taladb-core` — Rust core library published to crates.io
- `taladb-cli` — CLI tools (`inspect`, `export`, `import`, `count`, `drop`) published to crates.io
- MongoDB-like filter and update DSL (`$eq`, `$ne`, `$gt`, `$gte`, `$lt`, `$lte`, `$in`, `$nin`, `$exists`, `$and`, `$or`, `$not`, `$contains`)
- `$or` index union across different indexed fields using sorted-merge join on ULIDs (e.g. `{ $or: [{ status: 'pinned' }, { priority: 1 }] }`)
- CLI interactive shell: `taladb shell <file>` — REPL with JSON filter expressions, formatted table output, and tab-completion for collection and field names
- Secondary B-tree indexes with automatic index selection
- ACID transactions backed by [redb](https://github.com/cberner/redb)
- Full-text search via inverted token index (`$contains`)
- Live query subscriptions (`collection.subscribe()`)
- Optional AES-GCM-256 encryption at rest (PBKDF2-HMAC-SHA256 key derivation)
- Versioned, atomic schema migrations
- Binary snapshot export / import
- SharedWorker + OPFS persistence for browsers; in-memory fallback for Safari iOS
- Comprehensive VitePress documentation site
