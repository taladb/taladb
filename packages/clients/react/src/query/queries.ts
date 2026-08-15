import type { Collection, Document, TalaDB, Value } from 'taladb'
import type { QueryKey, QueryRecord, ResultShape } from './types'
import { ENVELOPE_INDEXES } from './envelope'
import { QUERY_COLLECTION, hashQueryKey, queryRecordId } from './keys'

/**
 * Prepare a collection to be managed by this layer.
 *
 * Idempotent — `createIndex` is a no-op when the index already exists, so this
 * runs on every registration rather than needing to track what it has seen.
 */
export async function ensureEnvelopeIndexes(collection: Collection<Document>): Promise<void> {
  for (const field of ENVELOPE_INDEXES) {
    await collection.createIndex(field)
  }
}

/** The `taladb_queries` collection, prepared for use. */
export async function openQueryCollection(db: TalaDB): Promise<Collection<QueryRecord>> {
  const collection = db.collection<QueryRecord>(QUERY_COLLECTION)
  // Read paths filter by collection when sweeping or debugging.
  await collection.createIndex('collection')
  return collection
}

/**
 * Read the record for one query shape, or `null` if this query has never been
 * fetched on this device.
 */
export async function readQueryRecord(
  queries: Collection<QueryRecord>,
  collection: string,
  key: QueryKey,
  hash?: (key: QueryKey) => string,
): Promise<QueryRecord | null> {
  return queries.findOne({ _id: queryRecordId(collection, key, hash) })
}

/**
 * Forget which documents a query returned.
 *
 * Deliberately *only* the record. The documents stay: they live in the
 * application's own collections, which it also reads directly, so deleting them
 * would destroy real user data rather than reclaim a cache. Dropping the record
 * makes the next mount cold, and it refetches to re-establish membership.
 */
export async function deleteQueryRecord(
  queries: Collection<QueryRecord>,
  collection: string,
  key: QueryKey,
  hash?: (key: QueryKey) => string,
): Promise<void> {
  await queries.deleteOne({ _id: queryRecordId(collection, key, hash) })
}

/**
 * Record what a fetch returned: which documents were in the result set, in
 * server order, and when.
 *
 * Membership is stored rather than derived because the two facts a client
 * cannot otherwise distinguish — "this document no longer matches the filter"
 * and "this document was deleted upstream" — look identical from here. Keeping
 * the id list means a refetch can shrink the result set without having to
 * delete anything.
 */
export async function writeQueryRecord(
  queries: Collection<QueryRecord>,
  collection: string,
  key: QueryKey,
  ids: string[],
  now: number,
  ttl: number,
  /** How `queryFn`'s return value mapped onto documents. */
  shape: ResultShape = 'array',
  /** The raw response, for envelope queries. `undefined` for every other shape. */
  payload?: Value,
  /** Override the key serialiser — see `queryKeyHashFn`. */
  hash: (key: QueryKey) => string = hashQueryKey,
): Promise<QueryRecord> {
  const record: QueryRecord = {
    _id: queryRecordId(collection, key, hash),
    collection,
    key: hash(key),
    ids,
    fetchedAt: now,
    ttl,
    shape,
    ...(payload === undefined ? {} : { payload }),
  }
  // Deterministic id, so a refetch overwrites its own record rather than
  // accumulating a second one.
  const replaced = await queries.updateOne(
    { _id: record._id },
    {
      $set: {
        ids,
        fetchedAt: now,
        ttl,
        collection,
        key: record.key,
        shape,
        // Written unconditionally so a query that stops returning an envelope
        // does not keep serving the previous response's `total` forever.
        payload: payload ?? null,
      },
    },
  )
  if (!replaced) await queries.insert(record)
  return record
}

/**
 * Whether a record is past its freshness window.
 *
 * Staleness drives a background refetch — never a deletion. The documents this
 * layer hydrates land in the application's own collections, which the app also
 * reads directly; a TTL sweep over them would delete real user data, not
 * reclaim cache. A record with `ttl: 0` is always stale, which is how
 * "revalidate on every mount" is expressed.
 */
export function isStale(record: QueryRecord, now: number): boolean {
  return now >= record.fetchedAt + record.ttl
}
