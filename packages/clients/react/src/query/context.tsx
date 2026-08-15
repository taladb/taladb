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
  QueryDefaults,
  QueryKey,
  QueryRecord,
  ResolvedBackend,
  WriteOutcome,
} from './types'

export interface QueryContextValue {
  /** The `taladb_queries` collection, or `null` until it has opened. */
  queries: Collection<QueryRecord> | null
  backend: ResolvedBackend
  drain: Required<Pick<DrainOptions, 'policy'>> & DrainOptions
  /** Maps a query key to a collection name. See `QueryProviderProps`. */
  resolveCollection?: (queryKey: QueryKey) => string | undefined
  /** Provider-wide query defaults. Each hook option overrides its entry. */
  defaults: QueryDefaults['queries']
  /** Declare a collection as managed, so the drain looks at it. */
  register: (collection: string) => void
  /** Collections currently managed. Stable identity until one is added. */
  collections: string[]
  /** A drain cycle is in flight. */
  draining: boolean
  /** Ask for a drain cycle soon. `force` is reserved for an explicit retry. */
  requestDrain: (force?: boolean) => void
  /**
   * Record a write that is deliberately *not* asking to be sent yet — a
   * `queued` mutation.
   *
   * Separate from `requestDrain` because the difference between `queued` and
   * `optimistic` is exactly whether the write asks for a cycle. It still has to
   * be counted, or a backlog building under a long interval would never trip
   * the threshold.
   */
  noteWrite: () => void
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
  /**
   * Work out which collection a query key's documents belong in.
   *
   * `useQuery` infers the collection from `queryKey[0]`, which covers the usual
   * `['todos', …]` convention. Codebases whose keys do not lead with the
   * collection — `['api', 'v2', 'todos']`, or keys built by a helper — set this
   * once here rather than passing `collection` at every call site.
   *
   * Returning `undefined` falls back to the default inference. The result is
   * still checked against the registered collections.
   */
  resolveCollection?: (queryKey: QueryKey) => string | undefined
  /**
   * Defaults for every `useQuery` below this provider, in TanStack's
   * `new QueryClient({ defaultOptions })` shape so existing config moves across
   * unchanged.
   *
   * ```tsx
   * <QueryProvider defaultOptions={{ queries: { staleTime: 0, retry: false } }}>
   * ```
   *
   * `staleTime: 0` here is the one-line way back to TanStack's
   * revalidate-on-every-mount behaviour.
   */
  defaultOptions?: QueryDefaults
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
  resolveCollection,
  defaultOptions,
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

  // Spread by field for the same reason the context value is: an inline
  // `defaultOptions={{…}}` would otherwise be a new object every render.
  const defaults = defaultOptions?.queries
  const policy = drain?.policy ?? 'auto'
  const intervalMs = drain?.intervalMs
  const batch = drain?.batch
  const flushAt = drain?.flushAt ?? 100
  const flushOnHide = drain?.flushOnHide ?? true

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
  // Writes committed since the last cycle. Counted rather than queried, so the
  // threshold costs nothing on a path that runs on every write.
  const queuedSinceDrainRef = useRef(0)
  const [draining, setDraining] = useState(false)
  const runDrain = useCallback(async (keepalive = false) => {
    if (runningRef.current) return
    if (managedRef.current.size === 0) return
    runningRef.current = true
    queuedSinceDrainRef.current = 0
    setDraining(true)
    try {
      await drainOnce({
        db,
        backend,
        collections: () => [...managedRef.current],
        batch,
        keepalive,
      })
    } catch (error: unknown) {
      console.error('[@taladb/react/query] Drain cycle failed.', error)
    } finally {
      runningRef.current = false
      setDraining(false)
    }
  }, [db, backend, batch])

  const requestDrain = useCallback(
    (force = false) => {
      // Counted even when this cycle is declined: the point of the threshold is
      // to notice a backlog building under a policy that is not sending.
      queuedSinceDrainRef.current += 1
      const overThreshold = flushAt !== false && queuedSinceDrainRef.current >= flushAt

      if (!force && !overThreshold) {
        if (policy === 'manual') return
        if (policy === 'interval') return
        if (
          policy === 'online-only' &&
          typeof navigator !== 'undefined' &&
          !navigator.onLine
        ) {
          return
        }
      }
      // A threshold flush still respects being offline — there is nowhere to
      // send it, and `manual` is the one policy that means what it says.
      if (overThreshold && !force && policy === 'manual') return
      void runDrain()
    },
    [policy, flushAt, runDrain],
  )

  const noteWrite = useCallback(() => {
    queuedSinceDrainRef.current += 1
    if (flushAt === false || queuedSinceDrainRef.current < flushAt) return
    if (policy === 'manual') return
    if (typeof navigator !== 'undefined' && navigator.onLine === false) return
    void runDrain()
  }, [flushAt, policy, runDrain])

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

  // Hiding and closing.
  //
  // `visibilitychange` is where the work actually happens: the page is still
  // alive, so a normal drain completes. `pagehide` is the last-ditch attempt,
  // and it has to be sent with `keepalive` because an ordinary request is
  // cancelled the moment the document goes away.
  useEffect(() => {
    if (!flushOnHide || policy === 'manual' || typeof window === 'undefined') return

    const onHidden = () => {
      if (typeof document !== 'undefined' && document.visibilityState !== 'hidden') return
      void runDrain()
    }
    const onPageHide = () => void runDrain(true)

    document.addEventListener('visibilitychange', onHidden)
    window.addEventListener('pagehide', onPageHide)
    return () => {
      document.removeEventListener('visibilitychange', onHidden)
      window.removeEventListener('pagehide', onPageHide)
    }
  }, [flushOnHide, policy, runDrain])

  const value = useMemo<QueryContextValue>(
    () => ({
      queries,
      backend,
      drain: { policy, intervalMs, batch, flushAt, flushOnHide },
      resolveCollection,
      defaults,
      register,
      collections,
      draining,
      requestDrain,
      noteWrite,
    }),
    // Spread by field: an inline `drain={{…}}` object would otherwise produce a
    // new context value on every render and re-run every consumer's effects.
    [
      queries,
      backend,
      policy,
      intervalMs,
      batch,
      flushAt,
      flushOnHide,
      resolveCollection,
      defaults,
      register,
      collections,
      draining,
      requestDrain,
      noteWrite,
    ],
  )

  return <QueryContext.Provider value={value}>{children}</QueryContext.Provider>
}
