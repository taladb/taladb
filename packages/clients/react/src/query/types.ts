import type { Document, Filter, Value } from 'taladb'
import type { BoundParams, ParamValue } from './params'

// ---------------------------------------------------------------------------
// Document envelope
// ---------------------------------------------------------------------------

/** Where a document sits relative to the server. Indexed — this *is* the queue. */
export type SyncState = 'synced' | 'pending' | 'failed'

/** What a pending document still owes the server. */
export type SyncOp = 'insert' | 'update' | 'delete'

/**
 * Fields this layer adds to every document it manages, alongside the engine's
 * own `_id`, `_changed_at` and `_v`.
 *
 * They live *on the document* rather than in a separate outbox collection
 * because TalaDB exposes no multi-collection transaction: a document write and
 * an outbox row describing it would be two commits, and a crash between them
 * either orphans a queued mutation or strands a document that never syncs.
 * Keeping the state here makes every queued write a single atomic commit.
 *
 * All `_`-prefixed names are reserved. A user schema that declares one will be
 * rejected rather than silently overwritten.
 */
export interface SyncEnvelope {
  /** Drain state. Indexed. */
  _sync: SyncState
  /** Meaningful only while `_sync !== 'synced'`. */
  _op?: SyncOp
  /** Retry count, drives backoff. */
  _attempt: number
  /** Last terminal error, surfaced by `useSyncStatus`. */
  _error: string | null
  /**
   * Epoch ms of the last server hydration, or `0` for a document that only
   * exists locally so far.
   *
   * There is deliberately no `_expires_at` companion. Freshness belongs to a
   * *query*, not a document — see {@link QueryRecord} — and nothing here
   * expires by deletion: these documents live in the application's own
   * collections, which it also reads directly, so a TTL sweep would delete real
   * user data rather than reclaim a cache.
   */
  _fetched_at: number
  /**
   * Epoch ms before which the drain will not retry this document. `0` means
   * eligible now; the drain pushes it forward as it backs off.
   */
  _retry_at: number
  /**
   * Where this write is going, as a URL template — `/api/v2/todos/:id`.
   *
   * Stamped when the write is queued, because a queued write outlives the
   * component that made it: the drain may send it after a route change, a
   * reload, or a week later, when no closure survives to ask. A template is
   * data and can be stored; a function cannot.
   *
   * Absent falls back to the provider's backend.
   */
  _endpoint?: string
  /** HTTP verb for the current `_op`, recomputed whenever `_op` changes. */
  _method?: string
  /** Opaque local revision used to detect edits while a request is in flight. */
  _revision: string | null
}

/**
 * One record per query shape, in the `taladb_queries` collection.
 *
 * Separate from the documents on purpose. A collection name says *where
 * documents live*; a query key says *what went stale*. Collapse the two and TTL
 * becomes meaningless — a document belongs to many queries with different
 * freshness — and result-set membership becomes unrecoverable.
 */
export interface QueryRecord extends Document {
  /** `deriveDocId(QUERY_COLLECTION, collection + key)` — deterministic. */
  _id: string
  /** The collection whose documents this query returned. */
  collection: string
  /** The serialised query key, for debugging and inspection. */
  key: string
  /** Result-set membership, in server order. */
  ids: string[]
  fetchedAt: number
  /** Freshness window in ms. `0` means "always revalidate". */
  ttl: number
  /**
   * What `queryFn` returned, so a warm mount can rebuild the same shape without
   * fetching. A detail view whose response was one document must not come back
   * as a one-element array after a reload.
   */
  shape?: ResultShape
  /**
   * The raw response, for envelope queries only.
   *
   * Stored so `assemble` has its non-document half — `total`, `nextCursor` —
   * available on a warm mount. Written only when `assemble` is supplied, since
   * nothing else ever reads it and the response duplicates every document it
   * carries.
   */
  payload?: Value
}

// ---------------------------------------------------------------------------
// Backend contract
// ---------------------------------------------------------------------------

