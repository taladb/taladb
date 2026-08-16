// @vitest-environment node
/**
 * Hydration against a real engine.
 *
 * The headline test is `never overwrites a pending local edit`. That is the
 * failure a bulk upsert produces by default, it only happens once connectivity
 * returns, and it destroys work the user watched themselves do — so it is
 * pinned here before anything else.
 *
 * Skipped when the Node binding is not built; see query-record.integration.test.ts.
 */
import { describe, it, expect, afterAll, beforeEach, vi } from 'vitest'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { rm } from 'node:fs/promises'
import type { Collection, Document, TalaDB } from 'taladb'
import { hydrate } from '../../src/query/hydrate'
import { stampPending, stampSynced } from '../../src/query/envelope'

interface Todo extends Document {
  _id?: string
  title: string
  done?: boolean
}

const dbPath = join(tmpdir(), `taladb-hydrate-${process.pid}.db`)

let db: TalaDB | null = null
let deriveDocId: ((c: string, k: string) => string) | null = null
try {
  const mod = await import('taladb')
  db = await mod.openDB(dbPath)
  deriveDocId = mod.deriveDocId
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
  // A fresh collection per test — deletes cannot remove a tombstone by design.
  todos = db!.collection<Todo>(`todos${n++}`)
})

const id = (k: string) => deriveDocId!('todos', k)

