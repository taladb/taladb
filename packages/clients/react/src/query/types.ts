import type { Document } from 'taladb'

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

export interface UseQueryOptions<T extends Document> {
  collection: string
  key: QueryKey
  /** Fetch the current server state for this key. Honour `signal`. */
  fetch: (context: { signal: AbortSignal }) => Promise<T[]>
  /** Freshness window in ms. Omitted means "always revalidate". */
  ttl?: number
  /** Default `true`. */
  enabled?: boolean
}

export interface QueryResult<T extends Document> {
  /**
   * Live documents. Resolved through the local database rather than the fetch
   * response, so a local write to any of them re-renders this query with no
   * invalidation step.
   */
  data: T[]
  /** True only when nothing is cached yet. A stale hit renders immediately. */
  loading: boolean
  /** Cached, past its ttl, and being revalidated in the background. */
  stale: boolean
  /** Last fetch error. Does not clear `data`. */
  error: unknown | null
  refetch: () => Promise<void>
}

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