/**
 * How a request came out, from the drain loop's point of view.
 *
 * `applied` is the one people forget. A write that timed out and was retried
 * will often come back 409 — already applied. Without a way to say so, every
 * retried write poisons the queue permanently.
 */
export type WriteOutcome = 'ok' | 'applied' | 'retry' | 'terminal'

/** One queued write, as handed to the backend adapter. */
export interface PendingWrite {
  collection: string
  /** The client-generated ULID. Authoritative — the server must not reassign it. */
  id: string
  type: SyncOp
  /** The stored document after the local commit; `null` for a delete. */
  document: Record<string, unknown> | null
}

/**
 * The application's backend, described well enough for the drain loop to talk
 * to it. This is the only extension point: the library never guesses at URL
 * shapes or status codes.
 */
export interface BackendDefinition {
  /** Where this write goes. */
  url: (op: PendingWrite) => string
  /** Defaults to POST / PUT / DELETE for insert / update / delete. */
  method?: Partial<Record<SyncOp, string>>
  /** Resolved per request, so a write queued offline sends a current token. */
  headers?: () => HeadersInit | Promise<HeadersInit>
  /** Map a response onto the four outcomes. */
  classify: (response: Response, op: PendingWrite) => WriteOutcome
  /** Override the fetch implementation (tests, instrumentation). */
  fetch?: typeof globalThis.fetch
}

/** A `BackendDefinition` with defaults filled in. */
export interface ResolvedBackend extends BackendDefinition {
  method: Record<SyncOp, string>
}

// ---------------------------------------------------------------------------
// Drain
// ---------------------------------------------------------------------------

/**
 * When the queue drains. Separate from the mutation call site on purpose: the
 * call site expresses *intent*, the provider expresses *timing*. "Write now,
 * sync later on a schedule" is this knob, not a third mutation mode.
 */
export type DrainPolicy = 'auto' | 'interval' | 'online-only' | 'manual'

export interface DrainOptions {
  /**
   * `auto` (default) sends an `optimistic` write as soon as it commits.
   * `interval` waits for its timer. `online-only` sends while online and resumes
   * on the browser's `online` event. `manual` never drains on its own.
   *
   * A `queued` mutation ignores `auto` and waits for a trigger either way.
   */
  policy?: DrainPolicy
  /** For `'interval'`. */
  intervalMs?: number
  /** Documents per drain cycle. */
  batch?: number
}

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

/**
 * Identifies a query *shape*, not a storage location.
 *
 * A collection name says where documents live; a key says what went stale.
 * Collapsing the two makes TTL meaningless and server-side removals invisible,
 * so they stay separate: documents land in `collection`, freshness is tracked
 * per `key`.
 */
export type QueryKey = readonly unknown[]

/**
 * The constraint on a user's model type.
 *
 * Deliberately **not** `Document`. `Document` carries an index signature, and
 * TypeScript does not give interfaces implicit index signatures, so an ordinary
 * application model —
 *
 * ```ts
 * interface Todo { _id: string; title: string }
 * ```
 *
 * — fails `T extends Document` outright. Requiring every model in a migrating
 * codebase to be restated as a type alias or to `extends Document` is a far
 * larger diff than the import line this surface exists to keep small. The
 * narrowing to `Document` happens internally, at the collection boundary, where
 * the registered schema validates at runtime anyway.
 */
export type Doc = { _id?: string }

/** `false` never, `true` when stale, `'always'` regardless of freshness. */
export type RefetchTrigger = boolean | 'always'

/** `false` never, `true` forever, a number of attempts, or a per-failure decision. */
export type RetryOption = boolean | number | ((failureCount: number, error: Error) => boolean)

/** A fixed delay, or one computed from the zero-based attempt index. */
export type RetryDelayOption = number | ((attemptIndex: number, error: Error) => number)

/** How connectivity gates fetching. */
export type NetworkMode = 'online' | 'always' | 'offlineFirst'

