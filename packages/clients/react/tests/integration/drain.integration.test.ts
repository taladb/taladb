// @vitest-environment node
/**
 * The drain loop, against a real engine.
 *
 * Two paths carry the design and are tested explicitly:
 *
 *   - **409 after a timeout drains cleanly.** Without an `applied` outcome the
 *     queue stalls forever on a write the server already accepted.
 *   - **An in-flight edit beats its own response.** Applying the response there
 *     would silently revert a change the user watched themselves make.
 *
 * Skipped when the Node binding is not built; see query-record.integration.test.ts.
 */
import { describe, it, expect, afterAll, beforeEach, vi } from 'vitest'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { rm } from 'node:fs/promises'
import type { Collection, Document, TalaDB } from 'taladb'
import { backoffMs, drainOnce } from '../../src/query/drain'
import { writeLocal } from '../../src/query/mutate'
import type { ResolvedBackend } from '../../src/query/types'

const dbPath = join(tmpdir(), `taladb-drain-${process.pid}.db`)

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

let todos: Collection<Document>
let name: string
let n = 0
beforeEach(async () => {
  name = `d${n++}`
  todos = db!.collection<Document>(name)
  await todos.createIndex('_sync')
  await todos.createIndex('_changed_at')
})

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  })
}

function backendOf(fetch: unknown, classify?: ResolvedBackend['classify']): ResolvedBackend {
  return {
    url: (op) => `/api/${op.collection}/${op.id}`,
    method: { insert: 'POST', update: 'PUT', delete: 'DELETE' },
    classify:
      classify ??
      ((r) => (r.ok ? 'ok' : r.status === 409 ? 'applied' : r.status >= 500 ? 'retry' : 'terminal')),
    fetch: fetch as never,
  }
}

const deps = (backend: ResolvedBackend, extra: Record<string, unknown> = {}) => ({
  db: db!,
  backend,
  collections: () => [name],
  ...extra,
})

