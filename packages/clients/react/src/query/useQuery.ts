import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { Document, Value } from 'taladb'
import { useCollectionOptions } from '../context'
import { useCollection } from '../useCollection'
import { useFind } from '../useFind'
import { resolveCollectionName } from './collection'
import { useQueryContext } from './context'
import { hydrate } from './hydrate'
import { hashQueryKey } from './keys'
import { noteIgnoredOptions, warnOnce } from './dev'
import {
  deleteQueryRecord,
  ensureEnvelopeIndexes,
  readQueryRecord,
  writeQueryRecord,
} from './queries'
import type { Enveloped } from './envelope'
import { extractDocuments } from './shape'
import { cancelShared, inflightKey, runShared } from './inflight'
import { normalizeParams } from './params'
import { isOffline, retryDelayMs, shouldRetry, sleep } from './retry'
import { replaceEqualDeep } from './structural'
import type {
  AnyQueryOptions,
  Doc,
  QueryResult,
  QueryResultCommon,
  RefetchTrigger,
  ResultShape,
  UseDocumentQueryOptions,
  UseEnvelopeQueryOptions,
  UseQueryOptions,
} from './types'

/** Matches nothing, for a query whose result set is empty or not yet known. */
const MATCH_NONE = { _id: { $in: [] as string[] } }

/**
 * One hour.
 *
 * TanStack defaults `staleTime` to `0`, which revalidates on every mount. That
 * is right for a cache that dies on reload — the data is usually seconds old
 * and the request is usually free — and wrong for one that survives reload and
 * is expected to work offline, where it turns every mount into a network hit
 * against data the device already holds.
 */
export const DEFAULT_STALE_TIME = 60 * 60_000

/** Everything the hook tracks that is not derived from the database. */
interface FetchState {
  /** `null` until the record has been read or a fetch has resolved. */
  ids: string[] | null
  /** How the response mapped onto documents — decides what `data` looks like. */
  shape: ResultShape
  /** The raw envelope response, when `assemble` needs it back. */
  payload: Value | undefined
  error: Error | null
  fetchStatus: 'fetching' | 'paused' | 'idle'
  dataUpdatedAt: number
  errorUpdatedAt: number
  failureCount: number
  failureReason: Error | null
  errorUpdateCount: number
  isFetched: boolean
  isFetchedAfterMount: boolean
  /** Answered from the whole collection because the network could not answer. */
  fromCollection: boolean
}

const INITIAL: FetchState = {
  ids: null,
  shape: 'array',
  payload: undefined,
  error: null,
  fetchStatus: 'idle',
  dataUpdatedAt: 0,
  errorUpdatedAt: 0,
  failureCount: 0,
  failureReason: null,
  errorUpdateCount: 0,
  isFetched: false,
  isFetchedAfterMount: false,
  fromCollection: false,
}

/**
 * Whether a rejection is an abandoned request rather than a failure.
 *
 * `AbortController` rejects with a `DOMException` named `AbortError`; a plain
 * `Error` with that name covers environments without `DOMException`.
 */
function isAbort(cause: unknown): boolean {
  return cause instanceof Error && cause.name === 'AbortError'
}

/** Non-`Error` throws are wrapped, so `error.message` always works. */
function toError(cause: unknown): Error {
  return cause instanceof Error ? cause : new Error(String(cause))
}

/**
 * Pass this as `placeholderData` to hold the previous key's data on screen
 * while the next one loads.
 *
 * v5 replaced the old `keepPreviousData` *option* with exactly this identity
 * function, so it is exported under the same name a migrating codebase already
 * imports.
 */
export function keepPreviousData<T>(previous: T | undefined): T | undefined {
  return previous
}

/** Resolve `placeholderData`, whichever form it took. */
function resolvePlaceholder(options: AnyQueryOptions, previous: unknown): unknown {
  const placeholder = options.placeholderData
  if (placeholder === undefined) return undefined
  return typeof placeholder === 'function'
    ? (placeholder as (previous: unknown) => unknown)(previous)
    : placeholder
}

/**
 * Assemble the status-specific half of the result.
 *
 * Written as one function returning the union so the compiler checks each
 * member against its declared literal flags — spread inline, an inconsistent
 * pair like `isError: true` with `status: 'success'` would type-check happily.
 */
