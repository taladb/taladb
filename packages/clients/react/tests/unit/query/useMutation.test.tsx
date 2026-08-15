/**
 * useMutation — the two modes.
 *
 * The claim under test for `optimistic` is that `pending` never becomes true:
 * the whole reason the mode exists is that the UI does not wait on a round trip.
 * For `remote-first` it is the opposite — nothing is written locally until the
 * server agrees.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { renderHook, waitFor, act } from '@testing-library/react'
import type { Document } from 'taladb'
import { TalaDBProvider } from '../../../src/context'
import { QueryProvider } from '../../../src/query/context'
import { useMutation } from '../../../src/query/useMutation'
import { defineBackend } from '../../../src/query/defineBackend'
import { createFakeDB } from '../../helpers/fakeQueryDB'
import type { ResolvedBackend } from '../../../src/query/types'

interface Todo extends Document {
  _id?: string
  title: string
  done?: boolean
}

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  })
}

function setup(backend?: ResolvedBackend) {
  const { db, docsIn } = createFakeDB()
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db}>
      <QueryProvider backend={backend}>{children}</QueryProvider>
    </TalaDBProvider>
  )
  return { wrapper, docsIn }
}

describe('useMutation — optimistic', () => {
  it('never reports pending, because it never waits on the network', async () => {
    const { wrapper, docsIn } = setup()
    const { result } = renderHook(() => useMutation<Todo>({ collection: 'todos' }), { wrapper })

    await act(async () => {
      await result.current.mutateAsync({ type: 'insert', doc: { title: 'offline' } })
    })

    expect(result.current.pending).toBe(false)
    const stored = docsIn('todos')
    expect(stored).toHaveLength(1)
    expect(stored[0]._sync).toBe('pending')
    expect(stored[0]._op).toBe('insert')
  })

  it('commits without a backend configured at all', async () => {
    // Offline-first: a write must not depend on knowing where it will be sent.
    const { wrapper, docsIn } = setup(undefined)
    const { result } = renderHook(() => useMutation<Todo>({ collection: 'todos' }), { wrapper })

    await act(async () => {
      await result.current.mutateAsync({ type: 'insert', doc: { title: 'x' } })
    })
    expect(docsIn('todos')).toHaveLength(1)
  })

  it('surfaces a failure on error rather than throwing from mutate', async () => {
    const { wrapper } = setup()
    const { result } = renderHook(() => useMutation<Todo>({ collection: 'todos' }), { wrapper })

    await act(async () => {
      // No such document — writes are local-first, so there is nothing to update.
      result.current.mutate({ type: 'update', where: { _id: 'nope' }, set: { title: 'x' } })
    })

    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error))
  })
})

describe('useMutation — remote-first', () => {
  it('writes nothing locally until the server agrees', async () => {
    const fetch = vi.fn().mockResolvedValue(jsonResponse({ title: 'server said so', done: true }))
    const backend = defineBackend({
      url: () => '/api/todos',
      classify: (r) => (r.ok ? 'ok' : 'terminal'),
      fetch: fetch as never,
    })
    const { wrapper, docsIn } = setup(backend)
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'remote-first' }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ type: 'insert', doc: { title: 'sent' } })
    })

    expect(fetch).toHaveBeenCalledTimes(1)
    const stored = docsIn('todos')
    expect(stored).toHaveLength(1)
    // The response body is canonical, so server-set fields land immediately.
    expect(stored[0].title).toBe('server said so')
    expect(stored[0].done).toBe(true)
    expect(stored[0]._sync).toBe('synced')
  })

  it('sends a client-generated id, so a retry cannot duplicate', async () => {
    let sent: Record<string, unknown> | null = null
    const fetch = vi.fn().mockImplementation(async (_url: string, init: RequestInit) => {
      sent = JSON.parse(init.body as string) as Record<string, unknown>
      return jsonResponse(sent)
    })
    const backend = defineBackend({
      url: () => '/api/todos',
      classify: () => 'ok',
      fetch: fetch as never,
    })
    const { wrapper } = setup(backend)
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'remote-first' }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ type: 'insert', doc: { title: 'x' } })
    })

    expect(typeof sent!._id).toBe('string')
    expect(sent!._id).toMatch(/^[0-9ABCDEFGHJKMNPQRSTVWXYZ]{26}$/)
  })

  it('leaves the database untouched when the server rejects', async () => {
    const backend = defineBackend({
      url: () => '/api/todos',
      classify: () => 'terminal',
      fetch: (async () => jsonResponse({ message: 'nope' }, 422)) as never,
    })
    const { wrapper, docsIn } = setup(backend)
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'remote-first' }),
      { wrapper },
    )

    await act(async () => {
      await expect(
        result.current.mutateAsync({ type: 'insert', doc: { title: 'doomed' } }),
      ).rejects.toThrow(/422/)
    })

    expect(docsIn('todos')).toHaveLength(0)
    expect(result.current.pending).toBe(false)
  })

  it('explains itself when no backend is configured', async () => {
    const { wrapper } = setup(undefined)
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'remote-first' }),
      { wrapper },
    )

    await act(async () => {
      await expect(
        result.current.mutateAsync({ type: 'insert', doc: { title: 'x' } }),
      ).rejects.toThrow(/backend/i)
    })
  })

  it('uses the verb the backend maps for each operation', async () => {
    const calls: string[] = []
    const fetch = vi.fn().mockImplementation(async (_u: string, init: RequestInit) => {
      calls.push(init.method as string)
      return jsonResponse({ title: 'x' })
    })
    const backend = defineBackend({
      url: () => '/api/todos',
      method: { update: 'PATCH' },
      classify: () => 'ok',
      fetch: fetch as never,
    })
    const { wrapper, docsIn } = setup(backend)
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'remote-first' }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ type: 'insert', doc: { title: 'x' } })
    })
    const id = docsIn('todos')[0]._id as string
    await act(async () => {
      await result.current.mutateAsync({ type: 'update', where: { _id: id }, set: { done: true } })
    })

    expect(calls).toEqual(['POST', 'PATCH'])
  })
})
