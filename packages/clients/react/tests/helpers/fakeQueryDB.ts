/**
 * A small in-memory TalaDB stand-in that actually honours the two filter shapes
 * the query layer uses: `{ _id }` and `{ _id: { $in: [...] } }`.
 *
 * The shared `createMockCollection` helper ignores filters entirely and returns
 * `false` from `updateOne`, which is fine for the local hooks but would make
 * every assertion here vacuous. This one is deliberately minimal — it is not a
 * second engine, just enough to drive `useQuery` in jsdom so the hook's own
 * logic (ordering, tombstones, loading and stale transitions) is testable
 * without the native binding, and therefore in CI.
 *
 * Engine-level behaviour — real `$in` planning, duplicate-id rejection, index
 * creation — is covered against the real thing in tests/integration.
 */
import type { Collection, Document, TalaDB } from 'taladb'

type Rec = Record<string, unknown>

function matches(doc: Rec, filter: Rec | undefined): boolean {
  if (!filter) return true
  return Object.entries(filter).every(([field, expected]) => {
    const actual = doc[field]
    if (expected !== null && typeof expected === 'object' && '$in' in (expected as Rec)) {
      return ((expected as { $in: unknown[] }).$in ?? []).includes(actual)
    }
    return actual === expected
  })
}

export function createFakeCollection<T extends Document>(seed: T[] = []) {
  let docs: Rec[] = seed.map((d) => ({ ...d }))
  let autoId = 0
  const subscribers = new Set<{ filter: Rec | undefined; cb: (docs: T[]) => void }>()

  const snapshot = (filter: Rec | undefined) =>
    docs.filter((d) => matches(d, filter)).map((d) => ({ ...d })) as T[]

  function notify() {
    for (const { filter, cb } of subscribers) cb(snapshot(filter))
  }

  const collection = {
    subscribe(filter: Rec | undefined, callback: (docs: T[]) => void) {
      const entry = { filter, cb: callback }
      subscribers.add(entry)
      const first = snapshot(filter)
      void Promise.resolve().then(() => {
        if (subscribers.has(entry)) callback(first)
      })
      return () => subscribers.delete(entry)
    },
    find: async (filter?: Rec) => snapshot(filter),
    findOne: async (filter?: Rec) => snapshot(filter)[0] ?? null,
    count: async (filter?: Rec) => snapshot(filter).length,
    insert: async (doc: Rec) => {
      const _id = (doc._id as string) ?? `fake-${autoId++}`
      if (docs.some((d) => d._id === _id)) throw new Error(`DuplicateId: ${_id}`)
      docs.push({ ...doc, _id })
      notify()
      return _id
    },
    insertMany: async (incoming: Rec[]) => {
      const ids = incoming.map((doc) => (doc._id as string) ?? `fake-${autoId++}`)
      incoming.forEach((doc, i) => docs.push({ ...doc, _id: ids[i] }))
      notify()
      return ids
    },
    updateOne: async (filter: Rec, update: { $set?: Rec }) => {
      const target = docs.find((d) => matches(d, filter))
      if (!target) return false
      Object.assign(target, update.$set ?? {})
      notify()
      return true
    },
    deleteOne: async (filter: Rec) => {
      const before = docs.length
      docs = docs.filter((d) => !matches(d, filter))
      if (docs.length !== before) notify()
      return docs.length !== before
    },
    deleteMany: async (filter: Rec) => {
      const before = docs.length
      docs = docs.filter((d) => !matches(d, filter))
      notify()
      return before - docs.length
    },
    updateMany: async () => 0,
    createIndex: async () => {},
    dropIndex: async () => {},
    listIndexes: async () => ({ btree: [], fts: [], vector: [] }),
  } as unknown as Collection<T>

  return { collection, all: () => docs.map((d) => ({ ...d })) as T[] }
}

export function createFakeDB() {
  const collections = new Map<string, ReturnType<typeof createFakeCollection>>()
  const db = {
    collection<T extends Document>(name: string) {
      if (!collections.has(name)) collections.set(name, createFakeCollection<T>() as never)
      return collections.get(name)!.collection as unknown as Collection<T>
    },
    compact: async () => {},
    close: async () => {},
    isPrimary: async () => true,
  } as unknown as TalaDB

  return { db, docsIn: (name: string) => collections.get(name)?.all() ?? [] }
}
