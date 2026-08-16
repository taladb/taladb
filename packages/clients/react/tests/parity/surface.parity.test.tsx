/**
 * The result object's shape, pinned against TanStack Query v5.
 *
 * A component migrating across reads fields off this object without checking
 * whether they exist, so a missing key is not a missing feature — it is
 * `undefined` flowing into a render and a blank screen with no error. That
 * makes the key set worth asserting directly, separately from any behaviour.
 *
 * Expected to fail until PLAN-query-parity.md P1–P2 land. See ./README.md for
 * how to read a failure.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { renderHook, waitFor } from '@testing-library/react'
import { TalaDBProvider } from '../../src/context'
import { QueryProvider } from '../../src/query/context'
import { useQuery } from '../../src/query/useQuery'
import { createFakeDB } from '../helpers/fakeQueryDB'
import type { Todo } from './fixtures/todo'
import { id as docId } from '../helpers/docId'

/**
 * Every field TanStack v5's `UseQueryResult` documents as stable.
 *
 * Deliberately excluded, and not by oversight:
 * - `isInitialLoading` — deprecated in v5; new code will not reach for it.
 * - `promise` — behind `experimental_prefetchInRender`.
 * - `isEnabled` — added mid-v5; too version-dependent to hold a port to.
 */
const REQUIRED_RESULT_KEYS = [
  'data',
  'dataUpdatedAt',
  'error',
  'errorUpdatedAt',
  'errorUpdateCount',
  'failureCount',
  'failureReason',
  'fetchStatus',
  'isError',
  'isFetched',
  'isFetchedAfterMount',
  'isFetching',
  'isLoading',
  'isLoadingError',
  'isPaused',
  'isPending',
  'isPlaceholderData',
  'isRefetchError',
  'isRefetching',
  'isStale',
  'isSuccess',
  'refetch',
  'status',
] as const

function setup() {
  const { db, docsIn } = createFakeDB()
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db}>
      <QueryProvider defaultOptions={{ queries: { retry: false } }}>{children}</QueryProvider>
    </TalaDBProvider>
  )
  return { wrapper, docsIn }
}

const todo = (id: string, title: string): Todo => ({
  _id: docId(id),
  title,
  done: false,
  dueAt: null,
})

// The options every test here passes are the *migrated* ones — `queryKey` and
// `queryFn`, no `collection`. Cast at the call site rather than in the helper so
// each failure points at the option it is actually about.
const migrated = (options: Record<string, unknown>) =>
  useQuery<Todo>(options as never) as unknown as Record<string, unknown>

describe('result shape', () => {
  it('exposes every stable field of a TanStack v5 query result', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'first')])

    const { result } = renderHook(() => migrated({ queryKey: ['todos'], queryFn }), { wrapper })
    await waitFor(() => expect(result.current.status).toBe('success'))

    const missing = REQUIRED_RESULT_KEYS.filter((key) => !(key in result.current))
    expect(missing, `missing from the result object: ${missing.join(', ')}`).toEqual([])
  })

  it('reports data as undefined while pending, never as an empty array', async () => {
    const { wrapper } = setup()
    // Never resolves: the query stays pending for the life of the assertion.
    const queryFn = vi.fn().mockReturnValue(new Promise<Todo[]>(() => {}))

    const { result } = renderHook(() => migrated({ queryKey: ['todos'], queryFn }), { wrapper })

    // `if (!data) return <Skeleton/>` is the most common line in migrated code.
    // An empty array is truthy, so `[]` here renders an empty list forever.
    expect(result.current.data).toBeUndefined()
    expect(result.current.status).toBe('pending')
    expect(result.current.isPending).toBe(true)
    expect(result.current.isLoading).toBe(true)
  })

  it('surfaces a failure as an Error, with data left alone', async () => {
    const { wrapper } = setup()
    const queryFn = vi
      .fn()
      .mockResolvedValueOnce([todo('a', 'kept')])
      .mockRejectedValue(new Error('network down'))

    const { result } = renderHook(
      () => migrated({ queryKey: ['todos'], queryFn, staleTime: 0, retry: false }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    await (result.current.refetch as () => Promise<unknown>)()
    await waitFor(() => expect(result.current.isError).toBe(true))

    // `error.message` is everywhere in migrated code; `unknown` does not compile.
    expect(result.current.error).toBeInstanceOf(Error)
    expect((result.current.error as Error).message).toBe('network down')
    // A failed refetch must not clear what the device already holds.
    expect(result.current.data).toHaveLength(1)
    expect(result.current.isRefetchError).toBe(true)
    expect(result.current.isLoadingError).toBe(false)
  })

  it('distinguishes isStale (past staleTime) from isFetching (in flight)', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'first')])

    const { result } = renderHook(
      () => migrated({ queryKey: ['todos'], queryFn, staleTime: 60_000 }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    // Settled and inside its freshness window: neither stale nor fetching.
    expect(result.current.isFetching).toBe(false)
    expect(result.current.isStale).toBe(false)
    expect(result.current.fetchStatus).toBe('idle')
  })

  it('resolves refetch with the result rather than void', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'first')])

    const { result } = renderHook(() => migrated({ queryKey: ['todos'], queryFn }), { wrapper })
    await waitFor(() => expect(result.current.status).toBe('success'))

    // Migrated code writes `const { data } = await refetch()`.
    const returned = await (result.current.refetch as () => Promise<Record<string, unknown>>)()
    expect(returned?.data).toHaveLength(1)
  })
})