describe.skipIf(db === null)('hydrate', () => {
  it('never overwrites a pending local edit', async () => {
    const a = id('a')
    await todos.insert(stampPending({ _id: a, title: 'my offline edit' }, 'update') as never)

    const result = await hydrate(todos, [{ _id: a, title: 'server version' }], 1000)

    const stored = await todos.findOne({ _id: a })
    expect(stored?.title).toBe('my offline edit')
    expect(stored?._sync).toBe('pending')
    expect(stored?._op).toBe('update')
    expect(result).toEqual({ inserted: 0, updated: 0, skipped: 1 })
  })

  it('never overwrites a write that failed terminally', async () => {
    // Still local work awaiting a human decision — not the server's to discard.
    const a = id('f')
    const doc = { ...stampPending({ _id: a, title: 'rejected' }, 'update'), _sync: 'failed' }
    await todos.insert(doc as never)

    await hydrate(todos, [{ _id: a, title: 'server version' }], 1000)

    expect((await todos.findOne({ _id: a }))?.title).toBe('rejected')
  })

  it('refreshes a synced document', async () => {
    const a = id('b')
    await todos.insert(stampSynced({ _id: a, title: 'old' }, 1) as never)

    const result = await hydrate(todos, [{ _id: a, title: 'new', done: true }], 2000)

    const stored = await todos.findOne({ _id: a })
    expect(stored?.title).toBe('new')
    expect(stored?.done).toBe(true)
    expect(stored?._fetched_at).toBe(2000)
    expect(result).toEqual({ inserted: 0, updated: 1, skipped: 0 })
  })

  it('removes local fields absent from the canonical server document', async () => {
    const a = id('removed-field')
    await todos.insert(
      stampSynced({ _id: a, title: 'old', done: true, obsolete: 'local only' }, 1) as never,
    )

    await hydrate(todos, [{ _id: a, title: 'canonical' }], 2000)

    const stored = await todos.findOne({ _id: a })
    expect(stored?.title).toBe('canonical')
    expect(stored).not.toHaveProperty('done')
    expect(stored).not.toHaveProperty('obsolete')
  })

  it('adopts a document that predates this layer', async () => {
    // No `_sync` at all — it owes the server nothing, so refreshing is safe.
    const a = id('c')
    await todos.insert({ _id: a, title: 'legacy' } as never)

    await hydrate(todos, [{ _id: a, title: 'fresh' }], 3000)

    const stored = await todos.findOne({ _id: a })
    expect(stored?.title).toBe('fresh')
    expect(stored?._sync).toBe('synced')
  })

  it('inserts documents the device has never seen', async () => {
    const result = await hydrate(
      todos,
      [
        { _id: id('n1'), title: 'one' },
        { _id: id('n2'), title: 'two' },
      ],
      1000,
    )
    expect(result).toEqual({ inserted: 2, updated: 0, skipped: 0 })
    expect(await todos.count()).toBe(2)
  })

  it('mixes all three outcomes in one response', async () => {
    const [p, s] = [id('m1'), id('m2')]
    await todos.insert(stampPending({ _id: p, title: 'local' }, 'update') as never)
    await todos.insert(stampSynced({ _id: s, title: 'old' }, 1) as never)

    const result = await hydrate(
      todos,
      [
        { _id: p, title: 'server' },
        { _id: s, title: 'refreshed' },
        { _id: id('m3'), title: 'brand new' },
      ],
      2000,
    )

    expect(result).toEqual({ inserted: 1, updated: 1, skipped: 1 })
    expect((await todos.findOne({ _id: p }))?.title).toBe('local')
    expect((await todos.findOne({ _id: s }))?.title).toBe('refreshed')
  })

  it('is idempotent — replaying a response changes nothing', async () => {
    const docs = [{ _id: id('i1'), title: 'x' }]
    await hydrate(todos, docs, 1000)
    const second = await hydrate(todos, docs, 1000)

    expect(second).toEqual({ inserted: 0, updated: 1, skipped: 0 })
    expect(await todos.count()).toBe(1)
  })

  it('rejects a response document with no id', async () => {
    // Client ids are authoritative; a server that assigns its own breaks sync
    // permanently, so this fails loudly rather than inserting a duplicate.
    await expect(hydrate(todos, [{ title: 'no id' } as Todo], 1)).rejects.toThrow(/_id/)
  })

  /**
   * The failure every migration from TanStack Query hits first: a REST endpoint
   * returns `id: "265"`, it is passed straight through, and the engine refuses
   * it. Caught here rather than in the engine so the message can say where the
   * value came from and which call to make instead.
   */
  it('rejects a natural key and points at deriveDocId', async () => {
    const error = await hydrate(
      todos,
      [{ _id: '265', title: 'The Book of Dragons' } as Todo],
      1,
      'todos',
    ).catch((e: Error) => e)

    expect(error.message).toContain('not a ULID')
    expect(error.message).toContain('deriveDocId')
    // Names the offending value and the collection, so the fix is mechanical.
    expect(error.message).toContain('"265"')
    expect(error.message).toContain('todos')
  })

  it('rejects a UUID, which is a string but still not storable', async () => {
    await expect(
      hydrate(todos, [{ _id: crypto.randomUUID(), title: 'x' } as Todo], 1),
    ).rejects.toThrow(/not a ULID/)
  })

  it('rejects a non-string id', async () => {
    await expect(
      hydrate(todos, [{ _id: 265, title: 'x' } as unknown as Todo], 1),
    ).rejects.toThrow(/must be a string/)
  })

  /**
   * A payload naming the same row twice is ordinary — shelves merged for one
   * write, overlapping pages, a feed joined across categories. Before this was
   * collapsed, both copies reached `insertMany` and the whole hydration failed
   * with `duplicate document id`, losing every other document in the call.
   */
  it('collapses a document repeated within one response', async () => {
    const result = await hydrate(
      todos,
      [
        { _id: id('n1'), title: 'one' },
        { _id: id('n2'), title: 'two' },
        { _id: id('n1'), title: 'one, again' },
      ],
      1000,
    )

    expect(result).toEqual({ inserted: 2, updated: 0, skipped: 0 })
    expect(await todos.count()).toBe(2)
    // Last copy wins.
    expect((await todos.findOne({ _id: id('n1') }))?.title).toBe('one, again')
  })

  it('collapses a repeat against a document already stored', async () => {
    await todos.insert(stampSynced({ _id: id('n1'), title: 'old' }, 1) as never)

    const result = await hydrate(
      todos,
      [
        { _id: id('n1'), title: 'first' },
        { _id: id('n1'), title: 'second' },
      ],
      2000,
    )

    expect(result).toEqual({ inserted: 0, updated: 1, skipped: 0 })
    expect((await todos.findOne({ _id: id('n1') }))?.title).toBe('second')
  })

  /**
   * Two overlapping hydrations used to both read "absent" and both insert, and
   * the loser failed the entire response with `duplicate document id`. A
   * StrictMode effect firing twice is enough to cause it.
   */
  it('serialises overlapping hydrations of the same collection', async () => {
    const response = [
      { _id: id('n1'), title: 'one' },
      { _id: id('n2'), title: 'two' },
    ]

    const [first, second] = await Promise.all([
      hydrate(todos, response, 1000),
      hydrate(todos, response, 1000),
    ])

    expect(first).toEqual({ inserted: 2, updated: 0, skipped: 0 })
    // The second call sees the first's writes rather than colliding with them.
    expect(second).toEqual({ inserted: 0, updated: 2, skipped: 0 })
    expect(await todos.count()).toBe(2)
  })

  it('does nothing on an empty response', async () => {
    expect(await hydrate(todos, [], 1)).toEqual({ inserted: 0, updated: 0, skipped: 0 })
  })

  it('warns when a response carries a reserved envelope field', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    await hydrate(todos, [{ _id: id('w'), title: 'x', _sync: 'synced' } as Todo], 1)
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('_sync'))
    warn.mockRestore()
  })
})
