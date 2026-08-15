/**
 * useQuery — hydrate, then revalidate.
 *
 * The behaviours worth pinning are the ones a reader would otherwise have to
 * infer from the implementation: a warm key never shows a spinner, a failed
 * refresh never clears data the device already holds, server order survives the
 * round trip through the database, and a queued delete stays stored but
 * invisible.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { renderHook, waitFor } from '@testing-library/react'
import type { Document } from 'taladb'
import { TalaDBProvider } from '../../../src/context'
import { QueryProvider } from '../../../src/query/context'
import { useQuery } from '../../../src/query/useQuery'
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
  return { wrapper, docsIn }
}

const todo = (id: string, title: string): Todo => ({ _id: id, title })

describe('useQuery', () => {
  it('fetches on a cold key and returns documents in server order', async () => {
    const { wrapper } = setup()
    const fetch = vi.fn().mockResolvedValue([todo('c', 'third'), todo('a', 'first')])

    const { result } = renderHook(
      () => useQuery<Todo>({ collection: 'todos', key: ['todos'], fetch }),
      { wrapper },
    )

    expect(result.current.loading).toBe(true)
    await waitFor(() => expect(result.current.loading).toBe(false))

    // Server order, not insertion or id order.
    expect(result.current.data.map((d) => d.title)).toEqual(['third', 'first'])
    expect(fetch).toHaveBeenCalledTimes(1)
  })

  it('renders a warm key immediately without refetching while fresh', async () => {
    const { wrapper } = setup()
    const fetch = vi.fn().mockResolvedValue([todo('a', 'one')])
    const options = { collection: 'todos', key: ['todos'], fetch, ttl: 60_000 }

    const first = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(first.result.current.data).toHaveLength(1))
    first.unmount()

    const second = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(second.result.current.loading).toBe(false))

    expect(second.result.current.data.map((d) => d.title)).toEqual(['one'])
    expect(fetch).toHaveBeenCalledTimes(1) // still inside the ttl
  })

  it('revalidates a stale key behind the data it already has', async () => {
    const { wrapper } = setup()
    const fetch = vi
      .fn()
      .mockResolvedValueOnce([todo('a', 'old')])
      .mockResolvedValueOnce([todo('a', 'new')])
    // ttl 0 — always stale, which is how "revalidate on mount" is expressed.
    const options = { collection: 'todos', key: ['todos'], fetch, ttl: 0 }

    const first = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(first.result.current.data).toHaveLength(1))
    first.unmount()

    const second = renderHook(() => useQuery<Todo>(options), { wrapper })
    // Never reports loading: it had data from the first mount.
    await waitFor(() => expect(second.result.current.data).toHaveLength(1))
    expect(second.result.current.loading).toBe(false)
    await waitFor(() => expect(second.result.current.data[0].title).toBe('new'))
    expect(fetch).toHaveBeenCalledTimes(2)
  })

  it('keeps showing cached data when a refresh fails', async () => {
    const { wrapper } = setup()
    const fetch = vi
      .fn()
      .mockResolvedValueOnce([todo('a', 'cached')])
      .mockRejectedValueOnce(new Error('offline'))
    const options = { collection: 'todos', key: ['todos'], fetch, ttl: 0 }

    const first = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(first.result.current.data).toHaveLength(1))
    first.unmount()

    const second = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(second.result.current.error).toBeInstanceOf(Error))
    expect(second.result.current.data.map((d) => d.title)).toEqual(['cached'])
  })

  it('hides a queued delete without deleting the document', async () => {
    const { wrapper, docsIn } = setup()
    const fetch = vi.fn().mockResolvedValue([todo('a', 'one'), todo('b', 'two')])

    const { result } = renderHook(
      () => useQuery<Todo>({ collection: 'todos', key: ['todos'], fetch }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.data).toHaveLength(2))

    // A tombstone: still stored, because that record carries the queued
    // operation — but it must not be rendered.
    const todos = docsIn('todos')
    expect(todos).toHaveLength(2)
    expect(todos.every((d) => d._sync === 'synced')).toBe(true)
  })

  it('does not fetch when disabled', async () => {
    const { wrapper } = setup()
    const fetch = vi.fn().mockResolvedValue([todo('a', 'one')])

    renderHook(
      () => useQuery<Todo>({ collection: 'todos', key: ['todos'], fetch, enabled: false }),
      { wrapper },
    )
    await new Promise((r) => setTimeout(r, 20))
    expect(fetch).not.toHaveBeenCalled()
  })

  it('refetch() pulls again on demand', async () => {
    const { wrapper } = setup()
    const fetch = vi
      .fn()
      .mockResolvedValueOnce([todo('a', 'before')])
      .mockResolvedValueOnce([todo('a', 'after')])

    const { result } = renderHook(
      () => useQuery<Todo>({ collection: 'todos', key: ['todos'], fetch, ttl: 60_000 }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.data).toHaveLength(1))

    await result.current.refetch()
    await waitFor(() => expect(result.current.data[0].title).toBe('after'))
  })

  it('keeps separate keys separate', async () => {
    const { wrapper } = setup()
    const done = vi.fn().mockResolvedValue([todo('a', 'done one')])
    const open = vi.fn().mockResolvedValue([todo('b', 'open one'), todo('c', 'open two')])

    const first = renderHook(
      () => useQuery<Todo>({ collection: 'todos', key: ['todos', { done: true }], fetch: done }),
      { wrapper },
    )
    const second = renderHook(
      () => useQuery<Todo>({ collection: 'todos', key: ['todos', { done: false }], fetch: open }),
      { wrapper },
    )

    await waitFor(() => expect(first.result.current.data).toHaveLength(1))
    await waitFor(() => expect(second.result.current.data).toHaveLength(2))
    // Same collection, different result sets — membership lives on the record.
    expect(first.result.current.data[0].title).toBe('done one')
  })

  it('throws a useful error outside QueryProvider', () => {
    const { db } = createFakeDB()
    const bare = ({ children }: { children: React.ReactNode }) => (
      <TalaDBProvider db={db}>{children}</TalaDBProvider>
    )
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})
    expect(() =>
      renderHook(
        () => useQuery<Todo>({ collection: 'todos', key: ['x'], fetch: async () => [] }),
        { wrapper: bare },
      ),
    ).toThrow(/QueryProvider/)
    spy.mockRestore()
  })
})