function buildResult(
  common: QueryResultCommon<unknown>,
  data: unknown,
  error: Error | null,
): QueryResult<unknown> {
  if (error !== null) {
    return data === undefined
      ? {
          ...common,
          status: 'error',
          data: undefined,
          error,
          isPending: false,
          isSuccess: false,
          isError: true,
          isLoadingError: true,
          isRefetchError: false,
        }
      : {
          ...common,
          status: 'error',
          data,
          error,
          isPending: false,
          isSuccess: false,
          isError: true,
          isLoadingError: false,
          isRefetchError: true,
        }
  }
  if (data === undefined) {
    return {
      ...common,
      status: 'pending',
      data: undefined,
      error: null,
      isPending: true,
      isSuccess: false,
      isError: false,
      isLoadingError: false,
      isRefetchError: false,
    }
  }
  return {
    ...common,
    status: 'success',
    data,
    error: null,
    isPending: false,
    isSuccess: true,
    isError: false,
    isLoadingError: false,
    isRefetchError: false,
  }
}

/**
 * Read a slice of a collection, hydrated from a server and revalidated in the
 * background.
 *
 * `data` is resolved through the local database rather than from the fetch
 * response, so a local write to any document in the set re-renders this query
 * immediately — there is no cache to invalidate, because the collection *is*
 * the state. Documents arrive in the order the server returned them.
 *
 * On a cold key the hook fetches and reports `pending`. On a warm key it
 * renders what it has straight away and revalidates behind that only once
 * `staleTime` has elapsed — a reload never shows a spinner over data the device
 * already holds.
 */
