# Local-first storefront (Vite SPA)

A React SPA demonstrating `@taladb/react/query`: fetched pages are hydrated into
a real TalaDB collection, survive reloads, update through live queries, and stay
available offline.

```bash
pnpm install
pnpm --filter example-vite-spa-storefront api
pnpm --filter example-vite-spa-storefront dev
```

Open a few pages and filters, then revisit them. Within the five-minute TTL the
origin request counter does not move because the query records and documents are
read from TalaDB. Switch DevTools to Offline and previously visited pages still
work; an uncached page correctly reports that the origin is unavailable.

The server is intentionally an ordinary paginated REST endpoint. The client maps
each origin key to a stable ULID with `deriveDocId`, then `useQuery` stores the
canonical rows and ordered result membership locally. See the
[local-first query guide](../../docs/guide/query.md).
