# Parity suite

This suite is the acceptance test for `PLAN-query-parity.md`. It was written
first, to fail; as of P4 it passes in full — 15 behaviour tests and a clean
typecheck. It stays in its own config, out of the normal `test` and `typecheck`
runs, because that is what let it be honest while it was red, and because P5–P7
will make it red again.

```bash
pnpm test:parity        # behaviour: does migrated code run?
pnpm typecheck:parity   # types: does migrated code compile?
```

## What it asserts

The claim in §0 of the plan is that migrating off TanStack Query should be one
line:

```diff
- import { useQuery } from '@tanstack/react-query'
+ import { useQuery } from '@taladb/react/query'
```

So the fixtures in `fixtures/` are written the way TanStack v5 code is actually
written — `isPending` guards, `error.message`, `data.map(…)`, `queryKey`
destructured inside `queryFn`, `placeholderData: keepPreviousData` — and they
import from `@taladb/react/query`. Nothing in them is adapted to our surface. If
they compile and run, the claim holds for the shapes they cover.

The path alias that makes `@taladb/react/query` resolve to `src/query/index.ts`
lives in `vitest.parity.config.ts` and `tsconfig.parity.json`. It exists so the
fixtures can read like real application code instead of reaching through
`../../../src`.

| Fixture | Covers |
|---|---|
| `TodoList.tsx` | The default read: no `collection`, no `staleTime`, `isPending`/`isError`/`isFetching`, `refetch` |
| `TodoDetail.tsx` | Single-document `queryFn`, `enabled`, `staleTime`, `select`, `queryKey` + `signal` in the fetch context |
| `TodoPage.tsx` | `placeholderData: keepPreviousData`, `isPlaceholderData`, `retry`, `refetchOnWindowFocus: false`, an object segment in the key |

`surface.parity.test.tsx` additionally pins the result object's key set against
TanStack v5's, and the two behaviours most likely to break silently: `data`
being `undefined` (not `[]`) while pending, and `error` being an `Error`.

## Reading a failure

A failure here is a to-do, not a regression. The useful signal is *which* of the
three categories it falls into:

1. **Missing surface** — a key or option does not exist yet. Mechanical; P1.
2. **Wrong semantics** — it exists and does the wrong thing. `isStale` meaning
   "revalidating" rather than "past `staleTime`" is the one to watch; P2.
3. **Genuinely impossible** — the residue. Only one is known: a `queryFn` that
   returns something other than documents (plan §3.3). That case must fail with
   an error naming the collection, the key, and the `documents`/`assemble`
   options, and `surface.parity.test.tsx` asserts the message rather than
   pretending the case works.
4. **The fixture is wrong** — rare, and the only case where editing a fixture is
   the right fix. It has happened once: `TodoPage` called `data.map` after
   guarding `isPending` alone, which does not compile against TanStack either,
   because the loading-error member survives that guard and carries
   `data: undefined`. The bar for this category is high — the fixture must be
   demonstrably invalid *as TanStack code*, not merely inconvenient for us.

## What this suite does not cover

Provider setup is explicitly *not* a one-line migration (plan §5.0): there is no
`QueryClient` and no `QueryClientProvider`, so every fixture is rendered under
`<TalaDBProvider><QueryProvider>` by the test wrapper. That swap is the one edit
a migrating user is expected to make by hand, so asserting it away here would be
testing a promise the plan does not make.
