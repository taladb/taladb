import type { Collection, Document, TalaDB } from 'taladb'
import { applyCanonical, withoutEnvelope } from './canonical'
import { defaultClassify, resolveUrl } from './endpoint'
import { sendWrite, toPendingWrite } from './send'
import type { ResolvedBackend, SyncOp } from './types'
import { isDev, warnDrainNotPrimary } from './validate'

export interface DrainStats {
  sent: number
  /** Confirmed by the server (`ok` or `applied`). */
  synced: number
  /** Will be retried after a back-off. */
  deferred: number
  /** Terminal — parked for a human decision. */
  failed: number
  /** Superseded by a local edit that landed while the request was in flight. */
  superseded: number
  /** True when the cycle did not run because this tab is not the primary. */
  skippedNotPrimary: boolean
}

const EMPTY: DrainStats = {
  sent: 0,
  synced: 0,
  deferred: 0,
  failed: 0,
  superseded: 0,
  skippedNotPrimary: false,
}

export interface DrainDeps {
  db: TalaDB
  /** Fallback for documents queued without their own route. */
  backend: ResolvedBackend | null
  /** Collections this layer manages, resolved fresh each cycle. */
  collections: () => string[]
  batch?: number
  now?: () => number
  random?: () => number
  /** Send with `keepalive`, for a flush on `pagehide`. */
  keepalive?: boolean
}

/** 1s, doubling, ±25% jitter, capped at five minutes. */
export function backoffMs(attempt: number, random: () => number = Math.random): number {
  const base = Math.min(1_000 * 2 ** Math.max(0, attempt - 1), 300_000)
  const jitter = 1 + (random() - 0.5) / 2
  return Math.round(base * jitter)
}

/**
 * Send every eligible queued write once.
 *
 * **Runs only on the primary tab.** This is a correctness requirement, not an
 * optimisation. A secondary tab reads a pending set that lags other tabs by up
 * to half a second, and its own bookkeeping — marking a document synced after a
 * successful request — is itself a forwarded write applied at the primary
 * later. Until that lands the document still reads as pending locally, so the
 * next cycle sends it again. Contract rules 2 and 3 keep that survivable rather
 * than corrupting, which is exactly why they are rules.
 *
 * Primary status changes mid-session when the owning tab closes, so it is
 * re-checked every cycle rather than cached.
 */
export async function drainOnce(deps: DrainDeps): Promise<DrainStats> {
  const { db, backend, collections } = deps
  const now = deps.now ?? Date.now
  const random = deps.random ?? Math.random
  const batch = deps.batch ?? 25

  if (!((await db.isPrimary?.()) ?? true)) {
    if (isDev()) warnDrainNotPrimary()
    return { ...EMPTY, skippedNotPrimary: true }
  }

  const stats: DrainStats = { ...EMPTY }

  for (const name of collections()) {
    const collection = db.collection<Document>(name)
    const due = (await collection.aggregate([
      { $match: { _sync: 'pending', _retry_at: { $lte: now() } } },
      { $sort: { _changed_at: 1 } },
      { $limit: batch },
    ])) as Document[]

    // Documents go concurrently; each document's own writes stay ordered
    // because at most one operation is ever pending per document.
    await Promise.all(due.map((doc) => sendOne(collection, name, doc, stats, deps, now, random)))
  }

  return stats
}

async function sendOne(
  collection: Collection<Document>,
  name: string,
  doc: Document,
  stats: DrainStats,
  deps: DrainDeps,
  now: () => number,
  random: () => number,
): Promise<void> {
  const id = doc._id as string
  const op = doc._op as SyncOp
  // Captured before the request so a local edit arriving mid-flight is visible
  // afterwards as a changed value.
  const sentRevision = doc._revision

  const backend = routeFor(doc, name, id, op, deps.backend)
  if (backend === null) {
    // Queued with no route and no provider fallback. Leaving it pending is the
    // honest outcome: the write is safe, and it will send once a backend exists.
    return
  }

  stats.sent++
  let result
  try {
    const attempt = (doc._attempt as number) ?? 0
        result = await sendWrite(
          backend,
          toPendingWrite(name, id, op, withoutEnvelope(doc)),
          undefined,
          attempt,
          deps.keepalive ?? false,
        )
  } catch {
    // A transport failure is indistinguishable from a 5xx: both may recover.
    await defer(collection, id, doc, stats, now, random)
    return
  }

  const current = await collection.findOne({ _id: id })
  if (current === null) return // deleted underneath us; nothing to record

  if (current._revision !== sentRevision) {
    // The user edited this document while its request was in flight. Applying
    // the response would silently revert a change they watched themselves make,
    // so the local edit wins and stays queued.
    stats.superseded++
    return
  }

  switch (result.outcome) {
    case 'ok':
    case 'applied': {
      // `applied` is a 409 for a write that already landed — usually a retry
      // after a timeout. Without treating it as success the queue would stall
      // on it permanently.
      if (op === 'delete') {
        await collection.deleteOne({ _id: id })
      } else {
        // A 409 has no body, and some endpoints violate the canonical-body
        // contract with an empty object. Settling the queue must not turn that
        // server mistake into local data loss; preserve the sent shape in that
        // case and let the development validator explain the missing body.
        const responseDocument = result.document
        const canonical = {
          ...(responseDocument && Object.keys(responseDocument).length > 0
            ? responseDocument
            : withoutEnvelope(current)),
          _id: id,
        } as Document
        await applyCanonical(collection, current, canonical, now())
      }
      stats.synced++
      return
    }
    case 'retry':
      await defer(collection, id, doc, stats, now, random)
      return
    case 'terminal':
      // Never retried on its own: it needs a human decision, surfaced through
      // useSyncStatus.
      await collection.updateOne(
        { _id: id },
        {
          $set: {
            _sync: 'failed',
            _error: `The server rejected this ${op} with status ${result.status}.`,
          },
        } as never,
      )
      stats.failed++
  }
}

async function defer(
  collection: Collection<Document>,
  id: string,
  doc: Document,
  stats: DrainStats,
  now: () => number,
  random: () => number,
): Promise<void> {
  const attempt = ((doc._attempt as number) ?? 0) + 1
  await collection.updateOne(
    { _id: id },
    { $set: { _attempt: attempt, _retry_at: now() + backoffMs(attempt, random) } } as never,
  )
  stats.deferred++
}

/**
 * Where this document should be sent.
 *
 * A route stamped at write time wins, so a per-hook `url` survives the
 * component that set it. The provider's backend is the fallback, and supplies
 * `headers` and `classify` either way — those are resolved at send time on
 * purpose, so a write queued offline carries a current token rather than the
 * one it was made with.
 */
function routeFor(
  doc: Document,
  collection: string,
  id: string,
  op: SyncOp,
  fallback: ResolvedBackend | null,
): ResolvedBackend | null {
  const template = doc._endpoint as string | undefined
  if (template === undefined) {
    if (fallback === null) return null
    const candidate = fallback.url(toPendingWrite(collection, id, op, withoutEnvelope(doc)))
    return candidate === '' ? null : { ...fallback, url: () => candidate }
  }

  const url = resolveUrl(template, collection, id, op)
  const method = (doc._method as string | undefined) ?? fallback?.method[op] ?? 'POST'
  return {
    url: () => url,
    method: { insert: method, update: method, delete: method },
    headers: fallback?.headers,
    classify: fallback?.classify ?? defaultClassify,
    fetch: fallback?.fetch,
  }
}
