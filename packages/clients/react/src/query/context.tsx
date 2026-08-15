import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import type { Collection } from 'taladb'
import { useTalaDB } from '../context'
import { drainOnce } from './drain'
import { openQueryCollection } from './queries'
import type { DrainOptions, QueryRecord, ResolvedBackend } from './types'

export interface QueryContextValue {
  /** The `taladb_queries` collection, or `null` until it has opened. */
  queries: Collection<QueryRecord> | null
  backend: ResolvedBackend | null
  drain: Required<Pick<DrainOptions, 'policy'>> & DrainOptions
  /** Declare a collection as managed, so the drain looks at it. */
  register: (collection: string) => void
  /** Ask for a drain cycle soon. A no-op under the `manual` policy. */
  requestDrain: () => void
}

const QueryContext = createContext<QueryContextValue | null>(null)

/**
 * Read the query layer's context.
 *
 * Throws rather than returning a default, because every plausible default is
 * wrong: silently doing nothing would make a missing provider look like an
 * empty result set.
 */
export function useQueryContext(): QueryContextValue {
  const value = useContext(QueryContext)
  if (value === null) {
    throw new Error(
      'A @taladb/react/query hook was used outside <QueryProvider>. Wrap the tree in ' +
        '<TalaDBProvider><QueryProvider …>…</QueryProvider></TalaDBProvider>.',
    )
  }
  return value
}

export interface QueryProviderProps {
  children: ReactNode
  /** Where writes go. Required once mutations are in play; unused by reads. */
  backend?: ResolvedBackend
  drain?: DrainOptions
}

/**
 * Provides the query layer's shared state beneath a `<TalaDBProvider>`.
 *
 * Nested rather than merged into `TalaDBProvider` so an application using only
 * the local hooks never opens the query-record collection at all.
 */
export function QueryProvider({ children, backend, drain }: QueryProviderProps) {
  const db = useTalaDB()
  const [queries, setQueries] = useState<Collection<QueryRecord> | null>(null)

  // Opening is async; a component that unmounts mid-open must not set state.
  const liveRef = useRef(true)
  useEffect(() => {
    liveRef.current = true
    let cancelled = false
    void openQueryCollection(db)
      .then((collection) => {
        if (!cancelled) setQueries(collection)
      })
      .catch((error: unknown) => {
        console.error('[@taladb/react/query] Failed to open the query-record collection.', error)
      })
    return () => {
      cancelled = true
      liveRef.current = false
    }
  }, [db])

  const policy = drain?.policy ?? 'immediate'
  const intervalMs = drain?.intervalMs
  const batch = drain?.batch

  // Which collections the drain should look at. A ref, not state: registering
  // is a side effect of rendering a hook and must not itself cause a render.
  const managedRef = useRef<Set<string>>(new Set())
  const register = useCallback((collection: string) => {
    managedRef.current.add(collection)
  }, [])

  const runningRef = useRef(false)
  const runDrain = useCallback(async () => {
    if (backend === undefined || runningRef.current) return
    if (managedRef.current.size === 0) return
    runningRef.current = true
    try {
      await drainOnce({
        db,
        backend,
        collections: () => [...managedRef.current],
        batch,
      })
    } catch (error: unknown) {
      console.error('[@taladb/react/query] Drain cycle failed.', error)
    } finally {
      runningRef.current = false
    }
  }, [db, backend, batch])

  const requestDrain = useCallback(() => {
    if (policy === 'manual') return
    void runDrain()
  }, [policy, runDrain])

  // Reconnecting is the moment a queue most needs draining, whatever the policy
  // (bar `manual`) — a backlog built offline is exactly what is waiting.
  useEffect(() => {
    if (policy === 'manual' || typeof window === 'undefined') return
    const onOnline = () => void runDrain()
    window.addEventListener('online', onOnline)
    return () => window.removeEventListener('online', onOnline)
  }, [policy, runDrain])

  useEffect(() => {
    if (policy !== 'interval') return
    const id = setInterval(() => void runDrain(), intervalMs ?? 30_000)
    return () => clearInterval(id)
  }, [policy, intervalMs, runDrain])

  const value = useMemo<QueryContextValue>(
    () => ({
      queries,
      backend: backend ?? null,
      drain: { policy, intervalMs, batch },
      register,
      requestDrain,
    }),
    // Spread by field: an inline `drain={{…}}` object would otherwise produce a
    // new context value on every render and re-run every consumer's effects.
    [queries, backend, policy, intervalMs, batch, register, requestDrain],
  )

  return <QueryContext.Provider value={value}>{children}</QueryContext.Provider>
}