/** What `queryFn` is handed. Mirrors TanStack's `QueryFunctionContext`. */
export interface QueryFunctionContext {
  queryKey: QueryKey
  signal: AbortSignal
  meta?: Record<string, unknown>
  /**
   * The bound request parameters, if `params` was given.
   *
   * Always in bound form, even when a plain object was passed, so
   * `params.toSearchParams()` works either way. The library never builds a URL
   * itself — it only reads the half of the declaration that concerns the
   * collection.
   */
  params?: BoundParams
}

/**
 * Options shared by every `queryFn` shape.
 *
 * `TFetched` is what `queryFn` returns; `TData` is what `data` ends up as once
 * `assemble` and `select` have run. They differ whenever a transform is in play.
 */
export interface BaseQueryOptions<TFetched = unknown, TData = unknown> {
  /**
   * Where fetched documents are stored.
   *
   * Optional: defaults to `queryKey[0]` when that is a string, which is what
   * makes a migrated `queryKey: ['todos', …]` need no edit. Inference is
   * validated against the provider's collection registry rather than trusted —
   * this option decides the *physical destination* of every fetched document,
   * so a wrong value pours server data into a collection the app reads for
   * something else.
   */
  collection?: string
  queryKey: QueryKey
  /**
   * Freshness window in ms. Defaults to **one hour**, not TanStack's `0`.
   *
   * A memory cache that dies on reload is usually seconds old and revalidating
   * it is nearly free; a cache that survives reload and is expected to work
   * offline is neither. Pass `0` for TanStack's behaviour, or set it once on
   * the provider.
   */
  staleTime?: number
  /**
   * Default `true`. A function is re-evaluated on every render, which is how a
   * dependent query waits for the value it needs.
   *
   * Note it takes no arguments, where TanStack passes the query — exposing our
   * internals to make one signature match would be a poor trade.
   */
  enabled?: boolean | (() => boolean)
  /** Opaque; passed through to `queryFn`. */
  meta?: Record<string, unknown>
  /**
   * Data to start from on a cold key, instead of fetching into a spinner.
   *
   * Unlike {@link BaseQueryOptions.placeholderData} this is **persisted**: the
   * documents are hydrated into the collection and a query record is written,
   * exactly as a fetch would. It is server data the app happened to already
   * have — from SSR, a route loader, a bundled seed — not a stand-in for it.
   */
  // `NoInfer` throughout the transform-adjacent options: without it, a generic
  // helper like `keepPreviousData` is an inference site for `TData`, which
  // collapses to `unknown` and takes `data` with it. These options should be
  // *checked* against the shape `queryFn` and `select` establish, never allowed
  // to decide it.
  initialData?: NoInfer<TFetched> | (() => NoInfer<TFetched>)
  /**
   * When {@link BaseQueryOptions.initialData} was last true, as epoch ms.
   * Defaults to now.
   *
   * Feeds staleness, so seeded data can be born stale and revalidate straight
   * away — which is usually what an SSR payload wants.
   */
  initialDataUpdatedAt?: number
  /**
   * Data to show *instead of* a pending state, never written anywhere.
   *
   * `placeholderData: keepPreviousData` is the common case: hold the last page
   * on screen while the next one loads rather than blanking the list. The
   * result reports `isPlaceholderData: true` so the UI can dim it.
   *
   * This is in the *final* `data` space — after `assemble` and `select` — which
   * is what lets `keepPreviousData` hand back the previous `data` untouched.
   * TanStack places it before `select`; the difference shows only for a
   * placeholder that is not the previous value.
   */
  placeholderData?:
    | NoInfer<TData>
    | ((previous: NoInfer<TData> | undefined) => NoInfer<TData> | undefined)
  /**
   * How many times to retry a failed fetch. Default `3`.
   *
   * `false` disables retrying, `true` retries forever, a number caps the
   * attempts, and a predicate decides per failure — which is how a 404 is kept
   * from being retried four times.
   */
  retry?: RetryOption
  /** Milliseconds between attempts. Default is exponential from 1s, capped at 30s. */
  retryDelay?: RetryDelayOption
  /**
   * Revalidate on mount when the data is stale. Default `true`.
   *
   * `'always'` revalidates whatever `staleTime` says; `false` leaves a mounted
   * query showing whatever the device has until something else asks.
   */
  refetchOnMount?: RefetchTrigger
  /** Revalidate when the window regains focus, if stale. Default `true`. */
  refetchOnWindowFocus?: RefetchTrigger
  /** Revalidate when the network comes back, if stale. Default `true`. */
  refetchOnReconnect?: RefetchTrigger
  /** Poll every N ms. Default `false`. A function is re-evaluated per render. */
  refetchInterval?: number | false | (() => number | false)
  /** Keep polling while the tab is hidden. Default `false`. */
  refetchIntervalInBackground?: boolean
  /**
   * What to do about fetching while offline. Default `'online'`.
   *
   * `'online'` reports `fetchStatus: 'paused'` instead of failing, and resumes
   * on reconnect. `'always'` ignores connectivity — the right setting when
   * `queryFn` talks to something local. `'offlineFirst'` attempts once, which
   * suits a request that may be answered by a service worker.
   */
  networkMode?: NetworkMode
  /**
   * How long to remember this query's result set after nothing is using it.
   * Default `Infinity` — remember indefinitely.
   *
   * **Not garbage collection, and deliberately not called that.** Documents are
   * never deleted: they live in your own collections, which the application
   * also reads directly, so sweeping them would destroy real user data rather
   * than reclaim a cache. What expires is the *record* of which documents this
   * query returned — a few hundred bytes — after which the next mount is cold
   * and refetches to re-establish membership.
   *
   * The default is `Infinity` because a query record is what makes a warm start
   * warm, and a cache that survives reload has no memory pressure to relieve.
   * Set it when a result set is genuinely disposable — a search whose terms
   * will never be typed again.
   */
  forgetAfter?: number
  /**
   * Rethrow a fetch error during render, for an error boundary to catch.
   * Default `false`.
   */
  throwOnError?: boolean | ((error: Error) => boolean)
  /**
   * Serialise a query key into the string that identifies its record.
   *
   * The default sorts plain-object properties and preserves `undefined`, so
   * `{ a: 1, b: 2 }` and `{ b: 2, a: 1 }` are one query rather than two.
   * Replace it only if you need keys this cannot express — the result becomes
   * part of the record's identity on disk.
   */
  queryKeyHashFn?: (queryKey: QueryKey) => string
  /**
   * Request parameters — `?category=book&page=1`.
   *
   * Appended to `queryKey`, handed to `queryFn`, and (for parameters declared
   * with `defineParams`) translated into a local filter. A plain object gets
   * the first two; declaring the parameters adds the third.
   */
  params?: BoundParams | Record<string, ParamValue>
  /**
   * What to answer with when the key is cold and the fetch fails.
   *
   * `'strict'` (default) keeps the guarantee that `data` is exactly what the
   * server returned: no record, no answer. `'collection'` falls back to
   * querying everything cached through the `params` filter and reports
   * `fromCollection: true`, so the UI can say so rather than presenting a
   * partial set as authoritative.
   *
   * Only meaningful with parameters declared via `defineParams` — there is
   * otherwise no predicate to query the collection with.
   */
  offline?: 'strict' | 'collection'
  /**
   * Accepted and ignored — every result field is computed either way. Present
   * so migrated code compiles; noted once in development.
   */
  notifyOnChangeProps?: readonly string[] | 'all' | (() => readonly string[] | 'all' | undefined)
  /**
   * Accepted and ignored — structural sharing is always on here. Present so
   * migrated code compiles; noted once in development.
   */
  structuralSharing?: boolean | ((previous: unknown, next: unknown) => unknown)
}

