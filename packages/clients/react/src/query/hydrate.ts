import type { Collection, Document } from 'taladb'
import { applyCanonical } from './canonical'
import { reservedFieldsIn, stampSynced } from './envelope'
import type { SyncState } from './types'

export interface HydrateResult {
  /** Documents the device had never seen. */
  inserted: number
  /** Documents refreshed from the response. */
  updated: number
  /** Documents left alone because they carry unsynced local work. */
  skipped: number
}

/**
 * Write a server response into the local collection.
 *
 * The rule that matters, and the one a naive bulk upsert gets wrong: **a
 * response must never overwrite a document with unsynced local work.** The
 * moment connectivity returns, a refetch and a queued edit race, and the
 * obvious implementation silently discards whatever the user did offline.
 * Anything not `synced` is therefore left exactly as it is — the drain loop
 * owns those documents until the server accepts them.
 *
 * Inserts go through one `insertMany`; updates cannot be batched, because each
 * document sets different values and the engine exposes no upsert. That matters
 * on a secondary browser tab, where every write is forwarded to the primary and
 * the forward buffer is bounded — see PLAN-query.md §5.
 */
export async function hydrate<T extends Document>(
  collection: Collection<T>,
  docs: T[],
  now: number,
): Promise<HydrateResult> {
  if (docs.length === 0) return { inserted: 0, updated: 0, skipped: 0 }

  const ids: string[] = []
  for (const doc of docs) {
    if (typeof doc._id !== 'string' || doc._id === '') {
      throw new Error(
        'A hydrated document has no `_id`. Client ids are authoritative: the ' +
          'endpoint must echo back the `_id` it was sent, and must not assign its own.',
      )
    }
    ids.push(doc._id)
    if (process.env.NODE_ENV !== 'production') warnOnReservedFields(doc)
  }

  // One read to learn which documents already exist and which are busy.
  const existing = await collection.find({ _id: { $in: ids } } as never)
  const state = new Map<string, T>()
  for (const doc of existing) {
    state.set(doc._id as string, doc)
  }

  const fresh: T[] = []
  let updated = 0
  let skipped = 0

  for (const doc of docs) {
    const id = doc._id as string
    if (!state.has(id)) {
      fresh.push(stampSynced(doc, now) as T)
      continue
    }
    // `undefined` means the document predates this layer — it owes nothing.
    const current = state.get(id)!
    const syncState = current._sync as SyncState | undefined
    if (syncState !== undefined && syncState !== 'synced') {
      skipped++
      continue
    }
    await applyCanonical(collection, current, doc, now)
    updated++
  }

  if (fresh.length > 0) await collection.insertMany(fresh)

  return { inserted: fresh.length, updated, skipped }
}

function warnOnReservedFields(doc: Document): void {
  const reserved = reservedFieldsIn(doc as Record<string, unknown>)
  if (reserved.length === 0) return
  console.warn(
    `[@taladb/react/query] A server response carried reserved field(s) ${reserved.join(', ')} ` +
      'on document ' +
      String(doc._id) +
      '. These belong to the sync envelope; the response will overwrite them and the ' +
      'document can fall out of the pending queue. Rename the field(s) on the server.',
  )
}
