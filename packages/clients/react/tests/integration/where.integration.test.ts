// @vitest-environment node
/**
 * `where` against a real engine.
 *
 * The in-memory stand-in in tests/helpers answers the filter shapes this layer
 * composes, but it is our own implementation of them — so it cannot settle
 * questions about what the *engine* does. Two of those matter here:
 *
 * 1. Does `$and` compose an id list with a predicate, which is exactly how
 *    `where` is built?
 * 2. Does `$ne` match a document where the field is **absent**? That decides
 *    whether the tombstone rule can move out of JS and into the query — every
 *    synced document has no `_op` at all, so if `$ne` skips absent fields, an
 *    engine-side `_op: { $ne: 'delete' }` would hide the entire collection.
 *
 * Node environment, not the suite default of jsdom: with `window` present the
 * client picks its browser adapter and looks for a worker that is not there.
 *
 * Skipped when the Node binding is not built.
 */
import { describe, it, expect, afterAll } from 'vitest'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { rm } from 'node:fs/promises'
import type { Collection, Document, TalaDB } from 'taladb'
import { deriveDocId } from 'taladb'

interface Todo extends Document {
  _id?: string
  title: string
  done: boolean
  /** Present only on documents with a queued write — like the real envelope. */
  _op?: string
}

const dbPath = join(tmpdir(), `taladb-where-${process.pid}.db`)

let db: TalaDB | null = null
let todos: Collection<Todo> | null = null
// Real ids are ULIDs on disk, so a natural key is folded into one — the same
// `deriveDocId` the query layer uses when hydrating rows that already have an
// identity upstream.
const id = (key: string) => deriveDocId('todos', key)
try {
  const { openDB } = await import('taladb')
  db = await openDB(dbPath)
  todos = db.collection<Todo>('todos')
  await todos.insert({ _id: id('a'), title: 'one', done: false })
  await todos.insert({ _id: id('b'), title: 'two', done: true })
  await todos.insert({ _id: id('c'), title: 'three', done: false, _op: 'delete' })
} catch {
  db = null
}

afterAll(async () => {
  await db?.close()
  await rm(dbPath, { force: true }).catch(() => {})
  await rm(`${dbPath}-lock`, { force: true }).catch(() => {})
})

describe.skipIf(db === null)('where against a real engine', () => {
  it('composes an id list with a predicate via $and', async () => {
    const found = await todos!.find({
      $and: [{ _id: { $in: [id('a'), id('b'), id('c')] } }, { done: false }],
    })

    // The exact shape `useQuery` builds: membership from the record, narrowing
    // from the caller.
    expect(found.map((t) => t._id).sort()).toEqual([id('a'), id('c')].sort())
  })

  it('narrows within the id list, never beyond it', async () => {
    const found = await todos!.find({
      $and: [{ _id: { $in: [id('a')] } }, { done: false }],
    })

    // 'c' also matches `done: false` but is not in the id list, so it must not
    // appear — `where` filters within the server's result set.
    expect(found.map((t) => t._id)).toEqual([id('a')])
  })

  it('answers whether $ne matches a document with the field absent', async () => {
    const found = await todos!.find({ _op: { $ne: 'delete' } })
    const ids = found.map((t) => t._id).sort()

    // This is the question PLAN-query-parity.md §3.5 left open. Documents 'a'
    // and 'b' have no `_op` field at all; 'c' has `_op: 'delete'`.
    //
    // If this returns ['a','b'], the tombstone rule can move into the composed
    // filter. If it returns [] — absent fields not matching `$ne` — it must
    // stay in JS at render, because an engine-side `_op: { $ne: 'delete' }`
    // would hide every synced document in the collection.
    //
    // ANSWER: `$ne` does match absent fields, so moving the tombstone rule into
    // the composed filter is safe. It stays in JS anyway — the id list already
    // bounds the set to what one query returned, and hiding tombstones in one
    // place (render) beats hiding them in two.
    expect(ids).toEqual([id('a'), id('b')].sort())
  })
})