/**
 * `queryFn` returns a list of documents. The common case.
 *
 * `TData` is inferred from `select` when it is present, and is `T[]` otherwise.
 */
export interface UseQueryOptions<T extends Doc, TData = T[]>
  extends BaseQueryOptions<T[], TData> {
  /** Fetch the current server state for this key. Honour `signal`. */
  queryFn: (context: QueryFunctionContext) => Promise<T[]>
  /**
   * Transform what this component sees, without changing what is stored.
   *
   * Runs over the live documents on every render in which they change, and its
   * output gets the same structural sharing they do — so an inline arrow here
   * is safe, and `data` keeps a stable identity between equal results.
   */
  select?: (data: T[]) => TData
  /**
   * Narrow this query's result set locally, in the engine.
   *
   * Composed with the query's stored id list as `$and`, so it filters *within*
   * what the server returned rather than replacing it. Indexed, and live: a
   * document edited out of the predicate leaves the view immediately, without
   * waiting for a refetch.
   *
   * Deliberately **not** part of the query key. That is the whole point — one
   * fetch, many narrowings, so All / Active / Done can be three components over
   * a single request. Folding it into the key would give each its own record
   * and its own fetch, which is worse than not having the option.
   *
   * It also never touches the stored record: the id list stays exactly what the
   * server returned. Otherwise a refetch would fight the filter, and a
   * locally-excluded document would be indistinguishable from one the server
   * deleted.
   */
  where?: Filter<T & Document>
  documents?: never
  assemble?: never
}

