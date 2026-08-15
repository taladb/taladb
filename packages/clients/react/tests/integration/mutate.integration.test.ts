// @vitest-environment node
/**
 * Local writes and coalescing, against a real engine.
 *
 * The two behaviours that carry the design: a delete is a tombstone (deleting
 * for real would erase the record carrying the queued operation), and a second
 * edit folds into the first rather than queueing behind it.
 *
 * Skipped when the Node binding is not built; see query-record.integration.test.ts.
 */
import { describe, it, expect, afterAll, beforeEach } from 'vitest'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { rm } from 'node:fs/promises'
import type { Collection, Document, TalaDB } from 'taladb'
import { writeLocal, writeConfirmed } from '../../src/query/mutate'
import { stampSynced } from '../../src/query/envelope'

interface Todo extends Document {
  _id?: string
  title: string
  done?: boolean
}

const dbPath = join(tmpdir(), `taladb-mutate-${process.pid}.db`)

let db: TalaDB | null = null
try {
  const mod = await import('taladb')
  db = await mod.openDB(dbPath)
} catch {
  db = null
}

afterAll(async () => {
  await db?.close()
  await rm(dbPath, { force: true }).catch(() => {})
  await rm(`${dbPath}-lock`, { force: true }).catch(() => {})
})

let todos: Collection<Todo>
let n = 0
beforeEach(() => {
  todos = db!.collection<Todo>(`m${n++}`)
})

describe.skipIf(db === null)('writeLocal', () => {
  it('queues an insert as pending', async () => {
    const { id, op } = await writeLocal(todos, { type: 'insert', doc: { title: 'new' } }, 1)
    expect(op).toBe('insert')

    const stored = await todos.findOne({ _id: id })
    expect(stored?._sync).toBe('pending')
    expect(stored?._op).toBe('insert')
  })

  it('makes a delete a tombstone, not a deletion', async () => {
    // Deleting for real would erase the only record of the queued operation,
    // and the server would never hear about it.
    const { id } = await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    await todos.updateOne({ _id: id }, { $set: { _sync: 'synced', _op: null } })

    const result = await writeLocal(todos, { type: 'delete', where: { _id: id } }, 2)

    expect(result.op).toBe('delete')
    expect(result.cancelled).toBe(false)
    const stored = await todos.findOne({ _id: id })
    expect(stored).not.toBeNull() // still stored
    expect(stored?._op).toBe('delete')
    expect(stored?._sync).toBe('pending')
  })

  it('keeps an unsent insert an insert when edited', async () => {
    const { id } = await writeLocal(todos, { type: 'insert', doc: { title: 'a' } }, 1)
    const result = await writeLocal(
      todos,
      { type: 'update', where: { _id: id }, set: { title: 'b' } },
      2,
    )

    expect(result.op).toBe('insert') // the server only ever needs the final shape
    const stored = await todos.findOne({ _id: id })
    expect(stored?.title).toBe('b')
    expect(stored?._op).toBe('insert')
  })

  it('cancels an unsent insert that is then deleted', async () => {
    const { id } = await writeLocal(todos, { type: 'insert', doc: { title: 'a' } }, 1)
    const result = await writeLocal(todos, { type: 'delete', where: { _id: id } }, 2)

    expect(result.op).toBeNull()
    expect(result.cancelled).toBe(true)
    // Nothing to tell the server about — it never knew this document existed.
    expect(await todos.findOne({ _id: id })).toBeNull()
  })

  it('lets a delete supersede a pending update', async () => {
    await todos.insert(stampSynced({ title: 'x' }, 1))
    const [doc] = await todos.find()
    const id = doc._id as string

    await writeLocal(todos, { type: 'update', where: { _id: id }, set: { done: true } }, 2)
    const result = await writeLocal(todos, { type: 'delete', where: { _id: id } }, 3)

    expect(result.op).toBe('delete')
    expect((await todos.findOne({ _id: id }))?._op).toBe('delete')
  })

  it('merges repeated updates into one pending operation', async () => {
    await todos.insert(stampSynced({ title: 'x' }, 1))
    const id = (await todos.find())[0]._id as string

    await writeLocal(todos, { type: 'update', where: { _id: id }, set: { title: 'y' } }, 2)
    const result = await writeLocal(
      todos,
      { type: 'update', where: { _id: id }, set: { title: 'z' } },
      3,
    )

    expect(result.op).toBe('update')
    const stored = await todos.findOne({ _id: id })
    expect(stored?.title).toBe('z')
  })

  it('resets the retry counter when a failed write is edited again', async () => {
    await todos.insert(stampSynced({ title: 'x' }, 1))
    const id = (await todos.find())[0]._id as string
    await todos.updateOne({ _id: id }, { $set: { _sync: 'failed', _op: 'update', _attempt: 3, _error: 'boom' } })

    await writeLocal(todos, { type: 'update', where: { _id: id }, set: { title: 'retry me' } }, 2)

    const stored = await todos.findOne({ _id: id })
    expect(stored?._sync).toBe('pending')
    expect(stored?._attempt).toBe(0)
    expect(stored?._error).toBeNull()
  })

  it('refuses to update a document that is not here', async () => {
    // Writes are local-first, so there is nothing to apply the change to.
    await expect(
      writeLocal(todos, { type: 'update', where: { _id: 'ZZZZZZZZZZZZZZZZZZZZZZZZZZ' }, set: {} }, 1),
    ).rejects.toThrow(/not in the local collection/)
  })
})

describe.skipIf(db === null)('writeConfirmed', () => {
  it('inserts a confirmed document that is not here yet', async () => {
    const id = (await todos.insert(stampSynced({ title: 'seed' }, 1))) as string
    await todos.deleteOne({ _id: id })

    await writeConfirmed(todos, { _id: id, title: 'from server' } as Todo, 5)

    const stored = await todos.findOne({ _id: id })
    expect(stored?.title).toBe('from server')
    expect(stored?._sync).toBe('synced')
  })

  it('overwrites a local document with the canonical server version', async () => {
    const id = (await todos.insert(stampSynced({ title: 'local' }, 1))) as string

    await writeConfirmed(todos, { _id: id, title: 'canonical', done: true } as Todo, 5)

    const stored = await todos.findOne({ _id: id })
    expect(stored?.title).toBe('canonical')
    expect(stored?.done).toBe(true)
    expect(stored?._fetched_at).toBe(5)
  })
})