describe('defaults', () => {
  it('infers the collection from the first key segment', async () => {
    const { wrapper, docsIn } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'first')])

    const { result } = renderHook(() => migrated({ queryKey: ['todos'], queryFn }), { wrapper })
    await waitFor(() => expect(result.current.status).toBe('success'))

    // The option that decides where documents physically land (plan §2.2).
    expect(docsIn('todos')).toHaveLength(1)
  })

  it('refuses to guess when the first key segment is not a collection name', () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([])

    // Throwing beats inventing an `api` collection and quietly filling it.
    expect(() =>
      renderHook(() => migrated({ queryKey: [{ scope: 'todos' }], queryFn }), { wrapper }),
    ).toThrow(/collection/i)
  })

  it('serves a warm key from the device for an hour without fetching', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'first')])
    // No staleTime: the default is what is under test (plan §2.4).
    const options = { queryKey: ['todos'], queryFn }

    const first = renderHook(() => migrated(options), { wrapper })
    await waitFor(() => expect(first.result.current.status).toBe('success'))
    first.unmount()

    const second = renderHook(() => migrated(options), { wrapper })
    await waitFor(() => expect(second.result.current.status).toBe('success'))

    expect(second.result.current.data).toHaveLength(1)
    expect(queryFn).toHaveBeenCalledTimes(1)
    expect(second.result.current.isStale).toBe(false)
  })
})

describe('the residue', () => {
  it('fails loudly when queryFn returns something other than documents', async () => {
    const { wrapper } = setup()
    // The shape no rename can fix: our `data` is rebuilt from the collection,
    // so the response has to reduce to documents (plan §3.3).
    const queryFn = vi.fn().mockResolvedValue({ items: [todo('a', 'first')], total: 1 })

    const { result } = renderHook(() => migrated({ queryKey: ['todos'], queryFn }), { wrapper })
    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error))

    const message = (result.current.error as Error).message
    // A codemod cannot fix this case, so the runtime has to explain it.
    expect(message).toMatch(/todos/)
    expect(message).toMatch(/documents/)
    expect(message).toMatch(/assemble/)
  })

  it('accepts an envelope once documents and assemble are supplied', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue({ items: [todo('a', 'first')], total: 42 })

    const { result } = renderHook(
      () =>
        migrated({
          queryKey: ['todos'],
          queryFn,
          documents: (raw: { items: Todo[] }) => raw.items,
          assemble: (docs: Todo[], raw: { total: number }) => ({ ...raw, items: docs }),
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    const data = result.current.data as { items: Todo[]; total: number }
    expect(data.total).toBe(42)
    // `items` stays live — it is resolved through the collection, not the response.
    expect(data.items.map((t) => t.title)).toEqual(['first'])
  })
})
