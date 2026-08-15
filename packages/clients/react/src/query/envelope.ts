import type { Document } from 'taladb'
import type { SyncEnvelope, SyncOp } from './types'

/**
 * Fields this layer owns on every document it manages.
 *
 * They live on the document rather than in a separate outbox collection because
 * TalaDB exposes no multi-collection transaction: a document write and an
 * outbox row describing it would be two commits, and a crash between them
 * either orphans a queued mutation or strands a document that never syncs.
 * Keeping the state here makes every queued write a single atomic commit — and
 * turns the queue into an indexed query.
 */
export const ENVELOPE_FIELDS = [
  '_sync',
  '_op',
  '_attempt',
  '_error',
  '_fetched_at',
  '_retry_at',
] as const

/**
 * Indexes this layer needs on every collection it manages.
 *
 * `_sync` is the queue the drain selects on. `_changed_at` is its order: the
 * drain sorts by it to send per-document writes in the order they happened, and
 * since 0.11.0 that field is no longer indexed automatically — without this the
 * sort falls back to reading the whole collection. `createIndex` is idempotent,
 * so both are safe to run on every registration.
 */
export const ENVELOPE_INDEXES = ['_sync', '_changed_at'] as const

/**
 * Reserved field names present in a document, if any.
 *
 * Checked against real documents rather than against the registered schema:
 * schemas are Standard Schema values (Zod, Valibot, …) with no portable way to
 * enumerate their keys, and the failure this guards against — a server response
 * carrying a `_sync` field that overwrites the envelope and drops the document
 * out of the queue — shows up in the data regardless of what the schema says.
 */
export function reservedFieldsIn(doc: Record<string, unknown>): string[] {
  return ENVELOPE_FIELDS.filter((field) => field in doc)
}

/** A document as stored: the caller's fields plus the envelope. */
export type Enveloped<T extends Document> = T & SyncEnvelope

/**
 * Stamp a document that came from the server and matches it.
 *
 * Only ever applied to documents that are not currently pending — a response
 * must never overwrite an unsynced local edit, which is the mistake a bulk
 * upsert makes by default.
 */
export function stampSynced<T extends Document>(doc: T, now: number): Enveloped<T> {
  return {
    ...doc,
    _sync: 'synced',
    _attempt: 0,
    _error: null,
    _fetched_at: now,
    _retry_at: 0,
  }
}

/**
 * Stamp a locally-written document that the server has not seen yet.
 *
 * `_fetched_at` is deliberately preserved from any previous hydration: it
 * records when the server last spoke about this document, and a local edit does
 * not change that.
 */
export function stampPending<T extends Document>(
  doc: T,
  op: SyncOp,
  previous?: Partial<SyncEnvelope>,
): Enveloped<T> {
  return {
    ...doc,
    _sync: 'pending',
    _op: op,
    _attempt: 0,
    _error: null,
    _fetched_at: previous?._fetched_at ?? 0,
    // Eligible immediately; the drain pushes this forward when it backs off.
    _retry_at: 0,
  }
}

/**
 * Fold a new write into whatever this document already owed the server.
 *
 * At most one operation is pending per document, so a second edit overwrites
 * the first rather than queueing behind it. This is what removes the dependency
 * graph, the id-remap table, and the replay log in one go — the cost being that
 * intermediate states are lost, so `$inc`-style operations cannot be expressed
 * offline.
 *
 * Returns `null` when the write cancels the pending operation outright: an
 * insert the server never saw, then deleted, is simply gone.
 */
export function coalesce(pending: SyncOp | undefined, next: SyncOp): SyncOp | null {
  if (pending === undefined) return next
  // Never acknowledged, so the server only ever needs the final shape.
  if (pending === 'insert') return next === 'delete' ? null : 'insert'
  if (pending === 'update') return next === 'delete' ? 'delete' : 'update'
  // A delete is terminal: nothing follows it for this document.
  return 'delete'
}
