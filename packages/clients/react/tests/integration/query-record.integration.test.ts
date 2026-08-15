// @vitest-environment node
/**
 * The query-record layer against a real engine.
 *
 * Node environment, not the suite default of jsdom: with `window` present the
 * client picks its browser adapter and looks for a worker that is not there.
 *
 * The mock collection in `tests/helpers` ignores filters and returns `false`
 * from `updateOne`, so it cannot answer the questions that matter here — does a
 * refetch overwrite its own record, does the engine accept a derived id, is
 * `createIndex` really idempotent. Those need the real thing.
 *
 * Engine availability is resolved at module scope rather than in `beforeAll`,
 * because `skipIf` is evaluated while tests are being collected — a flag set in
 * a hook is still unset by then, and every test would silently skip.
 *
 * Skipped when the Node binding is not built. CI's `@taladb/react` job does not
 * download that artifact today, so these run locally and in any job that does.
 */
import { describe, it, expect, afterAll } from 'vitest'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { rm } from 'node:fs/promises'
import type { Collection, Document, TalaDB } from 'taladb'
import type { QueryRecord } from '../../src/query/types'
import {
  ensureEnvelopeIndexes,
  openQueryCollection,
  readQueryRecord,
  writeQueryRecord,
  isStale,
} from '../../src/query/queries'
import { QUERY_COLLECTION, queryRecordId } from '../../src/query/keys'
import { stampSynced } from '../../src/query/envelope'

const dbPath = join(tmpdir(), `taladb-query-record-${process.pid}.db`)

let db: TalaDB | null = null
let queries: Collection<QueryRecord> | null = null
try {
  const { openDB } = await import('taladb')
  db = await openDB(dbPath)
  queries = await openQueryCollection(db)
} catch {
  db = null
}

afterAll(async () => {
  await db?.close()
  await rm(dbPath, { force: true }).catch(() => {})
  await rm(`${dbPath}-lock`, { force: true }).catch(() => {})
})

describe.skipIf(db === null)('query records against a real engine', () => {
  it('accepts the derived id — it is a valid ULID', async () => {
    const key = ['todos', { done: false }]
    const record = await writeQueryRecord(queries!, 'todos', key, ['a'], 1, 60)
    expect(record._id).toBe(queryRecordId('todos', key))
    expect(await readQueryRecord(queries!, 'todos', key)).not.toBeNull()
  })

  it('returns null for a query never fetched here', async () => {
    expect(await readQueryRecord(queries!, 'todos', ['never', 'seen'])).toBeNull()
  })

  it('overwrites its own record on refetch rather than accumulating', async () => {
    const key = ['todos', { page: 1 }]
    await writeQueryRecord(queries!, 'todos', key, ['a', 'b'], 100, 60)
    await writeQueryRecord(queries!, 'todos', key, ['b', 'c'], 200, 60)

    const record = await readQueryRecord(queries!, 'todos', key)
    expect(record?.ids).toEqual(['b', 'c'])
    expect(record?.fetchedAt).toBe(200)
    expect(await queries!.count({ _id: queryRecordId('todos', key) })).toBe(1)
  })

  it('lets a result set shrink without deleting documents', async () => {
    const todos = db!.collection<Document>('todos')
    await todos.insert(stampSynced({ title: 'kept' }, 1))
    const before = await todos.count()

    const key = ['todos', { page: 2 }]
    await writeQueryRecord(queries!, 'todos', key, ['x', 'y'], 100, 60)
    await writeQueryRecord(queries!, 'todos', key, ['x'], 200, 60)

    expect((await readQueryRecord(queries!, 'todos', key))?.ids).toEqual(['x'])
    expect(await todos.count()).toBe(before) // membership shrank; data did not
  })

  it('preserves server order in the id list', async () => {
    const key = ['ordered']
    const ids = ['c', 'a', 'b']
    await writeQueryRecord(queries!, 'todos', key, ids, 1, 60)
    expect((await readQueryRecord(queries!, 'todos', key))?.ids).toEqual(ids)
  })

  it('creates envelope indexes idempotently', async () => {
    const todos = db!.collection<Document>('todos')
    await ensureEnvelopeIndexes(todos)
    await ensureEnvelopeIndexes(todos) // must not throw on the second pass
    expect(JSON.stringify(await todos.listIndexes())).toContain('_sync')
  })

  it('uses a collection name the engine accepts', async () => {
    expect(await db!.listCollectionNames?.()).toContain(QUERY_COLLECTION)
  })
})

describe('isStale', () => {
  it('is fresh inside the window and stale on the boundary', () => {
    const record = { fetchedAt: 1000, ttl: 500 } as unknown as QueryRecord
    expect(isStale(record, 1499)).toBe(false)
    expect(isStale(record, 1500)).toBe(true)
  })

  it('treats ttl 0 as always revalidate', () => {
    expect(isStale({ fetchedAt: 1000, ttl: 0 } as unknown as QueryRecord, 1000)).toBe(true)
  })
})
