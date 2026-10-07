---
title: Roadmap
description: Planned and in-progress features for TalaDB
---

# Roadmap

What's planned for TalaDB, roughly in order of impact. Shipped work is recorded
in the [changelog](https://github.com/taladb/taladb/blob/main/CHANGELOG.md);
this page tracks only what's still open.

Have an idea, or want to help prioritise? Open a
[GitHub Discussion](https://github.com/taladb/taladb/discussions) or a feature
request issue.

---

## Developer experience

- **Change webhook delivery guarantees** — coalescing so a bulk import sends one
  request instead of hundreds, an opt-in durable outbox for at-least-once
  delivery, and acknowledged multi-tab write forwarding. See the
  [webhook API](/api/webhook).
- **Schema migrations on React Native** — the version accessors are wired
  through the native stack and await on-device verification. Read-time
  migrations already ship on browser and Node — see
  [Schema Validation](/api/schema).
- **Compound index coverage** — use an index when only the leading fields are
  constrained or the last is a range, per-field descending order, and an array
  shorthand for `createIndex`.
- **`taladb generate`** — emit TypeScript interfaces for each collection,
  inferred from the documents already stored.
- **Svelte and Vue adapters** — `@taladb/svelte` stores and `@taladb/vue`
  composables, over the same event model as the [React hooks](/guide/react).
- **VS Code extension** — filter-expression highlighting, inline document
  previews, and a collection browser.

---

## Performance & vector search

The goal is to keep TalaDB among the fastest embedded databases on every
JavaScript runtime.

- **Better approximate-search recall at scale** — improve candidate selection
  and graph connectivity, validated against exact search on larger collections
  and representative embedding datasets.
- **Automatic native memory signals** — connect Android/iOS memory hints and
  pressure callbacks in the native packages. Adaptive cache sizing, browser
  device-memory hints and explicit pressure/recovery commands already ship;
  native hosts currently forward signals themselves.
- **Wider native SIMD** — a runtime-detected AVX2/NEON kernel on top of the
  portable vectorisation already in place.
- **Index tuning guidance by device class** — recommended parameters from
  low-memory phones through desktops.
- **Broader benchmark coverage** — automate physical Android/iOS device runs,
  cover more browser engines and representative embedding datasets, and publish
  peak-memory measurements and trends across releases. Native and Chromium
  worker/OPFS comparisons already run in CI; manual phone-browser runs use the
  [vector benchmark workload](/guide/vector-benchmarks).

---

## Storage

- **Pluggable serialisation** — swap the internal encoding for MessagePack or
  CBOR, to interoperate with formats you already use.
- **Document TTL** — set an expiry when you write a document and have it swept
  automatically.

---

## Platform

- **WASI target** — run the same engine inside Wasmtime, WasmEdge and Fastly
  Compute.