describe.skipIf(db === null)('drainOnce', () => {
  it('drains a 409 cleanly — the write already landed', async () => {
    // The classic retry-after-timeout: the first attempt succeeded on the
    // server but the response never arrived.
    await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    const fetch = vi.fn().mockResolvedValue(json({ message: 'already have it' }, 409))

    const stats = await drainOnce(deps(backendOf(fetch)))

    expect(stats.synced).toBe(1)
    expect(stats.failed).toBe(0)
    const [doc] = await todos.find()
    expect(doc._sync).toBe('synced')
  })

  it('lets an in-flight local edit beat its own response', async () => {
    const { id } = await writeLocal(todos, { type: 'insert', doc: { title: 'first' } }, 1)

    // Edit the document while the request is "in flight".
    const fetch = vi.fn().mockImplementation(async () => {
      await todos.updateOne({ _id: id }, { $set: { title: 'edited mid-flight' } })
      return json({ title: 'server version' })
    })

    const stats = await drainOnce(deps(backendOf(fetch)))

    expect(stats.superseded).toBe(1)
    expect(stats.synced).toBe(0)
    const doc = await todos.findOne({ _id: id })
    expect(doc?.title).toBe('edited mid-flight') // not reverted
    expect(doc?._sync).toBe('pending') // still queued, so it will be re-sent
  })

  it('sends in _changed_at order', async () => {
    const sent: string[] = []
    for (const title of ['first', 'second', 'third']) {
      await writeLocal(todos, { type: 'insert', doc: { title } }, 1)
      await new Promise((r) => setTimeout(r, 3))
    }
    const fetch = vi.fn().mockImplementation(async (_u: string, init: RequestInit) => {
      sent.push((JSON.parse(init.body as string) as { title: string }).title)
      return json({})
    })

    await drainOnce(deps(backendOf(fetch)))
    expect(sent).toEqual(['first', 'second', 'third'])
  })

  it('honours the batch limit', async () => {
    for (const title of ['a', 'b', 'c']) {
      await writeLocal(todos, { type: 'insert', doc: { title } }, 1)
      await new Promise((r) => setTimeout(r, 3))
    }
    const fetch = vi.fn().mockResolvedValue(json({}))

    const stats = await drainOnce(deps(backendOf(fetch), { batch: 2 }))
    expect(stats.sent).toBe(2)
  })

  it('backs off a 5xx instead of failing it', async () => {
    await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    const fetch = vi.fn().mockResolvedValue(json({}, 503))

    const stats = await drainOnce(deps(backendOf(fetch), { now: () => 10_000, random: () => 0.5 }))

    expect(stats.deferred).toBe(1)
    const [doc] = await todos.find()
    expect(doc._sync).toBe('pending')
    expect(doc._attempt).toBe(1)
    expect(doc._retry_at).toBe(11_000) // 10_000 + 1s, no jitter at 0.5
  })

  it('does not re-send a document that is still backing off', async () => {
    await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    const fetch = vi.fn().mockResolvedValue(json({}, 503))

    await drainOnce(deps(backendOf(fetch), { now: () => 10_000, random: () => 0.5 }))
    const second = await drainOnce(deps(backendOf(fetch), { now: () => 10_500 }))

    expect(second.sent).toBe(0)
  })

  it('parks a 4xx as failed and stops retrying it', async () => {
    await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    const fetch = vi.fn().mockResolvedValue(json({ message: 'invalid' }, 422))

    const stats = await drainOnce(deps(backendOf(fetch)))

    expect(stats.failed).toBe(1)
    const [doc] = await todos.find()
    expect(doc._sync).toBe('failed')
    expect(doc._error).toContain('422')

    // A second cycle must not pick it up again — it needs a human decision.
    expect((await drainOnce(deps(backendOf(fetch)))).sent).toBe(0)
  })

  it('treats a transport failure as retryable', async () => {
    await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    const fetch = vi.fn().mockRejectedValue(new TypeError('network down'))

    const stats = await drainOnce(deps(backendOf(fetch), { now: () => 1000, random: () => 0.5 }))

    expect(stats.deferred).toBe(1)
    expect((await todos.find())[0]._sync).toBe('pending')
  })

  it('removes the document for real once a delete is confirmed', async () => {
    const { id } = await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    await todos.updateOne({ _id: id }, { $set: { _sync: 'synced', _op: null } })
    await writeLocal(todos, { type: 'delete', where: { _id: id } }, 2)
    expect(await todos.count()).toBe(1) // still a tombstone

    const stats = await drainOnce(deps(backendOf(vi.fn().mockResolvedValue(json({})))))

    expect(stats.synced).toBe(1)
    expect(await todos.count()).toBe(0)
  })

  it('applies the canonical response body over the local document', async () => {
    await writeLocal(todos, { type: 'insert', doc: { title: 'mine' } }, 1)
    const fetch = vi.fn().mockResolvedValue(json({ title: 'mine', serverStamp: 'abc' }))

    await drainOnce(deps(backendOf(fetch)))

    const [doc] = await todos.find()
    expect(doc.serverStamp).toBe('abc')
  })

  it('never sends the sync envelope to the backend', async () => {
    await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    let body: Record<string, unknown> = {}
    const fetch = vi.fn().mockImplementation(async (_u: string, init: RequestInit) => {
      body = JSON.parse(init.body as string) as Record<string, unknown>
      return json({})
    })

    await drainOnce(deps(backendOf(fetch)))

    expect(body._id).toBeDefined()
    for (const field of ['_sync', '_op', '_attempt', '_error', '_fetched_at', '_retry_at']) {
      expect(body).not.toHaveProperty(field)
    }
  })

  it('does nothing at all on a secondary tab', async () => {
    // Correctness, not efficiency: a secondary reads a stale pending set and
    // its bookkeeping lands late, so it would re-send what it just sent.
    await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    const fetch = vi.fn()
    const secondary = { ...db!, isPrimary: async () => false } as TalaDB

    const stats = await drainOnce({
      db: secondary,
      backend: backendOf(fetch),
      collections: () => [name],
    })

    expect(stats.skippedNotPrimary).toBe(true)
    expect(fetch).not.toHaveBeenCalled()
  })
})