/**
 * `queryFn` returns one document — a detail view.
 *
 * `data` is that document, not a one-element array, and becomes `undefined` if
 * the document is deleted locally while the query is on screen. `select` is
 * only called when there is a document, so it never has to handle the gap.
 */
export interface UseDocumentQueryOptions<T extends Doc, TData = T>
  extends BaseQueryOptions<T, TData | undefined> {
  queryFn: (context: QueryFunctionContext) => Promise<T>
  select?: (data: T) => TData
  /**
   * Narrow this query's result set locally, in the engine.
   *
   * Composed with the query's stored id list as `$and`, so it filters *within*
   * what the server returned rather than replacing it. Indexed, and live: a
   * document edited out of the predicate leaves the view immediately, without
   * waiting for a refetch.
   *
   * Deliberately **not** part of the query key. That is the whole point — one
   * fetch, many narrowings, so All / Active / Done can be three components over
   * a single request. Folding it into the key would give each its own record
   * and its own fetch, which is worse than not having the option.
   *
   * It also never touches the stored record: the id list stays exactly what the
   * server returned. Otherwise a refetch would fight the filter, and a
   * locally-excluded document would be indistinguishable from one the server
   * deleted.
   */
  where?: Filter<T & Document>
  documents?: never
  assemble?: never
}

/**
 * `queryFn` returns an envelope — `{ items, total, nextCursor }` and the like.
 *
 * This is the one shape no rename can absorb: `data` is rebuilt from the local
 * collection, so the response has to say which part of itself is documents.
 * `documents` extracts them; `assemble` puts the result back together, and is
 * what keeps liveness — `data.items` re-renders on a local write while
 * `data.total` stays whatever the server said.
 */
export interface UseEnvelopeQueryOptions<
  TRaw,
  TDoc extends Doc,
  TAssembled = TDoc[],
  TData = TAssembled,
> extends BaseQueryOptions<TRaw, TData> {
  queryFn: (context: QueryFunctionContext) => Promise<TRaw>
  documents: (raw: TRaw) => TDoc[]
  /** Defaults to returning the documents alone. */
  assemble?: (documents: TDoc[], raw: TRaw) => TAssembled
  /**
   * Narrow this query's result set locally, in the engine.
   *
   * Composed with the query's stored id list as `$and`, so it filters *within*
   * what the server returned rather than replacing it. Indexed, and live: a
   * document edited out of the predicate leaves the view immediately, without
   * waiting for a refetch.
   *
   * Deliberately **not** part of the query key. That is the whole point — one
   * fetch, many narrowings, so All / Active / Done can be three components over
   * a single request. Folding it into the key would give each its own record
   * and its own fetch, which is worse than not having the option.
   *
   * It also never touches the stored record: the id list stays exactly what the
   * server returned. Otherwise a refetch would fight the filter, and a
   * locally-excluded document would be indistinguishable from one the server
   * deleted.
   */
  where?: Filter<TDoc & Document>
  select?: (data: TAssembled) => TData
}

