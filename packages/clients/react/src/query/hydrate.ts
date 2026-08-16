import { isDocId } from 'taladb'
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
/**
 * One hydration at a time per collection.
 *
 * `hydrate` reads which ids already exist and then writes, and the engine has
 * no multi-document transaction to make that atomic. Two overlapping calls both
 * read "absent" and both insert, so the second fails with `duplicate document
 * id` — losing every other document in that response. It is easy to reach: a
 * StrictMode effect fires twice, or two components hydrate the same collection
 * on one render.
 *
 * Keyed on the handle, which `useCollection` memoises per `(db, name)`, so
 * every caller in a tab lines up on the same chain. Links are detached from
 * each other's failures — one bad response must not wedge the queue.
 */
const hydrationChains = new WeakMap<Collection<Document>, Promise<unknown>>()

export function hydrate<T extends Document>(
  collection: Collection<T>,
  docs: T[],
  now: number,
  collectionName?: string,
): Promise<HydrateResult> {
  const key = collection as unknown as Collection<Document>
  const previous = hydrationChains.get(key) ?? Promise.resolve()
  const run = previous.then(() => hydrateOnce(collection, docs, now, collectionName))
  hydrationChains.set(key, run.catch(() => undefined))
  return run
}

async function hydrateOnce<T extends Document>(
  collection: Collection<T>,
  docs: T[],
  now: number,
  collectionName?: string,
): Promise<HydrateResult> {
  if (docs.length === 0) return { inserted: 0, updated: 0, skipped: 0 }

  // Collapse repeats by `_id`, keeping the last occurrence.
  //
  // Not a defensive nicety: without it, a payload naming the same row twice
  // puts two copies in `fresh`, and the `insertMany` below fails the whole
  // hydration with `duplicate document id`. That payload is ordinary — merged
  // shelves or carousels that share a bestseller, overlapping pages, a feed
  // joined across categories — and it is `_id` collapse, not `===`: two
  // fetches of one row are different objects.
  //
  // Last copy wins, matching the per-document loop below, where a later copy
  // would have overwritten an earlier one anyway. A `Map` keeps each id at the
  // position it first appeared, so server order survives.
  const unique = new Map<string, T>()
  for (const doc of docs) {
    if (!isDocId(doc._id)) throw badDocId(doc._id, collectionName)
    if (process.env.NODE_ENV !== 'production') warnOnReservedFields(doc)
    unique.set(doc._id, doc)
  }
  const deduped = [...unique.values()]
  const ids = [...unique.keys()]

  // One read to learn which documents already exist and which are busy.
  const existing = await collection.find({ _id: { $in: ids } } as never)
  const state = new Map<string, T>()
  for (const doc of existing) {
    state.set(doc._id as string, doc)
  }

  const fresh: T[] = []
  let updated = 0
  let skipped = 0

  for (const doc of deduped) {
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

/**
 * The error for a document the engine would refuse to store.
 *
 * Split from the engine's own message on purpose. By the time a bad id reaches
 * the engine the failure reads as a database problem, but this layer knows the
 * one thing that actually explains it: the value came from *your server*, over
 * the wire, and the fix belongs in `queryFn` rather than in the backend. The
 * overwhelmingly common case is a natural key — `265`, a slug, a UUID — being
 * passed straight through, so the message leads with that.
 */
function badDocId(value: unknown, collectionName?: string): Error {
  const where = collectionName ? ` for collection '${collectionName}'` : ''
  if (value === undefined || value === null || value === '') {
    return new Error(
      `[@taladb/react/query] A document in the response${where} has no \`_id\`. ` +
        '`data` is resolved from the local collection, not from what `queryFn` returned, ' +
        'so every document needs one. Map your rows in `queryFn`: ' +
        '`rows.map((row) => ({ ...row, _id: deriveDocId(collection, String(row.id)) }))`.',
    )
  }
  if (typeof value !== 'string') {
    return new Error(
      `[@taladb/react/query] \`_id\` must be a string${where}, got ${typeof value}. ` +
        'Derive one from your natural key with `deriveDocId(collection, String(row.id))`.',
    )
  }
  return new Error(
    `[@taladb/react/query] \`_id\` ${JSON.stringify(value)}${where} is not a ULID, ` +
      'so the engine cannot store it — ids are 16 raw bytes on disk. This is what a ' +
      'natural key (a numeric id, a slug, a UUID) looks like when it is passed straight ' +
      'through from the server. Fold it into a ULID in `queryFn` instead:\n' +
      "  import { deriveDocId } from 'taladb'\n" +
      `  rows.map((row) => ({ ...row, _id: deriveDocId(${
        collectionName ? `'${collectionName}'` : 'collection'
      }, String(row.id)) }))\n` +
      'The same key always maps to the same id, which is what keeps refetches idempotent.',
  )
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
