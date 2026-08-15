import type { Collection, Document } from 'taladb'
import { coalesce, stampPending, stampSynced } from './envelope'
import type { Enveloped } from './envelope'
import { DEFAULT_METHODS } from './endpoint'
import type { MutationOp, SyncEnvelope, SyncOp } from './types'

/** Where a queued write should be sent, stamped onto the document. */
export interface Route {
  url?: string
  method?: Partial<Record<SyncOp, string>>
}

/**
 * Stamp the route onto a queued document.
 *
 * Only when a url is given: without one the drain falls back to the provider's
 * backend, and writing `_endpoint: undefined` would shadow that.
 */
function stampRoute(route: Route | undefined, op: SyncOp): Record<string, unknown> {
  if (route?.url === undefined) return {}
  return {
    _endpoint: route.url,
    _method: route.method?.[op] ?? DEFAULT_METHODS[op],
  }
}

/** What a local write did, for tests and for `useSyncStatus` to count. */
export interface LocalWriteResult {
  id: string
  /** What the document now owes the server, or `null` if it owes nothing. */
  op: SyncOp | null
  /** The write cancelled an unsent insert, so the document was removed outright. */
  cancelled: boolean
}

const OP_FOR: Record<MutationOp<Document>['type'], SyncOp> = {
  insert: 'insert',
  update: 'update',
  delete: 'delete',
}

/**
 * Apply a write locally and record what the server still owes.
 *
 * Deletes become **tombstones**, not deletions. A queued delete lives on the
 * document it deletes; calling `deleteOne` here would erase the only record of
 * the operation, and the server would never hear about it. The document is
 * removed for real once the drain gets an acknowledgement.
 *
 * At most one operation is pending per document — a second edit folds into the
 * first rather than queueing behind it. That is what removes the dependency
 * graph, the id-remap table, and the replay log in one stroke; the cost is that
 * intermediate states are lost, so `$inc`-style writes cannot be expressed
 * offline.
 */
export async function writeLocal<T extends Document>(
  collection: Collection<T>,
  op: MutationOp<T>,
  now: number,
  route?: Route,
): Promise<LocalWriteResult> {
  if (op.type === 'insert') {
    const doc = { ...stampPending(op.doc as T, 'insert'), ...stampRoute(route, 'insert') }
    const id = await collection.insert(doc)
    return { id, op: 'insert', cancelled: false }
  }

  const id = op.where._id
  const existing = (await collection.findOne({ _id: id } as never)) as Enveloped<T> | null
  if (existing === null) {
    throw new Error(
      `Cannot ${op.type} document ${id}: it is not in the local collection. ` +
        'Writes apply locally first, so the document must already be here.',
    )
  }

  const next = coalesce(pendingOp(existing), OP_FOR[op.type])

  // An insert the server never saw, now deleted: nothing to tell it about.
  if (next === null) {
    await collection.deleteOne({ _id: id } as never)
    return { id, op: null, cancelled: true }
  }

  const fields: Record<string, unknown> = {
    ...(op.type === 'update' ? op.set : {}),
    _sync: 'pending',
    _op: next,
    _attempt: 0,
    _error: null,
    // The verb follows the coalesced operation, not the one just requested:
    // editing an unsent insert still POSTs.
    ...stampRoute(route, next),
  }
  await collection.updateOne({ _id: id } as never, { $set: fields } as never)
  return { id, op: next, cancelled: false }
}

/**
 * What this document currently owes the server.
 *
 * A `synced` document owes nothing regardless of any stale `_op` left behind,
 * and a document written before this layer existed owes nothing either.
 */
function pendingOp(doc: Partial<SyncEnvelope>): SyncOp | undefined {
  if (doc._sync !== 'pending' && doc._sync !== 'failed') return undefined
  return doc._op
}

/**
 * Apply a confirmed server document locally, after a `remote-first` write.
 *
 * The response body is canonical, so server-set fields — timestamps, defaults,
 * computed columns — land without a second fetch.
 */
export async function writeConfirmed<T extends Document>(
  collection: Collection<T>,
  doc: T,
  now: number,
): Promise<void> {
  const id = doc._id as string
  const stamped = stampSynced(doc, now)
  const { _id: _ignored, ...fields } = stamped
  const updated = await collection.updateOne({ _id: id } as never, { $set: fields } as never)
  if (!updated) await collection.insert(stamped)
}