/**
 * Provider-wide defaults, in TanStack's `new QueryClient({ defaultOptions })`
 * shape so that config migrates as a copy-paste.
 *
 * This is also where `staleTime: 0` goes to restore TanStack's revalidate-on-
 * every-mount behaviour in one line, and where a test harness sets
 * `retry: false` so failing requests fail immediately.
 */
export interface QueryDefaults {
  queries?: Pick<
    BaseQueryOptions,
    | 'staleTime'
    | 'retry'
    | 'retryDelay'
    | 'refetchOnMount'
    | 'refetchOnWindowFocus'
    | 'refetchOnReconnect'
    | 'refetchInterval'
    | 'refetchIntervalInBackground'
    | 'networkMode'
    | 'forgetAfter'
    | 'throwOnError'
  >
}

/** Any of the three forms, as the implementation sees them. */
// Transform parameters are `never` so that every public overload's narrower
// signature stays assignable to this one: function parameters are
// contravariant, and `never` is assignable to anything. The implementation
// casts at each call site, which is the honest place for it — this type exists
// only to give the erased signature a name.
export type AnyQueryOptions = BaseQueryOptions<unknown, unknown> & {
  queryFn: (context: QueryFunctionContext) => Promise<unknown>
  documents?: (raw: never) => Doc[]
  assemble?: (documents: never, raw: never) => unknown
  select?: (data: never) => unknown
  where?: Filter<Document>
}

/** How a `queryFn`'s return value maps onto documents. Stored on the record. */
export type ResultShape = 'array' | 'document' | 'envelope'

/** Fields present on a query result whatever its status. */
export interface QueryResultCommon<TData> {
  /** `'fetching'` while a request is in flight, `'paused'` when offline. */
  fetchStatus: 'fetching' | 'paused' | 'idle'
  /** `isPending && isFetching` — a first load with nothing to show yet. */
  isLoading: boolean
  isFetching: boolean
  isPaused: boolean
  /** Fetching over data that is already on screen. */
  isRefetching: boolean
  /**
   * Past `staleTime`.
   *
   * Note this is *not* "currently revalidating" — that is `isFetching`. The two
   * were one flag before parity work and the meanings are opposite.
   */
  isStale: boolean
  isFetched: boolean
  isFetchedAfterMount: boolean
  isPlaceholderData: boolean
  /**
   * `data` was answered from the whole collection rather than from this
   * query's stored result set — see `offline: 'collection'`. Always `false`
   * under the default `'strict'`.
   */
  fromCollection: boolean
  dataUpdatedAt: number
  errorUpdatedAt: number
  failureCount: number
  failureReason: Error | null
  errorUpdateCount: number
  refetch: (options?: {
    /** Reject rather than recording the failure on the result. */
    throwOnError?: boolean
    /**
     * Default `true`: abandon any request already on its way and start again.
     * `false` joins the in-flight request instead.
     */
    cancelRefetch?: boolean
  }) => Promise<QueryResult<TData>>
}

/**
 * The result of `useQuery`.
 *
 * A discriminated union rather than one object with `data: T[] | undefined`,
 * because the canonical React Query component shape depends on narrowing:
 *
 * ```tsx
 * if (isPending) return <Spinner/>
 * if (isError) return <p>{error.message}</p>
 * return <ul>{data.map(…)}</ul>   // `data` is T[] here, with no `!` or `?.`
 * ```
 *
 * `data` is resolved through the local database rather than from the fetch
 * response, so a local write to any document in the set re-renders this query
 * immediately — there is no cache to invalidate, because the collection *is*
 * the state.
 */
