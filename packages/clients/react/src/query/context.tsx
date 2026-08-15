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
import { DEFAULT_METHODS, defaultClassify } from './endpoint'
import { openQueryCollection } from './queries'
import type {
  DrainOptions,
  PendingWrite,
  QueryRecord,
  ResolvedBackend,
  WriteOutcome,
} from './types'

export interface QueryContextValue {
  /** The `taladb_queries` collection, or `null` until it has opened. */
  queries: Collection<QueryRecord> | null
  backend: ResolvedBackend
  drain: Required<Pick<DrainOptions, 'policy'>> & DrainOptions
  /** Declare a collection as managed, so the drain looks at it. */
  register: (collection: string) => void
  /** Collections currently managed. Stable identity until one is added. */
  collections: string[]
  /** A drain cycle is in flight. */
  draining: boolean
  /** Ask for a drain cycle soon. `force` is reserved for an explicit retry. */
  requestDrain: (force?: boolean) => void
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
  /**
   * Headers for every outbound write, resolved **at send time**.
   *
   * That timing is the reason this lives here rather than on the hook: a write
   * queued while offline and drained an hour later must carry a current token,
   * not the one it was made with.
   */
  headers?: () => HeadersInit | Promise<HeadersInit>
  /**
   * Map a response onto what the queue should do. Optional — by default `2xx`
   * succeeds, `409` is already-applied, `5xx`/`408`/`429` retry, and any other
   * `4xx` is terminal.
   */
  classify?: (response: Response, op: PendingWrite) => WriteOutcome
  /** Override the fetch implementation (tests, instrumentation). */
  fetch?: typeof globalThis.fetch
  drain?: DrainOptions
}

/**
 * Provides the query layer's shared state beneath a `<TalaDBProvider>`.
 *
 * Nested rather than merged into `TalaDBProvider` so an application using only
 * the local hooks never opens the query-record collection at all.
 */
export function QueryProvider({
  children,
  headers,
  classify,
  fetch,
  drain,
}: QueryProviderProps) {
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

  // Assembled once from the three policy props. Routing is not here — it lives
  // on each `useMutation`, stamped onto the write so it survives the component.
  const backend = useMemo<ResolvedBackend>(
    () => ({
      url: () => '',
      method: DEFAULT_METHODS,
      headers,
      classify: classify ?? defaultClassify,
      fetch,
    }),
    [headers, classify, fetch],
  )

  const policy = drain?.policy ?? 'auto'
  const intervalMs = drain?.intervalMs
  const batch = drain?.batch

  // Which collections the drain should look at. A ref, not state: registering
  // is a side effect of rendering a hook and must not itself cause a render.
  const managedRef = useRef<Set<string>>(new Set())
  const [collections, setCollections] = useState<string[]>([])
  const register = useCallback((collection: string) => {
    if (managedRef.current.has(collection)) return
    managedRef.current.add(collection)
    // Only on a genuinely new collection, so re-registering on every render
    // cannot thrash subscribers of `collections`.
    setCollections([...managedRef.current])
  }, [])

  const runningRef = useRef(false)
  const [draining, setDraining] = useState(false)
  const runDrain = useCallback(async () => {
    if (runningRef.current) return
    if (managedRef.current.size === 0) return
    runningRef.current = true
    setDraining(true)
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
      setDraining(false)
    }
  }, [db, backend, batch])

  const requestDrain = useCallback((force = false) => {
    if (!force && policy === 'manual') return
    if (!force && policy === 'interval') return
    if (
      !force &&
      policy === 'online-only' &&
      typeof navigator !== 'undefined' &&
      !navigator.onLine
    ) {
      return
    }
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
      backend,
      drain: { policy, intervalMs, batch },
      register,
      collections,
      draining,
      requestDrain,
    }),
    // Spread by field: an inline `drain={{…}}` object would otherwise produce a
    // new context value on every render and re-run every consumer's effects.
    [queries, backend, policy, intervalMs, batch, register, collections, draining, requestDrain],
  )

  return <QueryContext.Provider value={value}>{children}</QueryContext.Provider>
}
