/**
 * When the queue is flushed.
 *
 * The interval is the fallback, not the mechanism: the moment a queue most
 * needs draining — the tab being hidden or closed — is the moment the timer is
 * least likely to be about to fire.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { act, renderHook, waitFor } from '@testing-library/react'
import type { Document } from 'taladb'
import { TalaDBProvider } from '../../../src/context'
import { QueryProvider } from '../../../src/query/context'
import { useMutation } from '../../../src/query/useMutation'
import type { DrainOptions } from '../../../src/query/types'
import { createFakeDB } from '../../helpers/fakeQueryDB'

interface Todo extends Document {
  _id?: string
  title: string
}

function setup(drain?: DrainOptions) {
  const { db, docsIn } = createFakeDB()
  const sent: string[] = []
  const fetch = vi.fn(async (url: string, init?: RequestInit) => {
    sent.push(String(init?.method ?? 'GET'))
    return new Response(JSON.stringify({ ok: true }), { status: 200 })
  })
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db}>
      <QueryProvider drain={drain} fetch={fetch as never}>
        {children}
      </QueryProvider>
    </TalaDBProvider>
  )
  return { wrapper, docsIn, fetch, sent }
}

const hidden = (state: 'hidden' | 'visible') => {
  Object.defineProperty(document, 'visibilityState', { value: state, configurable: true })
}

describe('flush on hide', () => {
  it('drains when the tab is hidden', async () => {
    const { wrapper, fetch } = setup({ policy: 'interval', intervalMs: 60_000 })
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'queued', url: '/api/todos/:id' }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'x' })
    })
    // Nothing yet: a 60-second interval is not about to fire.
    expect(fetch).not.toHaveBeenCalled()

    hidden('hidden')
    await act(async () => {
      document.dispatchEvent(new Event('visibilitychange'))
    })

    // The page is still alive here, so this is a normal drain that completes.
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1))
    hidden('visible')
  })

  it('ignores becoming visible again', async () => {
    const { wrapper, fetch } = setup({ policy: 'interval', intervalMs: 60_000 })
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'queued', url: '/api/todos/:id' }),
      { wrapper },
    )
    await act(async () => {
      await result.current.mutateAsync({ title: 'x' })
    })

    hidden('visible')
    await act(async () => {
      document.dispatchEvent(new Event('visibilitychange'))
    })
    await new Promise((r) => setTimeout(r, 20))

    // `visibilitychange` fires on the way in as well as the way out.
    expect(fetch).not.toHaveBeenCalled()
  })

  it('sends with keepalive on pagehide', async () => {
    const { wrapper, fetch } = setup({ policy: 'interval', intervalMs: 60_000 })
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'queued', url: '/api/todos/:id' }),
      { wrapper },
    )
    await act(async () => {
      await result.current.mutateAsync({ title: 'x' })
    })

    await act(async () => {
      window.dispatchEvent(new Event('pagehide'))
    })

    // An ordinary request is cancelled the moment the document goes away, so
    // this one has to ask to outlive it. Best-effort by construction: browsers
    // cap keepalive bodies at 64 KB in total.
    await waitFor(() => expect(fetch).toHaveBeenCalled())
    const init = fetch.mock.calls[0][1] as RequestInit
    expect(init.keepalive).toBe(true)
  })

  it('does not use keepalive for an ordinary drain', async () => {
    const { wrapper, fetch } = setup()
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', url: '/api/todos/:id' }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'x' })
    })

    await waitFor(() => expect(fetch).toHaveBeenCalled())
    // The 64 KB cap is a real cost, and a live page does not need paying it.
    expect((fetch.mock.calls[0][1] as RequestInit).keepalive).toBe(false)
  })

  it('can be turned off', async () => {
    const { wrapper, fetch } = setup({
      policy: 'interval',
      intervalMs: 60_000,
      flushOnHide: false,
    })
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'queued', url: '/api/todos/:id' }),
      { wrapper },
    )
    await act(async () => {
      await result.current.mutateAsync({ title: 'x' })
    })

    hidden('hidden')
    await act(async () => {
      document.dispatchEvent(new Event('visibilitychange'))
    })
    await new Promise((r) => setTimeout(r, 20))

    expect(fetch).not.toHaveBeenCalled()
    hidden('visible')
  })
})

describe('flush at a queue depth', () => {
  it('sends once enough writes have piled up under a long interval', async () => {
    const { wrapper, fetch } = setup({ policy: 'interval', intervalMs: 60_000, flushAt: 3 })
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'queued', url: '/api/todos/:id' }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'one' })
      await result.current.mutateAsync({ title: 'two' })
    })
    // Under the threshold: still accumulating.
    expect(fetch).not.toHaveBeenCalled()

    await act(async () => {
      await result.current.mutateAsync({ title: 'three' })
    })

    // The timer is the fallback; without this a burst would sit unsent for the
    // whole window and the queue would grow without bound between ticks.
    await waitFor(() => expect(fetch).toHaveBeenCalled())
  })

  it('respects a manual policy, which means what it says', async () => {
    const { wrapper, fetch } = setup({ policy: 'manual', flushAt: 2 })
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'queued', url: '/api/todos/:id' }),
      { wrapper },
    )

    await act(async () => {
      await result.current.mutateAsync({ title: 'one' })
      await result.current.mutateAsync({ title: 'two' })
      await result.current.mutateAsync({ title: 'three' })
    })
    await new Promise((r) => setTimeout(r, 20))

    expect(fetch).not.toHaveBeenCalled()
  })

  it('can be disabled', async () => {
    const { wrapper, fetch } = setup({ policy: 'interval', intervalMs: 60_000, flushAt: false })
    const { result } = renderHook(
      () => useMutation<Todo>({ collection: 'todos', mode: 'queued', url: '/api/todos/:id' }),
      { wrapper },
    )

    await act(async () => {
      for (const title of ['a', 'b', 'c', 'd']) {
        await result.current.mutateAsync({ title })
      }
    })
    await new Promise((r) => setTimeout(r, 20))

    expect(fetch).not.toHaveBeenCalled()
  })
})
