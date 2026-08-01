---
title: Next.js
description: Local-first Next.js apps with TalaDB — on-device reads and writes in client components, live queries via @taladb/react, and your API routes as the sync backend via @taladb/next.
---

# Next.js

TalaDB turns a Next.js app local-first: **reads and writes hit an on-device database** (WASM + OPFS, in a worker) from client components — instant, offline-capable, no loading spinners for your own data — while a background loop syncs to your API routes.

One mental model before anything else: **server components can never render a user's TalaDB data.** The data lives in *that user's browser*; the server doesn't have it. The division of labor is:

| Where | Role |
|---|---|
| Client components | All TalaDB reads/writes, live queries, the sync loop |
| Server components | Layout, shell, anything not user-data-dependent |
| Route handlers | The sync backend (`@taladb/next/server`) — your security boundary |

A complete working app ships in the repo: [`examples/nextjs-sync`](https://github.com/taladb/taladb/tree/main/examples/nextjs-sync) — CI runs a real `next build` over it on every commit.

## Install

```bash
pnpm add taladb @taladb/web @taladb/react @taladb/next
```

| Package | Why |
|---|---|
| `taladb` | The unified API (`openDB`, `HttpSyncAdapter`) |
| `@taladb/web` | The WASM engine + OPFS worker |
| `@taladb/react` | `TalaDBProvider`, `useFind` / `useFindOne` / `useCollection` hooks |
| `@taladb/next` | Optional but recommended: sync route handlers + `<SyncProvider>` |

No webpack config is needed — the browser bundle is webpack-clean and the worker is bundled via the standard `new URL(..., import.meta.url)` asset pattern.

## Providers

`<TalaDBProvider name>` owns the client-only `openDB()`: during SSR (and while the database opens) it renders `fallback`; children mount only with a ready database, so hooks never observe a missing instance. `@taladb/react` ships the `'use client'` directive, so these imports are safe anywhere.

```tsx
// app/providers.tsx
'use client'
import { TalaDBProvider } from '@taladb/react'
import { SyncProvider } from '@taladb/next/client'

export function Providers({ children }: { children: React.ReactNode }) {
  return (
    <TalaDBProvider name="myapp.db" fallback={<p>opening…</p>}>
      <SyncProvider
        endpoint="/api/sync"
        headers={() => ({ Authorization: `Bearer ${getToken()}` })}
      >
        {children}
      </SyncProvider>
    </TalaDBProvider>
  )
}
```

```tsx
// app/layout.tsx — a server component; the providers handle the client boundary
import { Providers } from './providers'

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body><Providers>{children}</Providers></body>
    </html>
  )
}
```

`<SyncProvider>` drives `db.sync()` on mount, every 30 s (configurable via `intervalMs`), on reconnect, and on tab focus — entirely off the main thread, since the whole sync pass runs inside TalaDB's worker. Skip it if you want to [wire the cadence yourself](/guide/bidirectional-sync#react-browser-spa).

## Using the hooks

Pages that touch data are client components. Live queries mean synced changes appear without any refetching code:

```tsx
// app/page.tsx
'use client'
import { useCollection, useFind } from '@taladb/react'

interface Note {
  _id?: string
  text: string
  createdAt: number
  [key: string]: string | number | undefined
}

export default function Home() {
  const notes = useCollection<Note>('notes')
  const { data, loading } = useFind(notes) // re-renders on local writes AND synced pulls

  return loading ? <p>loading…</p> : (
    <ul>{data.map((n) => <li key={n._id}>{n.text}</li>)}</ul>
  )
}
```

During prerender the hooks return `{ data: [], loading: true }` (via `getServerSnapshot`), then hydrate into live data — no hydration mismatch.

## The sync backend

One route file is a complete server:

```ts
// app/api/sync/[[...action]]/route.ts
import { createSyncHandlers, memorySyncStore } from '@taladb/next/server'

export const { POST, GET } = createSyncHandlers({
  store: memorySyncStore(), // dev only — see below for production stores
  authorize: async (req) => {
    // Your security boundary: return a scope key (user id) or null → 401.
    return verifySession(req.headers.get('authorization'))
  },
})
```

`authorize` partitions the change store per user (or per workspace) — a caller can never pull another scope's changes. Production stores:

- **`taladbSyncStore(await openDB('hub.db'))`** — a server-side TalaDB as the change hub. Requires the Node.js runtime (`export const runtime = 'nodejs'`) and a persistent filesystem — a VPS/container, not serverless-ephemeral.
- **[`@taladb/sync-mongodb`](/guide/bidirectional-sync#mongodb-adapter)** — MongoDB as the hub; fits serverless deployments.
- **Your own [`SyncStore`](/guide/bidirectional-sync#your-server-two-endpoints)** — two methods over any database.

## Deployment notes

- **Route handlers run on the Node.js runtime by default** — keep it that way for `taladbSyncStore` and the MongoDB adapter (both need Node APIs). `memorySyncStore` also works on edge, but its state is per-instance.
- **OPFS persistence is per-origin and per-browser-profile.** Users get their data on the device they wrote it; sync is what moves it across devices.
- **Serverless cold starts**: the client's local database is unaffected — the app keeps working; a failed sync pass simply retries (cursors advance only on success).

## Relationship to the other guides

- [Web guide](/guide/web) — the underlying browser engine (OPFS, workers, Vite setup for non-Next apps)
- [React hooks guide](/guide/react) — the full hooks API
- [Bidirectional sync guide](/guide/bidirectional-sync) — how sync works (cursors, LWW, idempotency), the wire contract, server-to-server
