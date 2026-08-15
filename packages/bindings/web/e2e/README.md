# Browser end-to-end tests

Drives a real Chrome against the built `@taladb/web` WASM and the `taladb`
client, over OPFS, through the worker — the stack an application actually runs.

```bash
pnpm --filter @taladb/web test:e2e         # everything
pnpm --filter @taladb/web test:e2e 3       # one phase
VERBOSE=1 pnpm --filter @taladb/web test:e2e 3   # with browser console output
```

Build first — the suite loads `pkg/` and the client's `dist/`, not the sources:

```bash
pnpm --filter @taladb/web build && pnpm --filter taladb build
```

## Why this exists alongside the unit tests

The Node tests mock the browser. Everything below is invisible to them, and each
line is a bug this suite caught after the unit tests were green:

| Phase | Covers |
|---|---|
| `phase1-happy` | CRUD, indexes, aggregation, BM25, vector search, live queries, OPFS persistence across reload, and the three collection shapes (document-only, vector-only, mixed) |
| `phase2-edge` | Empty collections, value round-tripping, null vs missing, caller-supplied `_id`, sort ties and paging, malformed queries, vector-index edges, use-after-close |
| `phase3-multitab` | Two and three tabs on one database: the OPFS lock, the IndexedDB-snapshot fallback, cross-tab write forwarding, live queries across tabs, **lock takeover when the owning tab closes**, and the write/close race |
| `phase4-webhook` | One request per committed mutation, verb per operation, payload shape, ordering, retries, queue overflow, drain on close, and exactly-once across tabs |
| `phase5-perf` | Open latency, bulk insert, indexed vs unindexed reads, paged sort, aggregation, vector search, and webhook overhead — printed, and asserted only against loose ceilings |

Multi-tab is the part with no other coverage. A dedicated worker per tab, one
OPFS lock between them, and a snapshot in IndexedDB for everyone else is a design
whose failure modes are all silent: writes that land in memory and are never
persisted, live queries that stop firing, a tab that keeps accepting writes after
the tab owning the file has gone. None of it can be reproduced without two real
tabs sharing real origin storage.

## How it is wired

`server.mjs` serves the built artefacts at the paths the client's
`new URL('@taladb/web/pkg/…', import.meta.url)` resolution produces, so no
bundler is involved. It also *is* the webhook receiver: `/hook` records every
request, and `/ctl/*` lets a test inject 500s, 4xx, or latency.

`lib.mjs` starts that server, launches Chrome, and provides the assertion runner.
Tests run in the page via `page.evaluate`, so they are written against the public
`taladb` API exactly as an application would use it.

Failures print the expected and actual value and the suite continues, so one run
reports every problem rather than the first.
