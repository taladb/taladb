/**
 * `mutationFn`, and the boundary it cannot cross.
 *
 * This is the mutation half of the residue: TanStack's mutation *is* a
 * function, and under `optimistic` or `queued` the request is sent by the drain
 * loop long after the closure is gone. The interesting tests here are the ones
 * that assert a refusal.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { act, renderHook, waitFor } from '@testing-library/react'
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

function setup() {
  const { db, docsIn } = createFakeDB()
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db}>
      <QueryProvider>{children}</QueryProvider>
    </TalaDBProvider>
  )
  return { wrapper, docsIn, db }
}

describe('the boundary', () => {
  it.each(['optimistic', 'queued'] as const)('refuses mutationFn under %s', (mode) => {
    const { wrapper } = setup()
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})

    expect(() =>
      renderHook(
        () =>
          useMutation<Todo>({
            collection: 'todos',
            mode,
            mutationFn: async (todo) => todo as Todo,
          } as never),
        { wrapper },
      ),
    ).toThrow(new RegExp(mode))

    spy.mockRestore()
  })

  it('throws on the first render, not the first click', () => {
    const { wrapper } = setup()
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})

    // A component wired this way is broken however it is used. Finding out on
    // the first paint beats finding out in production on the first submit.
    expect(() =>
      renderHook(
        () =>
          useMutation<Todo>({
            collection: 'todos',
            mutationFn: async (todo) => todo as Todo,
          } as never),
        { wrapper },
      ),
    ).toThrow(/drain loop/)

    spy.mockRestore()
  })

  it('names both ways out in the message', () => {
    const { wrapper } = setup()
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})

    let message = ''
    try {
      renderHook(
        () =>
          useMutation<Todo>({
            collection: 'todos',
            mutationFn: async (todo) => todo as Todo,
          } as never),
        { wrapper },
      )
    } catch (error) {
      message = (error as Error).message
    }

    // A codemod cannot fix this, so the runtime carries the guide.
    expect(message).toMatch(/url: '\/api\/todos\/:id'/)
    expect(message).toMatch(/mode: 'immediate'/)
    spy.mockRestore()
  })
})

describe('mutationFn under immediate', () => {
  it('owns the request, and its result is what gets stored', async () => {
    const { wrapper, docsIn } = setup()
    const fetch = vi.fn()
    const mutationFn = vi
      .fn()
      .mockResolvedValue({ _id: 'server-id', title: 'Buy milk', done: false })

    const { result } = renderHook(
      () =>
        useMutation<Todo>({
          collection: 'todos',
          mode: 'immediate',
          // `url` is deliberately absent: `mutationFn` replaces the routing,
          // not just the transport.
          mutationFn,
        }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'Buy milk' })
    })

    expect(mutationFn).toHaveBeenCalledWith({ title: 'Buy milk' })
    expect(fetch).not.toHaveBeenCalled()
    expect(docsIn('todos')).toHaveLength(1)
    // Server-assigned fields land without a second request.
    expect(docsIn('todos')[0]._id).toBe('server-id')
    expect(result.current.data?.title).toBe('Buy milk')
  })

  it('stores nothing when the function rejects', async () => {
    const { wrapper, docsIn } = setup()
    const mutationFn = vi.fn().mockRejectedValue(new Error('Checkout failed (402)'))

    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'immediate', mutationFn }),
      { wrapper },
    )

    await act(async () => {
      await expect(result.current.mutateAsync({ title: 'doomed' })).rejects.toThrow('402')
    })

    // The reason `immediate` is right for an irreversible write: nothing was
    // stored, so there is nothing to roll back.
    expect(docsIn('todos')).toHaveLength(0)
    expect(result.current.status).toBe('error')
    expect(result.current.error?.message).toMatch(/402/)
  })

  it('mints an id when the function returns none', async () => {
    const { wrapper, docsIn } = setup()
    const mutationFn = vi.fn().mockResolvedValue({ title: 'Buy milk' })

    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'immediate', mutationFn }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'Buy milk' })
    })

    expect(docsIn('todos')).toHaveLength(1)
    expect(typeof docsIn('todos')[0]._id).toBe('string')
  })
})

describe('onSynced', () => {
  it('fires straight away under immediate, where the server has already agreed', async () => {
    const { wrapper } = setup()
    const onSynced = vi.fn()
    const mutationFn = vi.fn().mockResolvedValue({ _id: 'a', title: 'Buy milk' })

    const { result } = renderHook(
      () =>
        useMutation<Todo>({ collection: 'todos', mode: 'immediate', mutationFn, onSynced }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'Buy milk' })
    })

    await waitFor(() => expect(onSynced).toHaveBeenCalledTimes(1))
  })

  it('waits for the drain under optimistic, and does not fire on the local commit', async () => {
    const { wrapper } = setup()
    const onSynced = vi.fn()
    const onSuccess = vi.fn()

    const { result } = renderHook(
      () =>
        useMutation<Todo>({
          collection: 'todos',
          url: '/api/todos/:id',
          onSuccess,
          onSynced,
        }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'Buy milk' })
    })

    // The write is durable and the UI has updated — but nothing has reached the
    // server, and saying otherwise is the mistake that loses writes quietly.
    await waitFor(() => expect(onSuccess).toHaveBeenCalledTimes(1))
    expect(onSynced).not.toHaveBeenCalled()
  })

  it('fires once the document is marked synced, whoever marked it', async () => {
    const { wrapper, docsIn, db } = setup()
    const onSynced = vi.fn()

    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', url: '/api/todos/:id', onSynced }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'Buy milk' })
    })
    const id = docsIn('todos')[0]._id as string
    expect(onSynced).not.toHaveBeenCalled()

    // Stands in for the drain. It is watched through the database rather than
    // hooked into the drain directly, because the confirmation may come from
    // another tab's drain — which only the database sees.
    await act(async () => {
      await db.collection('todos').updateOne({ _id: id } as never, {
        $set: { _sync: 'synced' },
      } as never)
    })

    await waitFor(() => expect(onSynced).toHaveBeenCalledTimes(1))
  })
})