describe('backoffMs', () => {
  it('doubles from one second', () => {
    const noJitter = () => 0.5
    expect(backoffMs(1, noJitter)).toBe(1_000)
    expect(backoffMs(2, noJitter)).toBe(2_000)
    expect(backoffMs(3, noJitter)).toBe(4_000)
  })

  it('caps at five minutes', () => {
    expect(backoffMs(50, () => 0.5)).toBe(300_000)
  })

  it('jitters within ±25%', () => {
    // Spread retries so a fleet of clients does not reconnect in lockstep.
    expect(backoffMs(1, () => 0)).toBe(750)
    expect(backoffMs(1, () => 1)).toBe(1_250)
  })
})

describe.skipIf(db === null)('per-hook routing', () => {
  it('sends a queued write to the url stamped on it, not the provider default', async () => {
    // The point of the template: the component that chose this url is long gone
    // by the time the drain runs, so the route has to live on the document.
    await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1, {
      url: '/api/v2/todos/:id',
    })
    let called = ''
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      called = url
      return json({})
    })

    await drainOnce(deps(backendOf(fetch)))
    expect(called).toBe('/api/v2/todos')
  })

  it('resolves :id for an update', async () => {
    const { id } = await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    await todos.updateOne({ _id: id }, { $set: { _sync: 'synced', _op: null } })
    await writeLocal(todos, { type: 'update', where: { _id: id }, set: { title: 'y' } }, 2, {
      url: '/api/v2/todos/:id',
      method: { update: 'PATCH' },
    })

    let called = ''
    let verb = ''
    const fetch = vi.fn().mockImplementation(async (url: string, init: RequestInit) => {
      called = url
      verb = init.method as string
      return json({})
    })

    await drainOnce(deps(backendOf(fetch)))
    expect(called).toBe(`/api/v2/todos/${id}`)
    expect(verb).toBe('PATCH')
  })

  it('falls back to the provider backend when no url was stamped', async () => {
    await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    let called = ''
    const fetch = vi.fn().mockImplementation(async (url: string) => {
      called = url
      return json({})
    })

    await drainOnce(deps(backendOf(fetch)))
    expect(called).toContain('/api/')
  })

  it('leaves a write pending when it has neither a route nor a fallback', async () => {
    // Safe rather than lost: it will send as soon as a backend exists.
    await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1)
    const stats = await drainOnce({ db: db!, backend: null, collections: () => [name] })

    expect(stats.sent).toBe(0)
    expect((await todos.find())[0]._sync).toBe('pending')
  })

  it('keeps the insert verb when an unsent insert is edited', async () => {
    // Coalescing keeps `_op: 'insert'`, so `_method` must follow it — a PUT
    // to a record the server has never seen would 404.
    const { id } = await writeLocal(todos, { type: 'insert', doc: { title: 'a' } }, 1, {
      url: '/api/todos/:id',
    })
    await writeLocal(todos, { type: 'update', where: { _id: id }, set: { title: 'b' } }, 2, {
      url: '/api/todos/:id',
    })

    const doc = await todos.findOne({ _id: id })
    expect(doc?._op).toBe('insert')
    expect(doc?._method).toBe('POST')
  })

  it('never sends the route fields to the backend', async () => {
    await writeLocal(todos, { type: 'insert', doc: { title: 'x' } }, 1, { url: '/api/todos/:id' })
    let body: Record<string, unknown> = {}
    const fetch = vi.fn().mockImplementation(async (_u: string, init: RequestInit) => {
      body = JSON.parse(init.body as string) as Record<string, unknown>
      return json({})
    })

    await drainOnce(deps(backendOf(fetch)))
    expect(body).not.toHaveProperty('_endpoint')
    expect(body).not.toHaveProperty('_method')
  })
})
