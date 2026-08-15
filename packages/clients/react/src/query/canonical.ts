import type { Collection, Document, Update } from 'taladb'
import { ENVELOPE_FIELDS, stampSynced } from './envelope'

/** Fields owned by the storage engine rather than the server/query envelope. */
const ENGINE_FIELDS = new Set(['_id', '_changed_at'])

/** Remove this layer's bookkeeping while retaining application/engine fields. */
export function withoutEnvelope(doc: Document): Record<string, unknown> {
  const application: Record<string, unknown> = {}
  for (const [field, value] of Object.entries(doc)) {
    if (!(ENVELOPE_FIELDS as readonly string[]).includes(field)) application[field] = value
  }
  return application
}

/**
 * Replace the application's local shape with a canonical server document while
 * retaining the engine identity/timestamp and installing a clean envelope.
 * `$set` alone is insufficient: a field removed by the server would otherwise
 * survive locally forever.
 */
export async function applyCanonical<T extends Document>(
  collection: Collection<T>,
  current: T | null,
  canonical: T,
  now: number,
): Promise<void> {
  const id = canonical._id as string
  const stamped = stampSynced(canonical, now)

  if (current === null) {
    await collection.insert(stamped)
    return
  }

  const fields = { ...stamped } as Record<string, unknown>
  delete fields._id
  delete fields._changed_at

  const unset: Record<string, true> = {}
  for (const field of Object.keys(current)) {
    if (!ENGINE_FIELDS.has(field) && !(field in fields)) unset[field] = true
  }

  const update: Update<T> = { $set: fields as never }
  if (Object.keys(unset).length > 0) update.$unset = unset as never
  await collection.updateOne({ _id: id } as never, update)
}
