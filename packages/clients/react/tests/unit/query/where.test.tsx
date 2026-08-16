/**
 * `where` — narrowing a query's result set locally.
 *
 * Not a parity feature. It is the one that argues *for* this library rather
 * than merely lowering the cost of trying it: one fetch, many views, and a
 * predicate that keeps working when the network stops.
 *
 * The behaviours worth pinning are the ones that would otherwise be guesses:
 * that `where` filters *within* the server's result set rather than replacing
 * it, that it stays out of the query key, and that it never reaches the record.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { renderHook, waitFor } from '@testing-library/react'
import type { Document } from 'taladb'
import { TalaDBProvider } from '../../../src/context'
import { QueryProvider } from '../../../src/query/context'
import { useQuery } from '../../../src/query/useQuery'
import { openQueryCollection, readQueryRecord } from '../../../src/query/queries'
import { createFakeDB } from '../../helpers/fakeQueryDB'
import { id } from '../../helpers/docId'

interface Todo extends Document {
  _id?: string
  title: string
  done: boolean
}

const todo = (label: string, title: string, done = false): Todo => ({ _id: id(label), title, done })

function setup() {
  const { db, docsIn } = createFakeDB()
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db}>
      <QueryProvider defaultOptions={{ queries: { retry: false } }}>{children}</QueryProvider>
    </TalaDBProvider>
  )
  return { wrapper, docsIn, db }
}

const three = () => [todo('a', 'one'), todo('b', 'two', true), todo('c', 'three')]

describe('where', () => {
  it('narrows the result set without changing what was fetched', async () => {
    const { wrapper, docsIn } = setup()
    const queryFn = vi.fn().mockResolvedValue(three())

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, where: { done: false } }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    expect(result.current.data?.map((t) => t.title)).toEqual(['one', 'three'])
    // Everything the server sent is still stored — `where` is a view, not a
    // filter on what gets persisted.
    expect(docsIn('todos')).toHaveLength(3)
  })

  it('keeps server order within the narrowed set', async () => {
    const { wrapper } = setup()
    const queryFn = vi
      .fn()
      .mockResolvedValue([todo('c', 'third'), todo('b', 'skipped', true), todo('a', 'first')])

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, where: { done: false } }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    // `where` removes entries; it does not reorder them.
    expect(result.current.data?.map((t) => t.title)).toEqual(['third', 'first'])
  })

  it('reports an empty narrowing as success, not pending', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue(three())

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, where: { title: 'nothing' } }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    // Narrowing to nothing is a legitimate answer. Reporting `pending` would
    // put a spinner over a question that has been answered.
    expect(result.current.data).toEqual([])
    expect(result.current.isPending).toBe(false)
  })

  it('serves many narrowings from one fetch', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue(three())
    const key = ['todos']

    const all = renderHook(() => useQuery<Todo>({ queryKey: key, queryFn }), { wrapper })
    await waitFor(() => expect(all.result.current.status).toBe('success'))

    const active = renderHook(
      () => useQuery<Todo>({ queryKey: key, queryFn, where: { done: false } }),
      { wrapper },
    )
    const done = renderHook(
      () => useQuery<Todo>({ queryKey: key, queryFn, where: { done: true } }),
      { wrapper },
    )
    await waitFor(() => expect(active.result.current.status).toBe('success'))
    await waitFor(() => expect(done.result.current.status).toBe('success'))

    // The whole point: `where` is outside the key, so All / Active / Done are
    // three components over a single request.
    expect(queryFn).toHaveBeenCalledTimes(1)
    expect(all.result.current.data).toHaveLength(3)
    expect(active.result.current.data).toHaveLength(2)
    expect(done.result.current.data).toHaveLength(1)
  })

  it('leaves the stored record untouched', async () => {
    const { wrapper, db } = setup()
    const queryFn = vi.fn().mockResolvedValue(three())

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, where: { done: true } }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.data).toHaveLength(1))

    // Membership is what the server said. If `where` narrowed the record, a
    // refetch would fight the filter and a locally-excluded document would be
    // indistinguishable from one the server deleted.
    const queries = await openQueryCollection(db)
    const record = await readQueryRecord(queries, 'todos', ['todos'])
    expect(record?.ids).toEqual([id('a'), id('b'), id('c')])
  })

  it('supports operators, not just equality', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue(three())

    const { result } = renderHook(
      () =>
        useQuery<Todo>({
          queryKey: ['todos'],
          queryFn,
          where: { title: { $in: ['one', 'three'] } },
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    expect(result.current.data?.map((t) => t.title)).toEqual(['one', 'three'])
  })

  it('narrows a single-document query to nothing when it does not match', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue(todo('a', 'one', true))

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos', 'a'], queryFn, where: { done: false } }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.isFetched).toBe(true))

    expect(result.current.data).toBeUndefined()
  })

  it('applies before assemble, so an envelope total stays the server’s', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue({ items: three(), total: 3 })

    const { result } = renderHook(
      () =>
        useQuery({
          queryKey: ['todos'],
          queryFn,
          documents: (raw: { items: Todo[]; total: number }) => raw.items,
          assemble: (items: Todo[], raw: { total: number }) => ({ ...raw, items }),
          where: { done: false },
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    expect(result.current.data?.items.map((t) => t.title)).toEqual(['one', 'three'])
    // `total` is the server's count of the whole result set, and a local
    // narrowing has no business rewriting it.
    expect(result.current.data?.total).toBe(3)
  })

  it('does not refetch when only the predicate changes', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue(three())

    const { result, rerender } = renderHook(
      ({ done }: { done: boolean }) =>
        useQuery<Todo>({ queryKey: ['todos'], queryFn, where: { done } }),
      { wrapper, initialProps: { done: false } },
    )
    await waitFor(() => expect(result.current.data).toHaveLength(2))

    rerender({ done: true })

    await waitFor(() => expect(result.current.data).toHaveLength(1))
    expect(queryFn).toHaveBeenCalledTimes(1)
  })
})
