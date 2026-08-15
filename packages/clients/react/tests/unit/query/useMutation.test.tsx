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
import { createFakeDB } from '../../helpers/fakeQueryDB'

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

type Policy = { classify?: (r: Response) => 'ok' | 'applied' | 'retry' | 'terminal'; fetch?: unknown }

function setup(policy?: Policy) {
  const { db, docsIn } = createFakeDB()
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db}>
      <QueryProvider classify={policy?.classify as never} fetch={policy?.fetch as never}>
        {children}
      </QueryProvider>
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

describe('useMutation — immediate', () => {
  it('writes nothing locally until the server agrees', async () => {
    const fetch = vi.fn().mockResolvedValue(jsonResponse({ title: 'server said so', done: true }))
    const { wrapper, docsIn } = setup({ fetch: fetch as never })
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'immediate', url: '/api/todos/:id' }),
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
    const { wrapper } = setup({ fetch: fetch as never })
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'immediate', url: '/api/todos/:id' }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ type: 'insert', doc: { title: 'x' } })
    })

    expect(typeof sent!._id).toBe('string')
    expect(sent!._id).toMatch(/^[0-9ABCDEFGHJKMNPQRSTVWXYZ]{26}$/)
  })

  it('leaves the database untouched when the server rejects', async () => {
    const fetch = vi.fn().mockResolvedValue(jsonResponse({ message: 'nope' }, 422))
    const { wrapper, docsIn } = setup({ fetch: fetch as never })
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'immediate', url: '/api/todos/:id' }),
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

  it('explains itself when no url is given', async () => {
    const { wrapper } = setup(undefined)
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'immediate' }),
      { wrapper },
    )

    await act(async () => {
      await expect(
        result.current.mutateAsync({ type: 'insert', doc: { title: 'x' } }),
      ).rejects.toThrow(/url/i)
    })
  })

  it('works as a plain fetch with no provider backend at all', async () => {
    const fetch = vi.fn().mockResolvedValue(jsonResponse({ title: 'saved' }))
    const original = globalThis.fetch
    globalThis.fetch = fetch as never
    try {
      const { wrapper, docsIn } = setup(undefined)
      const { result } = renderHook(
        () => useMutation<Todo>({ collection: 'todos', mode: 'immediate', url: '/api/todos/:id' }),
        { wrapper },
      )
      await act(async () => {
        await result.current.mutateAsync({ type: 'insert', doc: { title: 'x' } })
      })
      expect(fetch).toHaveBeenCalledWith('/api/todos', expect.objectContaining({ method: 'POST' }))
      expect(docsIn('todos')[0].title).toBe('saved')
    } finally {
      globalThis.fetch = original
    }
  })

  it('uses the verb the backend maps for each operation', async () => {
    const calls: string[] = []
    const fetch = vi.fn().mockImplementation(async (_u: string, init: RequestInit) => {
      calls.push(init.method as string)
      return jsonResponse({ title: 'x' })
    })
    const { wrapper, docsIn } = setup({ fetch: fetch as never })
    const { result } = renderHook(
      () =>
        useMutation<Todo>({
          collection: 'todos',
          mode: 'immediate',
          url: '/api/todos/:id',
          method: { update: 'PATCH' },
        }),
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
