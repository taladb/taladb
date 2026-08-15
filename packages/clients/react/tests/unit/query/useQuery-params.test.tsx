/**
 * `params` inside `useQuery`.
 *
 * The two behaviours that justify translating parameters at all — rather than
 * just documenting `where` — are at the bottom: an optimistic write that moves
 * a document out of the filter, and a cold key answered from the collection
 * while offline.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { renderHook, waitFor } from '@testing-library/react'
import type { Document } from 'taladb'
import { TalaDBProvider } from '../../../src/context'
import { QueryProvider } from '../../../src/query/context'
import { useQuery } from '../../../src/query/useQuery'
import { defineParams, eq, gte, shape } from '../../../src/query/params'
import { createFakeDB } from '../../helpers/fakeQueryDB'

interface Book extends Document {
  _id?: string
  title: string
  category: string
  price: number
}

const book = (id: string, title: string, category = 'book', price = 10): Book => ({
  _id: id,
  title,
  category,
  price,
})

const bookParams = defineParams({
  category: eq('category'),
  minPrice: gte('price'),
  page: shape(),
})

function setup() {
  const { db, docsIn } = createFakeDB()
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db}>
      <QueryProvider defaultOptions={{ queries: { retry: false } }}>{children}</QueryProvider>
    </TalaDBProvider>
  )
  return { wrapper, docsIn, db }
}

describe('params', () => {
  it('hands the bound parameters to queryFn', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([book('a', 'Dune')])

    const { result } = renderHook(
      () =>
        useQuery<Book>({
          queryKey: ['books'],
          params: bookParams({ category: 'book', page: 2 }),
          queryFn,
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    // The library never builds the URL — it hands back the declaration so the
    // caller can.
    const context = queryFn.mock.calls[0][0]
    expect(context.params.toSearchParams().toString()).toBe('category=book&page=2')
  })

  it('makes different parameters a different query', async () => {
    const { wrapper } = setup()
    const queryFn = vi
      .fn()
      .mockImplementation(async ({ params }) =>
        params.values.category === 'book' ? [book('a', 'Dune')] : [book('b', 'Pen', 'stationery')],
      )

    const first = renderHook(
      () =>
        useQuery<Book>({ queryKey: ['books'], params: bookParams({ category: 'book' }), queryFn }),
      { wrapper },
    )
    await waitFor(() => expect(first.result.current.status).toBe('success'))

    const second = renderHook(
      () =>
        useQuery<Book>({
          queryKey: ['books'],
          params: bookParams({ category: 'stationery' }),
          queryFn,
        }),
      { wrapper },
    )
    await waitFor(() => expect(second.result.current.status).toBe('success'))

    // Parameters are part of the query's identity: a different request and a
    // different result set.
    expect(queryFn).toHaveBeenCalledTimes(2)
    expect(first.result.current.data?.[0].title).toBe('Dune')
    expect(second.result.current.data?.[0].title).toBe('Pen')
  })

  it('accepts a plain object, with no local predicate', async () => {
    const { wrapper } = setup()
    // The server said these two belong to the query; an undeclared `category`
    // parameter must not second-guess it locally.
    const queryFn = vi.fn().mockResolvedValue([book('a', 'Dune'), book('b', 'Pen', 'stationery')])

    const { result } = renderHook(
      () => useQuery<Book>({ queryKey: ['books'], params: { category: 'book' }, queryFn }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    expect(result.current.data).toHaveLength(2)
  })

  it('narrows locally once the parameter is declared', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([book('a', 'Dune'), book('b', 'Pen', 'stationery')])

    const { result } = renderHook(
      () =>
        useQuery<Book>({ queryKey: ['books'], params: bookParams({ category: 'book' }), queryFn }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    // Same call shape, one import more — the upgrade is additive.
    expect(result.current.data?.map((b) => b.title)).toEqual(['Dune'])
  })

  it('ignores a response-shaping parameter locally', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([book('a', 'Dune'), book('b', 'Emma')])

    const { result } = renderHook(
      () => useQuery<Book>({ queryKey: ['books'], params: bookParams({ page: 2 }), queryFn }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    // `{ page: 2 }` as a predicate would match nothing and render an empty
    // list with `status: 'success'`.
    expect(result.current.data).toHaveLength(2)
  })

  it('drops a document edited out of the filter, without refetching', async () => {
    const { wrapper, db } = setup()
    const queryFn = vi.fn().mockResolvedValue([book('a', 'Dune'), book('b', 'Emma')])

    const { result } = renderHook(
      () =>
        useQuery<Book>({ queryKey: ['books'], params: bookParams({ category: 'book' }), queryFn }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.data).toHaveLength(2))

    await db.collection<Book>('books').updateOne({ _id: 'b' }, { $set: { category: 'stationery' } })

    // This is the reason to translate parameters at all. Against the raw id
    // list the document would linger until a refetch, because membership was
    // decided by the server before the edit existed.
    await waitFor(() => expect(result.current.data).toHaveLength(1))
    expect(result.current.data?.[0].title).toBe('Dune')
    expect(queryFn).toHaveBeenCalledTimes(1)
  })
})

describe("offline: 'collection'", () => {
  it('answers a cold key from the collection when the fetch fails', async () => {
    const { wrapper, db } = setup()
    // Cached by some earlier query, under a key this one has never fetched.
    const books = db.collection<Book>('books')
    await books.insert(book('a', 'Dune'))
    await books.insert(book('b', 'Pen', 'stationery'))

    const queryFn = vi.fn().mockRejectedValue(new Error('offline'))

    const { result } = renderHook(
      () =>
        useQuery<Book>({
          queryKey: ['books'],
          params: bookParams({ category: 'book' }),
          queryFn,
          offline: 'collection',
        }),
      { wrapper },
    )

    await waitFor(() => expect(result.current.fromCollection).toBe(true))
    // A spinner over a database that holds matching books helps nobody.
    expect(result.current.data?.map((b) => b.title)).toEqual(['Dune'])
  })

  it('stays empty-handed under the default strict mode', async () => {
    const { wrapper, db } = setup()
    await db.collection<Book>('books').insert(book('a', 'Dune'))
    const queryFn = vi.fn().mockRejectedValue(new Error('offline'))

    const { result } = renderHook(
      () =>
        useQuery<Book>({
          queryKey: ['books'],
          params: bookParams({ category: 'book' }),
          queryFn,
        }),
      { wrapper },
    )

    await waitFor(() => expect(result.current.status).toBe('error'))
    // `strict` keeps the guarantee that `data` is exactly what the server
    // returned. No record, no answer.
    expect(result.current.fromCollection).toBe(false)
    expect(result.current.data).toBeUndefined()
  })

  it('warns that a local query has no page 2', async () => {
    const { wrapper, db } = setup()
    await db.collection<Book>('books').insert(book('a', 'Dune'))
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const queryFn = vi.fn().mockRejectedValue(new Error('offline'))

    const { result } = renderHook(
      () =>
        useQuery<Book>({
          queryKey: ['books'],
          params: bookParams({ category: 'book', page: 2 }),
          queryFn,
          offline: 'collection',
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.fromCollection).toBe(true))

    const notice = warn.mock.calls.map((c) => String(c[0])).find((m) => m.includes('page'))
    expect(notice).toMatch(/only the server can honour/)

    warn.mockRestore()
  })

  it('a successful fetch supersedes the local fallback', async () => {
    const { wrapper, db } = setup()
    await db.collection<Book>('books').insert(book('a', 'Dune'))
    const queryFn = vi
      .fn()
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValue([book('c', 'Authoritative')])

    const { result } = renderHook(
      () =>
        useQuery<Book>({
          queryKey: ['books'],
          params: bookParams({ category: 'book' }),
          queryFn,
          offline: 'collection',
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.fromCollection).toBe(true))

    await result.current.refetch()

    await waitFor(() => expect(result.current.fromCollection).toBe(false))
    expect(result.current.data?.map((b) => b.title)).toEqual(['Authoritative'])
  })
})
