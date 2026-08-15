/**
 * Failure and scheduling policy for writes.
 *
 * The default worth arguing about is `retry: 0`. Queries retry three times;
 * writes retry none, because a read is idempotent and a write may not be —
 * retrying a `POST` that timed out *after* the server accepted it is how
 * duplicate rows appear.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { act, render, renderHook, screen, waitFor } from '@testing-library/react'
import type { Document } from 'taladb'
import { TalaDBProvider } from '../../../src/context'
import { QueryProvider } from '../../../src/query/context'
import { useMutation } from '../../../src/query/useMutation'
import { createFakeDB } from '../../helpers/fakeQueryDB'

interface Todo extends Document {
  _id?: string
  title: string
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

describe('retry', () => {
  it('does not retry by default', async () => {
    const { wrapper } = setup()
    const mutationFn = vi.fn().mockRejectedValue(new Error('boom'))

    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'immediate', mutationFn }),
      { wrapper },
    )

    await act(async () => {
      await expect(result.current.mutateAsync({ title: 'x' })).rejects.toThrow('boom')
    })

    // One attempt. A write is not assumed idempotent.
    expect(mutationFn).toHaveBeenCalledTimes(1)
    expect(result.current.failureCount).toBe(1)
  })

  it('retries when asked, for an endpoint that says it is safe', async () => {
    const { wrapper } = setup()
    const mutationFn = vi
      .fn()
      .mockRejectedValueOnce(new Error('flaky'))
      .mockResolvedValue({ _id: 'a', title: 'x' })

    const { result } = renderHook(
      () =>
        useMutation<Todo>({
          collection: 'todos',
          mode: 'immediate',
          mutationFn,
          retry: 2,
          retryDelay: 1,
        }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'x' })
    })

    expect(mutationFn).toHaveBeenCalledTimes(2)
    expect(result.current.status).toBe('success')
    // Reset on success, as TanStack does.
    expect(result.current.failureCount).toBe(0)
  })

  it('gives up after the cap and reports the last reason', async () => {
    const { wrapper } = setup()
    const mutationFn = vi.fn().mockRejectedValue(new Error('down'))

    const { result } = renderHook(
      () =>
        useMutation<Todo>({
          collection: 'todos',
          mode: 'immediate',
          mutationFn,
          retry: 2,
          retryDelay: 1,
        }),
      { wrapper },
    )

    await act(async () => {
      await expect(result.current.mutateAsync({ title: 'x' })).rejects.toThrow('down')
    })

    expect(mutationFn).toHaveBeenCalledTimes(3)
    expect(result.current.failureReason?.message).toBe('down')
  })

  it('leaves optimistic writes to the drain', async () => {
    const { wrapper, docsIn } = setup()

    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', url: '/api/todos/:id', retry: 5 }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'x' })
    })

    // The local commit cannot fail over the network, and the queued request's
    // backoff lives on the document where it survives a reload.
    expect(docsIn('todos')).toHaveLength(1)
    expect(result.current.status).toBe('success')
  })
})

describe('networkMode', () => {
  it('pauses instead of failing while offline, then goes on reconnect', async () => {
    const { wrapper, docsIn } = setup()
    vi.stubGlobal('navigator', { onLine: false })
    const mutationFn = vi.fn().mockResolvedValue({ _id: 'a', title: 'x' })

    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'immediate', mutationFn }),
      { wrapper },
    )

    let settled = false
    const pending = result.current.mutateAsync({ title: 'x' }).then(() => {
      settled = true
    })

    await waitFor(() => expect(result.current.isPaused).toBe(true))
    // A write made in a tunnel should wait for the connection, not be thrown
    // away. It is still pending, not failed.
    expect(mutationFn).not.toHaveBeenCalled()
    expect(result.current.status).toBe('pending')
    expect(settled).toBe(false)

    vi.stubGlobal('navigator', { onLine: true })
    await act(async () => {
      window.dispatchEvent(new Event('online'))
      await pending
    })

    await waitFor(() => expect(docsIn('todos')).toHaveLength(1))
    expect(settled).toBe(true)
    expect(result.current.isPaused).toBe(false)
  })

  it('attempts regardless under always', async () => {
    const { wrapper } = setup()
    vi.stubGlobal('navigator', { onLine: false })
    const mutationFn = vi.fn().mockResolvedValue({ _id: 'a', title: 'x' })

    const { result } = renderHook(
      () =>
        useMutation<Todo>({
          collection: 'todos',
          mode: 'immediate',
          mutationFn,
          networkMode: 'always',
        }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'x' })
    })

    // The setting for a write that talks to something local.
    expect(mutationFn).toHaveBeenCalledTimes(1)
  })
})

describe('scope', () => {
  it('runs mutations sharing an id one at a time, in call order', async () => {
    const { wrapper } = setup()
    const order: string[] = []
    const release: Array<() => void> = []
    const mutationFn = vi.fn().mockImplementation(
      (vars: { title: string }) =>
        new Promise((resolve) => {
          order.push(`start:${vars.title}`)
          release.push(() => {
            order.push(`end:${vars.title}`)
            resolve({ _id: vars.title, title: vars.title })
          })
        }),
    )

    const { result } = renderHook(
      () =>
        useMutation<Todo>({
          collection: 'todos',
          mode: 'immediate',
          mutationFn,
          scope: { id: 'checkout' },
        }),
      { wrapper },
    )

    void result.current.mutateAsync({ title: 'first' })
    void result.current.mutateAsync({ title: 'second' })

    await waitFor(() => expect(order).toEqual(['start:first']))
    // The second has not started: without this, two rapid submits race and the
    // later response can land first.
    expect(mutationFn).toHaveBeenCalledTimes(1)

    await act(async () => {
      release[0]()
    })
    await waitFor(() => expect(order).toEqual(['start:first', 'end:first', 'start:second']))
    await act(async () => {
      release[1]()
    })
  })

  it('does not let a failure stall what is queued behind it', async () => {
    const { wrapper } = setup()
    const mutationFn = vi
      .fn()
      .mockRejectedValueOnce(new Error('first failed'))
      .mockResolvedValue({ _id: 'b', title: 'second' })

    const { result } = renderHook(
      () =>
        useMutation<Todo>({
          collection: 'todos',
          mode: 'immediate',
          mutationFn,
          scope: { id: 'checkout' },
        }),
      { wrapper },
    )

    await act(async () => {
      const first = result.current.mutateAsync({ title: 'first' }).catch(() => 'failed')
      const second = result.current.mutateAsync({ title: 'second' })
      await expect(first).resolves.toBe('failed')
      await expect(second).resolves.toMatchObject({ title: 'second' })
    })

    expect(mutationFn).toHaveBeenCalledTimes(2)
  })

  it('runs in parallel without a scope', async () => {
    const { wrapper } = setup()
    let inFlight = 0
    let peak = 0
    const mutationFn = vi.fn().mockImplementation(async (vars: { title: string }) => {
      inFlight += 1
      peak = Math.max(peak, inFlight)
      await new Promise((r) => setTimeout(r, 5))
      inFlight -= 1
      return { _id: vars.title, title: vars.title }
    })

    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'immediate', mutationFn }),
      { wrapper },
    )

    await act(async () => {
      await Promise.all([
        result.current.mutateAsync({ title: 'a' }),
        result.current.mutateAsync({ title: 'b' }),
      ])
    })

    // Parallel is the default, as in TanStack — serialising is opt-in.
    expect(peak).toBe(2)
  })
})

describe('throwOnError', () => {
  class Boundary extends React.Component<
    { children: React.ReactNode; onCatch: (error: Error) => void },
    { caught: boolean }
  > {
    state = { caught: false }
    static getDerivedStateFromError() {
      return { caught: true }
    }
    componentDidCatch(error: Error) {
      this.props.onCatch(error)
    }
    render() {
      return this.state.caught ? <p>caught</p> : this.props.children
    }
  }

  it('rethrows during render so an error boundary can catch it', async () => {
    const { wrapper: Wrapper } = setup()
    const onCatch = vi.fn()
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})
    const mutationFn = vi.fn().mockRejectedValue(new Error('boom'))

    function Form() {
      const { mutate } = useMutation<Todo>({
        collection: 'todos',
        mode: 'immediate',
        mutationFn,
        throwOnError: true,
      })
      return (
        <button type="button" onClick={() => mutate({ title: 'x' })}>
          Save
        </button>
      )
    }

    render(
      <Wrapper>
        <Boundary onCatch={onCatch}>
          <Form />
        </Boundary>
      </Wrapper>,
    )

    await act(async () => {
      screen.getByRole('button').click()
    })

    await waitFor(() => expect(onCatch).toHaveBeenCalled())
    expect((onCatch.mock.calls[0][0] as Error).message).toBe('boom')
    spy.mockRestore()
  })
})