// A list of documents — `data` is `T[]`, or whatever `select` returns.
export function useQuery<T extends Doc, TData = T[]>(
  options: UseQueryOptions<T, TData>,
): QueryResult<TData>
// One document — `undefined` once it is deleted locally, so `select`'s output
// carries the same gap.
export function useQuery<T extends Doc, TData = T>(
  options: UseDocumentQueryOptions<T, TData>,
): QueryResult<TData | undefined>
// An envelope — `data` is whatever `assemble` builds, then `select` transforms.
export function useQuery<TRaw, TDoc extends Doc, TAssembled = TDoc[], TData = TAssembled>(
  options: UseEnvelopeQueryOptions<TRaw, TDoc, TAssembled, TData>,
): QueryResult<TData>
export function useQuery(options: AnyQueryOptions): QueryResult<unknown> {
  const { queryKey, meta } = options
  const enabled = typeof options.enabled === 'function' ? options.enabled() : (options.enabled ?? true)
  const { queries, register, resolveCollection, defaults } = useQueryContext()

  // Hook option, then provider default, then the built-in. Resolved in one
  // place so no call site has to remember the precedence.
  const staleTime = options.staleTime ?? defaults?.staleTime ?? DEFAULT_STALE_TIME
  const retry = options.retry ?? defaults?.retry
  const retryDelay = options.retryDelay ?? defaults?.retryDelay
  const networkMode = options.networkMode ?? defaults?.networkMode
  const refetchOnMount = options.refetchOnMount ?? defaults?.refetchOnMount
  const refetchOnWindowFocus = options.refetchOnWindowFocus ?? defaults?.refetchOnWindowFocus
  const refetchOnReconnect = options.refetchOnReconnect ?? defaults?.refetchOnReconnect
  const refetchIntervalOption = options.refetchInterval ?? defaults?.refetchInterval
  const refetchInterval =
    typeof refetchIntervalOption === 'function' ? refetchIntervalOption() : refetchIntervalOption
  const refetchIntervalInBackground =
    options.refetchIntervalInBackground ?? defaults?.refetchIntervalInBackground ?? false
  const throwOnError = options.throwOnError ?? defaults?.throwOnError ?? false
  const forgetAfter = options.forgetAfter ?? defaults?.forgetAfter ?? Number.POSITIVE_INFINITY
  const hashFn = options.queryKeyHashFn ?? hashQueryKey

  noteIgnoredOptions(options as unknown as Record<string, unknown>)
  const registry = useCollectionOptions()

  // Bound once per render so `queryFn` and the local filter see the same thing,
  // whichever form the caller passed.
  const params = useMemo(
    () => normalizeParams(options.params),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [JSON.stringify(options.params === undefined ? null : (options.params as { values?: unknown }).values ?? options.params)],
  )

  // Parameters are part of the query's identity: different parameters are a
  // different request and a different result set, so they belong in the key
  // rather than beside it.
  const effectiveKey = useMemo(
    () => (params === undefined ? queryKey : [...queryKey, params.values]),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [hashFn(queryKey), params],
  )

  // Serialised, so an inline key literal does not restart the effect each render.
  const keyHash = hashFn(effectiveKey)

  // Throws during render when the collection cannot be established, which is
  // deliberate: every alternative silently writes documents somewhere wrong.
  const name = useMemo(
    () =>
      resolveCollectionName({
        collection: options.collection,
        // The caller's key, not the parameter-extended one: the collection is
        // named by what they wrote, and appending values must not change it.
        queryKey,
        resolve: resolveCollection,
        registered: registry.names(),
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [options.collection, keyHash, resolveCollection, registry],
  )

  // The engine's own constraint, applied at the boundary where documents are
  // actually stored. The public generic is looser on purpose — see `Doc`.
  const collection = useCollection<Document>(name)

  // Starts `fetching` for an enabled query, before any effect has run.
  //
  // TanStack can report `isLoading` on the very first render because its cache
  // is in memory and answers synchronously. Ours is a database: the record read
  // is asynchronous, so there is a window where the query is pending but has
  // not started fetching — and `isLoading = isPending && isFetching` would be
  // false through it, flickering any `if (isLoading)` spinner in migrated code.
  // Treating the initial cache read as part of the load closes that window; the
  // effect drops back to `idle` on every path that does not go to the network.
  const [state, setState] = useState<FetchState>(() => ({
    ...INITIAL,
    fetchStatus: enabled ? 'fetching' : 'idle',
  }))
  const offline = options.offline ?? 'strict'

  // Reset when the key changes, during render rather than in an effect.
  //
  // The hook instance survives a key change — a paginated list is one `useQuery`
  // whose key moves — so without this the previous key's ids stay in state and
  // render as though they belonged to the new key. Doing it in an effect would
  // paint that wrong answer for one frame first.
  const [renderedKey, setRenderedKey] = useState(keyHash)
  const previousDataRef = useRef<unknown>(undefined)
  if (renderedKey !== keyHash) {
    setRenderedKey(keyHash)
    setState({ ...INITIAL, fetchStatus: enabled ? 'fetching' : 'idle' })
  }

  // Read through refs so `queryFn` may be an inline arrow without re-triggering.
  const queryFnRef = useRef(options.queryFn)
  queryFnRef.current = options.queryFn
  const staleTimeRef = useRef(staleTime)
  staleTimeRef.current = staleTime
  const metaRef = useRef(meta)
  metaRef.current = meta
  const documentsRef = useRef(options.documents)
  documentsRef.current = options.documents
  const assembleRef = useRef(options.assemble)
  assembleRef.current = options.assemble
  const initialDataRef = useRef(options.initialData)
  initialDataRef.current = options.initialData
  const initialDataUpdatedAtRef = useRef(options.initialDataUpdatedAt)
  initialDataUpdatedAtRef.current = options.initialDataUpdatedAt
  const retryRef = useRef(retry)
  retryRef.current = retry
  const retryDelayRef = useRef(retryDelay)
  retryDelayRef.current = retryDelay
  const networkModeRef = useRef(networkMode)
  networkModeRef.current = networkMode
  const enabledRef = useRef(enabled)
  enabledRef.current = enabled
  const hashRef = useRef(hashFn)
  hashRef.current = hashFn
  const forgetAfterRef = useRef(forgetAfter)
  forgetAfterRef.current = forgetAfter
  const paramsRef = useRef(params)
  paramsRef.current = params
  // Mirrors state for the event handlers, which are registered once and would
  // otherwise close over whatever `state` was when they were attached.
  const stateRef = useRef(state)
  stateRef.current = state

  useEffect(() => {
    register(name)
  }, [register, name])

  // Requests started by `refetch` rather than by the mount effect. Tracked so
  // unmounting abandons them too — otherwise a fetch outlives its component
  // with nowhere to deliver.
  const refetchControllersRef = useRef<Set<AbortController>>(new Set())

  // Callers of `refetch` waiting for the render that reflects their fetch.
  //
  // Each remembers the live-query snapshot it was queued against, because the
  // commit that carries the new ids and the commit that carries the new
  // documents are two different renders — the subscription re-delivers
  // asynchronously. Resolving on the first would hand back the new membership
  // with the old rows.
  const resolversRef = useRef<
    Array<{ resolve: (result: QueryResult<unknown>) => void; snapshot: unknown }>
  >([])
  const foundRef = useRef<unknown>(undefined)

  const mountedRef = useRef(true)
  useEffect(() => {
    mountedRef.current = true
    return () => {
      mountedRef.current = false
      for (const controller of refetchControllersRef.current) controller.abort()
      refetchControllersRef.current.clear()
      // Never leave an awaited `refetch()` hanging because the component that
      // owned it went away.
      const waiting = resolversRef.current
      resolversRef.current = []
      for (const { resolve } of waiting) resolve(resultRef.current)
    }
  }, [])

  /** Resolves once the rendered documents reflect the fetch that just landed. */
  const nextResult = useCallback((): Promise<QueryResult<unknown>> => {
    if (!mountedRef.current) return Promise.resolve(resultRef.current)
    return new Promise((resolve) => {
      const entry = { resolve, snapshot: foundRef.current }
      resolversRef.current.push(entry)
      // Safety net. When a refetch returns the same membership the live query
      // never re-delivers, so there is no new snapshot to wait for — and in
      // that case the current result is already the right answer.
      setTimeout(() => {
        const index = resolversRef.current.indexOf(entry)
        if (index === -1) return
        resolversRef.current.splice(index, 1)
        resolve(resultRef.current)
      }, 50)
    })
  }, [])

  // Forget this query's result set once nothing has used it for `forgetAfter`.
  //
  // The timer is module state rather than component state precisely because it
  // has to outlive the component: the point is to expire a key *because* it
  // went unused. Remounting the same key cancels it, so a route the user keeps
  // returning to never expires.
  useEffect(() => {
    cancelForget(name, keyHash)
    return () => {
      const after = forgetAfterRef.current
      if (!Number.isFinite(after) || queries === null) return
      scheduleForget(name, keyHash, after, () => {
        void deleteQueryRecord(queries, name, effectiveKey, hashRef.current).catch(() => {
          // Best effort. A record that outlives its window costs a stale id
          // list, which the next fetch overwrites anyway.
        })
      })
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [queries, name, keyHash])

  const patch = useCallback((next: Partial<FetchState>) => {
    if (mountedRef.current) setState((current) => ({ ...current, ...next }))
  }, [])

  /**
   * Store a response and record what it contained.
   *
   * Shared by the network path and by `initialData`, because seeding is not a
   * different kind of write — it is a fetch whose result the app already had.
   */
  const commit = useCallback(
    async (raw: unknown, fetchedAt: number): Promise<Partial<FetchState>> => {
      const { documents: docs, shape } = extractDocuments({
        raw,
        collection: name,
        queryKey: effectiveKey,
        documents: documentsRef.current as ((raw: unknown) => Doc[]) | undefined,
      })

      await hydrate(collection, docs as unknown as Document[], fetchedAt)
      const nextIds = docs.map((doc) => doc._id as string)
      // The response is kept only when `assemble` will read it back — it
      // duplicates every document it carries, so storing it otherwise is
      // pure weight.
      const payload =
        shape === 'envelope' && assembleRef.current !== undefined ? (raw as Value) : undefined
      await writeQueryRecord(
        queries as never,
        name,
        effectiveKey,
        nextIds,
        fetchedAt,
        staleTimeRef.current,
        shape,
        payload,
        hashRef.current,
      )

      return {
        ids: nextIds,
        shape,
        payload,
        error: null,
        fetchStatus: 'idle',
        dataUpdatedAt: fetchedAt,
        failureCount: 0,
        failureReason: null,
        isFetched: true,
        fromCollection: false,
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [queries, collection, name, keyHash],
  )

  /**
   * Call `queryFn`, retrying on failure.
   *
   * `failureCount` climbs while this runs but `error` does not move: a query
   * that is still retrying has not failed yet, and surfacing an error the hook
   * is about to paper over would flash a failure state that never happened.
   */
  const attemptFetch = useCallback(
    async (signal: AbortSignal): Promise<unknown> => {
      let failures = 0
      for (;;) {
        try {
          return await queryFnRef.current({
            queryKey: effectiveKey,
            signal,
            meta: metaRef.current,
            params: paramsRef.current,
          })
        } catch (cause: unknown) {
          if (signal.aborted) throw cause
          const error = toError(cause)
          failures += 1
          patch({ failureCount: failures, failureReason: error })
          if (!shouldRetry(retryRef.current, failures, error)) throw error
          await sleep(retryDelayMs(retryDelayRef.current, failures - 1, error), signal)
          if (signal.aborted) throw error
        }
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [keyHash, patch],
  )

  const revalidate = useCallback(
    async (signal: AbortSignal): Promise<void> => {
      if (queries === null) return

      // Offline with the default network mode: report it rather than burning
      // the retry budget on requests that cannot succeed. The reconnect
      // listener below is what resumes.
      if (networkModeRef.current !== 'always' && isOffline()) {
        patch({ fetchStatus: 'paused' })
        return
      }
      patch({ fetchStatus: 'fetching' })

      try {
        // One request per key, however many components asked. The shared work is
        // the whole fetch, not just the network call: hydrating the same
        // documents once per waiter is write amplification, and on a secondary
        // tab every one of those writes is forwarded against a bounded buffer.
        const committed = await runShared(
          inflightKey(name, keyHash),
          async (sharedSignal) => {
            const raw = await attemptFetch(sharedSignal)
            // Outside the retry loop on purpose: a response the layer cannot
            // turn into documents will fail identically every time, so retrying
            // it just delays the error by half a minute.
            return commit(raw, Date.now())
          },
          signal,
        )
        if (signal.aborted) return
        patch({ ...committed, isFetchedAfterMount: true })
      } catch (cause: unknown) {
        if (signal.aborted) return
        // An abandoned request is not a failure. It happens when the last
        // interested component unmounts, or when `refetch` supersedes a request
        // already on its way — neither is something a user should see.
        if (isAbort(cause)) {
          patch({ fetchStatus: 'idle' })
          return
        }
        const error = toError(cause)
        // A failed refresh must not clear data the device already holds, so
        // only the error fields move.
        setState((current) => ({
          ...current,
          error,
          fetchStatus: 'idle',
          errorUpdatedAt: Date.now(),
          // `attemptFetch` already counted each network failure. A shape error
          // never reached it, so this floors the count at one.
          failureCount: Math.max(current.failureCount, 1),
          failureReason: error,
          errorUpdateCount: current.errorUpdateCount + 1,
          isFetched: true,
        }))
        throw error
      }
    },
    // `queryKey` is covered by keyHash; including the array itself would defeat it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [queries, collection, name, keyHash, attemptFetch, commit, patch],
  )

  /**
   * Revalidate if this trigger is on and the data warrants it.
   *
   * Shared by mount, focus, reconnect and the poll timer, because they differ
   * only in which flag gates them and all four mean "something happened that
   * makes now a reasonable time to ask again".
   */
  const refetchIfAllowed = useCallback(
    (trigger: RefetchTrigger | undefined) => {
      const setting = trigger ?? true
      if (setting === false || !enabledRef.current) return
      const { dataUpdatedAt } = stateRef.current
      const stale = dataUpdatedAt === 0 || Date.now() >= dataUpdatedAt + staleTimeRef.current
      if (setting !== 'always' && !stale) return

      const controller = new AbortController()
      void revalidate(controller.signal).catch(() => {
        // Recorded in state by `revalidate`; nothing here can act on it.
      })
    },
    [revalidate],
  )

  useEffect(() => {
    if (queries === null) return
    const controller = new AbortController()

    void (async () => {
      try {
        await ensureEnvelopeIndexes(collection)
        const record = await readQueryRecord(queries, name, effectiveKey, hashRef.current)
        if (controller.signal.aborted) return

        if (record !== null) {
          // Warm: render immediately, whether or not this query may fetch. A
          // disabled query still gets its cached data — that is the difference
          // between a persistent cache and a memory one.
          patch({
            ids: record.ids,
            // Restored, not re-derived: a detail view whose response was one
            // document must not come back as a one-element array on reload.
            shape: record.shape ?? 'array',
            payload: record.payload ?? undefined,
            dataUpdatedAt: record.fetchedAt,
            isFetched: true,
          })
          const onMount = refetchOnMount ?? true
          // Judged against *this hook's* `staleTime`, not the `ttl` stored on
          // the record when it was written. Freshness is an opinion held by the
          // reader — two components can legitimately want the same data at
          // different ages — and using the stored value would make a hook that
          // widens `staleTime` refetch anyway, contradicting the `isStale` this
          // same render reports. The record's `ttl` stays written for
          // inspection and for anything that sweeps records without a hook.
          const staleEnough =
            onMount === 'always' ||
            record.fetchedAt === 0 ||
            Date.now() >= record.fetchedAt + staleTimeRef.current
          if (!enabled || onMount === false || !staleEnough) {
            patch({ fetchStatus: 'idle' })
            return
          }
        } else {
          // Cold. `initialData` is persisted before anything else — it is
          // server data the app already had, not a stand-in for it, so it goes
          // through the same path a fetch would and can then be stale in its
          // own right.
          const seed = initialDataRef.current
          if (seed !== undefined) {
            const raw = typeof seed === 'function' ? (seed as () => unknown)() : seed
            const seededAt = initialDataUpdatedAtRef.current ?? Date.now()
            const committed = await commit(raw, seededAt)
            if (controller.signal.aborted) return
            patch(committed)
            if (!enabled || Date.now() < seededAt + staleTimeRef.current) {
              patch({ fetchStatus: 'idle' })
              return
            }
          } else if (!enabled) {
            // Cold and disabled: stay pending with nothing to show, as TanStack
            // does. There is no result yet, and inventing an empty one would
            // read as "the server returned nothing".
            patch({ fetchStatus: 'idle' })
            return
          }
        }

        await revalidate(controller.signal)
      } catch {
        // Cold, and the network could not answer. Under `'collection'` there is
        // still a real question to ask locally: the parameters describe which
        // documents were wanted, and the device may well hold matching ones.
        // This changes what membership *means*, so it is opt-in and reported.
        if (offline === 'collection' && stateRef.current.ids === null) {
          const filter = paramsRef.current?.toFilter()
          if (filter !== undefined && !controller.signal.aborted) {
            const shapeParams = paramsRef.current?.shapeParams() ?? []
            if (shapeParams.length > 0) {
              warnOnce(
                `offline-shape:${name}`,
                `useQuery on '${name}' fell back to the whole collection while offline, but its ` +
                  `parameters include ${shapeParams.join(', ')}, which only the server can ` +
                  'honour. There is no page 2 of a local query — expect more results than the ' +
                  'request would have returned.',
              )
            }
            patch({ fromCollection: true, isFetched: true })
          }
        }
        // `revalidate` has already recorded the failure in state; rethrowing
        // here would surface as an unhandled rejection and nothing more. This
        // also covers a failure in the cache read itself, which has no state of
        // its own to land in.
        patch({ fetchStatus: 'idle' })
      }
    })()

    return () => controller.abort()
    // eslint-disable-next-line react-hooks/exhaustive-deps
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, refetchOnMount, offline, queries, collection, name, keyHash, revalidate, commit, patch])

  // Focus, reconnect and polling.
  //
  // Registered per hook rather than through a shared manager, which is what
  // TanStack does and what this should become if a screen ever holds enough
  // queries for the listener count to matter. Per-hook keeps the staleness
  // check local, which is the part that has to be right.
  useEffect(() => {
    if (typeof window === 'undefined' || refetchOnWindowFocus === false) return
    const onFocus = () => {
      // `visibilitychange` also fires on the way *out*; only a return counts.
      if (typeof document !== 'undefined' && document.visibilityState === 'hidden') return
      refetchIfAllowed(refetchOnWindowFocus)
    }
    window.addEventListener('visibilitychange', onFocus)
    window.addEventListener('focus', onFocus)
    return () => {
      window.removeEventListener('visibilitychange', onFocus)
      window.removeEventListener('focus', onFocus)
    }
  }, [refetchOnWindowFocus, refetchIfAllowed])

  useEffect(() => {
    if (typeof window === 'undefined' || refetchOnReconnect === false) return
    // Reconnecting also clears a `paused` fetch, since `revalidate` re-checks
    // connectivity on the way in.
    const onOnline = () => refetchIfAllowed(refetchOnReconnect)
    window.addEventListener('online', onOnline)
    return () => window.removeEventListener('online', onOnline)
  }, [refetchOnReconnect, refetchIfAllowed])

  useEffect(() => {
    if (!refetchInterval || !enabled) return
    const timer = setInterval(() => {
      if (
        !refetchIntervalInBackground &&
        typeof document !== 'undefined' &&
        document.visibilityState === 'hidden'
      ) {
        return
      }
      // A poll asks unconditionally: an interval that skipped fresh data would
      // never fire at all under the one-hour default.
      refetchIfAllowed('always')
    }, refetchInterval)
    return () => clearInterval(timer)
  }, [refetchInterval, refetchIntervalInBackground, enabled, refetchIfAllowed])

  // Re-render when this query crosses its freshness boundary.
  //
  // `isStale` is computed at render, so without this a query sitting on screen
  // would keep reporting `false` after it had actually gone stale — and any UI
  // keyed off it, a "refresh" affordance most obviously, would never appear.
  const [, forceStaleCheck] = useState(0)
  useEffect(() => {
    if (state.dataUpdatedAt === 0 || !Number.isFinite(staleTime)) return
    const remaining = state.dataUpdatedAt + staleTime - Date.now()
    if (remaining <= 0) return
    const timer = setTimeout(() => forceStaleCheck((n) => n + 1), remaining)
    return () => clearTimeout(timer)
  }, [state.dataUpdatedAt, staleTime])

  // Live view of the documents this query resolved to.
  //
  // `where` narrows *within* that set rather than replacing it: the id list is
  // what the server said belongs to this query, and the predicate is what this
  // component wants to see of it. Composed as `$and` so the engine can still
  // use an index for both halves.
  const ids = state.ids
  const { where } = options
  const whereHash = where === undefined ? '' : JSON.stringify(where)
  const { fromCollection } = state
  const filter = useMemo(() => {
    const clauses: unknown[] = []
    if (fromCollection) {
      // No id list to bound this — that is exactly what `fromCollection` is
      // reporting. The parameter filter is the only thing standing in for
      // membership.
      const params = paramsRef.current?.toFilter()
      if (params !== undefined) clauses.push(params)
    } else {
      if (ids === null || ids.length === 0) return MATCH_NONE
      clauses.push({ _id: { $in: ids } })
      // Redundant against the id list in the ordinary case — the server already
      // applied it — but it is what makes an optimistic write that moves a
      // document out of the filter leave the view immediately, instead of
      // lingering until the next refetch.
      const params = paramsRef.current?.toFilter()
      if (params !== undefined) clauses.push(params)
    }
    if (where !== undefined) clauses.push(where)
    if (clauses.length === 0) return MATCH_NONE
    if (clauses.length === 1) return clauses[0] as Record<string, unknown>
    return { $and: clauses }
    // `where` is covered by its serialisation, so an inline object literal does
    // not resubscribe on every render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ids, whereHash, fromCollection, keyHash])
  const { data: found } = useFind<Document>(collection, filter as never)

  const documents = useMemo<Document[] | undefined>(() => {
    if (fromCollection) {
      // Engine order: there is no server ordering to restore, because no server
      // answered.
      return found.filter((doc) => (doc as Enveloped<Document>)._op !== 'delete')
    }
    if (ids === null) return undefined
    const byId = new Map<string, Document>()
    for (const doc of found) byId.set(doc._id as string, doc)
    const ordered: Document[] = []
    for (const id of ids) {
      const doc = byId.get(id)
      // A tombstone is a delete this device has queued but the server has not
      // confirmed. It is still stored — that is what keeps the queued operation
      // alive — but it must not be rendered.
      if (doc !== undefined && (doc as Enveloped<Document>)._op !== 'delete') ordered.push(doc)
    }
    return ordered
  }, [found, ids, fromCollection])

  // Rebuild the shape the caller's `queryFn` returned. `data` for a detail view
  // is the document, not a one-element array; for an envelope it is whatever
  // `assemble` makes, with the live documents spliced back in.
  const { shape, payload } = state
  const assembled = useMemo<unknown>(() => {
    if (documents === undefined) return undefined
    if (shape === 'document') {
      // Undefined once the document is deleted locally. There is no honest
      // alternative: the thing this query names is gone.
      return documents[0] as unknown
    }
    if (shape === 'envelope' && assembleRef.current !== undefined) {
      const assemble = assembleRef.current as (documents: Doc[], raw: unknown) => unknown
      return assemble(documents as unknown as Doc[], payload)
    }
    return documents
    // `assembleRef` is a ref by design — an inline `assemble={(d, r) => …}`
    // must not rebuild `data` with a new identity on every render.
  }, [documents, shape, payload])

  // `select` runs on every render where the documents or the function itself
  // changed — the function identity matters because an inline `select` usually
  // closes over props, and reading it through a ref would freeze those.
  // Recomputing is safe because structural sharing immediately follows: an
  // inline arrow costs a call, not a new `data` identity.
  const { select } = options
  const selected = useMemo<unknown>(() => {
    if (select === undefined || assembled === undefined) return assembled
    return (select as (data: unknown) => unknown)(assembled)
  }, [assembled, select])

  // Hold the previous reference whenever the new value is equal, so that
  // `useEffect(…, [data])` in migrated code fires on real changes rather than
  // on every database snapshot.
  const sharedRef = useRef<unknown>(undefined)
  const shared = replaceEqualDeep(sharedRef.current, selected)
  sharedRef.current = shared

  // Remembered across a key change, which is what `keepPreviousData` reads.
  // Only real data is kept — remembering a placeholder would let one page's
  // stand-in become the next page's stand-in indefinitely.
  if (shared !== undefined) previousDataRef.current = shared

  const placeholder =
    shared === undefined && state.error === null ? resolvePlaceholder(options, previousDataRef.current) : undefined
  const isPlaceholderData = placeholder !== undefined
  const data = isPlaceholderData ? placeholder : shared

  // Held so `refetch` can resolve with a result object rather than void.
  const resultRef = useRef<QueryResult<unknown>>(undefined as never)

  const refetch = useCallback(
    async (opts?: {
      throwOnError?: boolean
      cancelRefetch?: boolean
    }): Promise<QueryResult<unknown>> => {
      // Default `true`, matching TanStack: an explicit refetch means "ask
      // again", not "give me whatever is already on its way".
      if (opts?.cancelRefetch !== false) cancelShared(inflightKey(name, keyHash))

      const controller = new AbortController()
      refetchControllersRef.current.add(controller)
      try {
        await revalidate(controller.signal)
      } catch (cause: unknown) {
        if (opts?.throwOnError) throw cause
      } finally {
        refetchControllersRef.current.delete(controller)
      }
      // Resolved on the next commit rather than immediately, so
      // `const { data } = await refetch()` sees the data it just fetched
      // instead of the render before it.
      return nextResult()
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [revalidate, name, keyHash],
  )

  const isFetching = state.fetchStatus === 'fetching'
  const isPending = ids === null
  // Computed at render rather than tracked: nothing needs to re-render at the
  // moment a query crosses its freshness boundary, since crossing it only
  // matters when something asks — a mount, a focus, an explicit refetch.
  const isStale =
    state.dataUpdatedAt === 0 || Date.now() >= state.dataUpdatedAt + staleTime

  const common: QueryResultCommon<unknown> = {
    fetchStatus: state.fetchStatus,
    isLoading: isPending && isFetching,
    isFetching,
    isPaused: state.fetchStatus === 'paused',
    isRefetching: isFetching && !isPending,
    isStale,
    isFetched: state.isFetched,
    isFetchedAfterMount: state.isFetchedAfterMount,
    isPlaceholderData,
    fromCollection: state.fromCollection,
    dataUpdatedAt: state.dataUpdatedAt,
    errorUpdatedAt: state.errorUpdatedAt,
    failureCount: state.failureCount,
    failureReason: state.failureReason,
    errorUpdateCount: state.errorUpdateCount,
    refetch,
  }

  const result: QueryResult<unknown> = buildResult(common, data, state.error)

  resultRef.current = result

  // Hand the committed result to anything awaiting `refetch()`. No dependency
  // array: it has to run after *every* commit, because the commit is the event.
  useEffect(() => {
    foundRef.current = found
    if (resolversRef.current.length === 0) return
    // Only those queued before this snapshot: a caller must not be answered
    // with the view it was already looking at.
    const ready = resolversRef.current.filter((entry) => entry.snapshot !== found)
    if (ready.length === 0) return
    resolversRef.current = resolversRef.current.filter((entry) => entry.snapshot === found)
    for (const { resolve } of ready) resolve(resultRef.current)
  })

  // Rethrow during render so an error boundary above can catch it. Last, so the
  // result is fully assembled first — `refetch` on `resultRef` stays usable by
  // anything that captured it before the throw.
  if (state.error !== null && shouldThrow(throwOnError, state.error)) throw state.error

  return result
}

/** Whether a fetch error should be rethrown for an error boundary. */
function shouldThrow(setting: boolean | ((error: Error) => boolean), error: Error): boolean {
  return typeof setting === 'function' ? setting(error) : setting
}

/**
 * Pending "forget this key" timers, keyed by collection and query hash.
 *
 * Module-level because the timer must outlive the component that scheduled it —
 * expiring a key *because* it went unused is the whole feature, and a cleanup
 * that cancelled itself on unmount would never fire.
 */
const forgetTimers = new Map<string, ReturnType<typeof setTimeout>>()

function forgetKey(collection: string, keyHash: string): string {
  return `${collection}\u0000${keyHash}`
}

function scheduleForget(
  collection: string,
  keyHash: string,
  after: number,
  run: () => void,
): void {
  const key = forgetKey(collection, keyHash)
  const timer = setTimeout(() => {
    forgetTimers.delete(key)
    run()
  }, after)
  // `unref` where it exists, so a pending timer cannot hold a Node process open.
  ;(timer as unknown as { unref?: () => void }).unref?.()
  forgetTimers.set(key, timer)
}

function cancelForget(collection: string, keyHash: string): void {
  const key = forgetKey(collection, keyHash)
  const timer = forgetTimers.get(key)
  if (timer === undefined) return
  clearTimeout(timer)
  forgetTimers.delete(key)
}

/** Test seam — pending timers are module state that would leak between cases. */
export function resetForgetTimers(): void {
  for (const timer of forgetTimers.values()) clearTimeout(timer)
  forgetTimers.clear()
}