export type QueryResult<TData> =
  | (QueryResultCommon<TData> & {
      status: 'pending'
      /** Undefined, never `[]` — an empty array would render as an empty list. */
      data: undefined
      error: null
      isPending: true
      isSuccess: false
      isError: false
      isLoadingError: false
      isRefetchError: false
    })
  | (QueryResultCommon<TData> & {
      status: 'success'
      data: TData
      error: null
      isPending: false
      isSuccess: true
      isError: false
      isLoadingError: false
      isRefetchError: false
    })
  // The error case is two members, not one, mirroring TanStack: whether `data`
  // survives a failure is the difference between a toast over a populated
  // screen and a blank one, and it is worth knowing in the type rather than at
  // runtime.
  | (QueryResultCommon<TData> & {
      status: 'error'
      /** Nothing was cached, so the failure left nothing to render. */
      data: undefined
      error: Error
      isPending: false
      isSuccess: false
      isError: true
      isLoadingError: true
      isRefetchError: false
    })
  | (QueryResultCommon<TData> & {
      status: 'error'
      /**
       * A *refetch* failed over data the device already holds. Clearing the
       * screen here would undo the point of a persistent cache.
       */
      data: TData
      error: Error
      isPending: false
      isSuccess: false
      isError: true
      isLoadingError: false
      isRefetchError: true
    })

/**
 * When the network happens, relative to the local write.
 *
 * - `optimistic` (default) — commit locally, return, and send in the
 *   background as soon as possible. `pending` stays false: the UI never waits.
 * - `immediate` — send first and write locally only once the server agrees.
 *   `pending` reflects the request. For writes the user would be misled by if
 *   they silently failed: checkout, payment, anything irreversible.
 * - `queued` — commit locally and wait for the provider's schedule rather than
 *   sending straight away. For high-volume writes worth batching.
 */
export type MutationMode = 'optimistic' | 'immediate' | 'queued'

export interface UseMutationOptions {
  collection: string
  /**
   * URL template for this collection's writes — `/api/v2/todos/:id`.
   *
   * `:id` and `:collection` interpolate; an insert drops a trailing `/:id`.
   * A template rather than a function because a queued write is sent long
   * after its component is gone, so the route has to be storable.
   *
   * Omitted falls back to the provider's backend.
   */
  url?: string
  /** Verb overrides. Defaults to POST / PUT / DELETE. */
  method?: Partial<Record<SyncOp, string>>
  /** Default `'optimistic'`. */
  mode?: MutationMode
}

export interface MutationResult<T extends Document> {
  /** Fire-and-forget. Errors surface on `error`, never thrown to render. */
  mutate: (op: MutationOp<T>) => void
  /** Awaitable. Under `optimistic`, resolves on the *local* commit. */
  mutateAsync: (op: MutationOp<T>) => Promise<void>
  /** Always false under `optimistic` — that is the point. */
  pending: boolean
  error: unknown | null
}

/**
 * A write intent.
 *
 * `where` is `{ _id }` rather than an arbitrary filter: a filter-based write
 * issued from a secondary tab is forwarded to the primary and re-evaluated
 * there against authoritative data, so it can affect a different set of
 * documents than the optimistic UI just showed.
 */
export type MutationOp<T extends Document> =
  | { type: 'insert'; doc: Omit<T, '_id'> & { _id?: string } }
  | { type: 'update'; where: { _id: string }; set: Partial<Omit<T, '_id'>> }
  | { type: 'delete'; where: { _id: string } }

/** A write that failed terminally and will not retry on its own. */
export interface FailedWrite {
  collection: string
  id: string
  op: SyncOp
  error: string
  attempts: number
}

export interface SyncStatus {
  /** Queued, will send. */
  pending: number
  /** Terminal. Needs a human decision. */
  failed: FailedWrite[]
  /** A drain cycle is running. */
  draining: boolean
  online: boolean
  /** Requeue a failed write. */
  retry: (id: string) => void
  /** Drop the local change and stop trying. */
  discard: (id: string) => void
}
