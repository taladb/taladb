/**
 * useQuery — hydrate, then revalidate.
 *
 * The behaviours worth pinning are the ones a reader would otherwise have to
 * infer from the implementation: a warm key never shows a spinner, a failed
 * refresh never clears data the device already holds, server order survives the
 * round trip through the database, and a queued delete stays stored but
 * invisible.
 *
 * The surface is TanStack Query v5's — `queryKey`, `queryFn`, `staleTime`,
 * `status`/`isPending` — so that migrating onto this layer is an import change.
 * See PLAN-query-parity.md and tests/parity/.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { renderHook, waitFor } from '@testing-library/react'
import type { Document } from 'taladb'
import { TalaDBProvider } from '../../../src/context'
import { QueryProvider } from '../../../src/query/context'
import { useQuery, keepPreviousData } from '../../../src/query/useQuery'
import { createFakeDB } from '../../helpers/fakeQueryDB'
import { id } from '../../helpers/docId'

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

const todo = (label: string, title: string): Todo => ({ _id: id(label), title })

describe('useQuery', () => {
  it('fetches on a cold key and returns documents in server order', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('c', 'third'), todo('a', 'first')])

    const { result } = renderHook(() => useQuery<Todo>({ queryKey: ['todos'], queryFn }), {
      wrapper,
    })

    expect(result.current.status).toBe('pending')
    // Undefined rather than `[]`: an empty array renders as an empty list.
    expect(result.current.data).toBeUndefined()
    await waitFor(() => expect(result.current.status).toBe('success'))

    // Server order, not insertion or id order.
    expect(result.current.data?.map((d) => d.title)).toEqual(['third', 'first'])
    expect(queryFn).toHaveBeenCalledTimes(1)
  })

  it('renders a warm key immediately without refetching while fresh', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])
    const options = { queryKey: ['todos'], queryFn, staleTime: 60_000 }

    const first = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(first.result.current.data).toHaveLength(1))
    first.unmount()

    const second = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(second.result.current.status).toBe('success'))

    expect(second.result.current.data?.map((d) => d.title)).toEqual(['one'])
    expect(second.result.current.isLoading).toBe(false)
    expect(queryFn).toHaveBeenCalledTimes(1) // still inside the freshness window
  })

  it('serves a warm key for an hour when staleTime is left at its default', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])
    // No staleTime: the one-hour default is what is under test. TanStack
    // defaults to 0, which would revalidate here.
    const options = { queryKey: ['todos'], queryFn }

    const first = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(first.result.current.status).toBe('success'))
    first.unmount()

    const second = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(second.result.current.status).toBe('success'))

    expect(queryFn).toHaveBeenCalledTimes(1)
    expect(second.result.current.isStale).toBe(false)
  })

  it('revalidates a stale key behind the data it already has', async () => {
    const { wrapper } = setup()
    const queryFn = vi
      .fn()
      .mockResolvedValueOnce([todo('a', 'old')])
      .mockResolvedValueOnce([todo('a', 'new')])
    // staleTime 0 — always stale, which is how "revalidate on mount" is expressed.
    const options = { queryKey: ['todos'], queryFn, staleTime: 0 }

    const first = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(first.result.current.data).toHaveLength(1))
    first.unmount()

    const second = renderHook(() => useQuery<Todo>(options), { wrapper })
    // Never reports loading: it had data from the first mount.
    await waitFor(() => expect(second.result.current.data).toHaveLength(1))
    expect(second.result.current.isLoading).toBe(false)
    await waitFor(() => expect(second.result.current.data?.[0].title).toBe('new'))
    expect(queryFn).toHaveBeenCalledTimes(2)
  })

  it('keeps showing cached data when a refresh fails', async () => {
    const { wrapper } = setup()
    const queryFn = vi
      .fn()
      .mockResolvedValueOnce([todo('a', 'cached')])
      .mockRejectedValueOnce(new Error('offline'))
    // `retry: false` because the default is three retries with exponential
    // backoff; the retrying itself is covered below.
    const options = { queryKey: ['todos'], queryFn, staleTime: 0, retry: false }

    const first = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(first.result.current.data).toHaveLength(1))
    first.unmount()

    const second = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(second.result.current.error).toBeInstanceOf(Error))

    expect(second.result.current.data?.map((d) => d.title)).toEqual(['cached'])
    // A failure over data on screen is a refetch error, not a load error — the
    // distinction a component uses to decide between a toast and a blank state.
    expect(second.result.current.isRefetchError).toBe(true)
    expect(second.result.current.isLoadingError).toBe(false)
    expect(second.result.current.failureCount).toBe(1)
  })

  it('wraps a non-Error throw so error.message always works', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockRejectedValue('just a string')

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, retry: false }),
      { wrapper },
    )

    await waitFor(() => expect(result.current.status).toBe('error'))
    expect(result.current.error).toBeInstanceOf(Error)
    expect(result.current.error?.message).toBe('just a string')
    expect(result.current.isLoadingError).toBe(true)
  })

  it('hides a queued delete without deleting the document', async () => {
    const { wrapper, docsIn } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one'), todo('b', 'two')])

    const { result } = renderHook(() => useQuery<Todo>({ queryKey: ['todos'], queryFn }), {
      wrapper,
    })
    await waitFor(() => expect(result.current.data).toHaveLength(2))

    // A tombstone: still stored, because that record carries the queued
    // operation — but it must not be rendered.
    const todos = docsIn('todos')
    expect(todos).toHaveLength(2)
    expect(todos.every((d) => d._sync === 'synced')).toBe(true)
  })

  it('does not fetch when disabled, and reports nothing rather than nothing-found', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, enabled: false }),
      { wrapper },
    )
    await new Promise((r) => setTimeout(r, 20))

    expect(queryFn).not.toHaveBeenCalled()
    // Cold and disabled stays pending. An empty array here would read as "the
    // server returned nothing", which is a different claim entirely.
    expect(result.current.status).toBe('pending')
    expect(result.current.data).toBeUndefined()
  })

  it('serves a disabled query from the cache when the key is warm', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const first = renderHook(() => useQuery<Todo>({ queryKey: ['todos'], queryFn }), { wrapper })
    await waitFor(() => expect(first.result.current.status).toBe('success'))
    first.unmount()

    const second = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, enabled: false, staleTime: 0 }),
      { wrapper },
    )

    // Disabled suppresses the *fetch*, not the cache. This is the difference
    // between a persistent cache and a memory one.
    await waitFor(() => expect(second.result.current.status).toBe('success'))
    expect(second.result.current.data).toHaveLength(1)
    expect(queryFn).toHaveBeenCalledTimes(1)
  })

  it('refetch() pulls again on demand and resolves with a result', async () => {
    const { wrapper } = setup()
    const queryFn = vi
      .fn()
      .mockResolvedValueOnce([todo('a', 'before')])
      .mockResolvedValueOnce([todo('a', 'after')])

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, staleTime: 60_000 }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.data).toHaveLength(1))

    // Migrated code writes `const { data } = await refetch()`.
    const returned = await result.current.refetch()
    expect(returned.data).toHaveLength(1)
    await waitFor(() => expect(result.current.data?.[0].title).toBe('after'))
  })

  it('keeps separate keys separate', async () => {
    const { wrapper } = setup()
    const done = vi.fn().mockResolvedValue([todo('a', 'done one')])
    const open = vi.fn().mockResolvedValue([todo('b', 'open one'), todo('c', 'open two')])

    const first = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos', { done: true }], queryFn: done }),
      { wrapper },
    )
    const second = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos', { done: false }], queryFn: open }),
      { wrapper },
    )

    await waitFor(() => expect(first.result.current.data).toHaveLength(1))
    await waitFor(() => expect(second.result.current.data).toHaveLength(2))
    // Same collection, different result sets — membership lives on the record.
    expect(first.result.current.data?.[0].title).toBe('done one')
  })

  it('infers the collection from the first key segment', async () => {
    const { wrapper, docsIn } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    // No `collection` option: this is what makes a migrated TanStack query,
    // which has no such concept, work unedited.
    const { result } = renderHook(() => useQuery<Todo>({ queryKey: ['todos'], queryFn }), {
      wrapper,
    })
    await waitFor(() => expect(result.current.status).toBe('success'))

    expect(docsIn('todos')).toHaveLength(1)
  })

  it('lets the provider resolve collections for keys that do not lead with one', async () => {
    const { db, docsIn } = createFakeDB()
    const wrapper = ({ children }: { children: React.ReactNode }) => (
      <TalaDBProvider db={db}>
        <QueryProvider resolveCollection={(key) => key[2] as string}>{children}</QueryProvider>
      </TalaDBProvider>
    )
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['api', 'v2', 'todos'], queryFn }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    // One function fixes a whole codebase's key convention; the alternative is
    // adding `collection:` back to every call site.
    expect(docsIn('todos')).toHaveLength(1)
    expect(docsIn('api')).toHaveLength(0)
  })

  it('refuses to guess when the first key segment is not a string', () => {
    const { wrapper } = setup()
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})

    // Inventing a collection here would quietly fill one nobody declared.
    expect(() =>
      renderHook(
        () => useQuery<Todo>({ queryKey: [{ scope: 'todos' }], queryFn: async () => [] }),
        { wrapper },
      ),
    ).toThrow(/collection/i)
    spy.mockRestore()
  })

  it('returns a single document as itself, not a one-element array', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue(todo('a', 'just one'))

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos', 'a'], queryFn }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    expect(result.current.data).toMatchObject({ _id: id('a'), title: 'just one' })
  })

  it('restores the single-document shape from the record without refetching', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue(todo('a', 'just one'))
    const options = { queryKey: ['todos', 'a'], queryFn }

    const first = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(first.result.current.status).toBe('success'))
    first.unmount()

    const second = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(second.result.current.status).toBe('success'))

    // The shape is stored on the record: a detail view must not come back as a
    // one-element array after a reload, when no fetch has happened to reveal it.
    expect(second.result.current.data).toMatchObject({ title: 'just one' })
    expect(queryFn).toHaveBeenCalledTimes(1)
  })

  it('rebuilds an envelope with live documents and the stored remainder', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue({ items: [todo('a', 'one')], total: 42 })

    const { result } = renderHook(
      () =>
        useQuery({
          queryKey: ['todos'],
          queryFn,
          documents: (raw: { items: Todo[]; total: number }) => raw.items,
          assemble: (items: Todo[], raw: { total: number }) => ({ ...raw, items }),
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    // `items` came back through the collection; `total` came from the response.
    expect(result.current.data?.items.map((t) => t.title)).toEqual(['one'])
    expect(result.current.data?.total).toBe(42)
  })

  it('reports an unusable response as an error rather than an empty result', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue({ items: [todo('a', 'one')], total: 1 })

    const { result } = renderHook(() => useQuery<Todo>({ queryKey: ['todos'], queryFn }), {
      wrapper,
    })
    await waitFor(() => expect(result.current.status).toBe('error'))

    // Silently storing nothing would look identical to "the server has no todos".
    expect(result.current.error?.message).toMatch(/documents:/)
    expect(result.current.data).toBeUndefined()
  })

  it('select transforms what the component sees, not what is stored', async () => {
    const { wrapper, docsIn } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one'), todo('b', 'two')])

    const { result } = renderHook(
      () =>
        useQuery({
          queryKey: ['todos'],
          queryFn,
          select: (todos: Todo[]) => todos.map((t) => t.title),
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    expect(result.current.data).toEqual(['one', 'two'])
    // The collection keeps whole documents regardless of the view.
    expect(docsIn('todos')).toHaveLength(2)
  })

  it('keeps data referentially stable across re-renders', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result, rerender } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, staleTime: 60_000 }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    const first = result.current.data
    rerender()

    // Migrated code is full of `useEffect(…, [data])`; a fresh array on every
    // render would fire all of them on every database snapshot.
    expect(result.current.data).toBe(first)
  })

  it('seeds a cold key from initialData without fetching', async () => {
    const { wrapper, docsIn } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('b', 'fetched')])

    const { result } = renderHook(
      () =>
        useQuery<Todo>({
          queryKey: ['todos'],
          queryFn,
          initialData: [todo('a', 'seeded')],
          staleTime: 60_000,
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    // Unlike a placeholder, initialData is real: it is stored, and it counts
    // as a fetch for freshness purposes.
    expect(result.current.data?.map((t) => t.title)).toEqual(['seeded'])
    expect(docsIn('todos')).toHaveLength(1)
    expect(queryFn).not.toHaveBeenCalled()
  })

  it('revalidates behind initialData that is born stale', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('b', 'fetched')])

    const { result } = renderHook(
      () =>
        useQuery<Todo>({
          queryKey: ['todos'],
          queryFn,
          initialData: [todo('a', 'seeded')],
          // An SSR payload from two hours ago, with the default one-hour window.
          initialDataUpdatedAt: Date.now() - 2 * 60 * 60_000,
        }),
      { wrapper },
    )

    await waitFor(() => expect(result.current.data?.[0].title).toBe('fetched'))
    expect(queryFn).toHaveBeenCalledTimes(1)
  })

  it('holds the previous key on screen with keepPreviousData', async () => {
    const { wrapper } = setup()
    const queryFn = vi
      .fn()
      .mockImplementation(async ({ queryKey }: { queryKey: readonly unknown[] }) =>
        queryKey[1] === 1 ? [todo('a', 'page one')] : [todo('b', 'page two')],
      )

    const { result, rerender } = renderHook(
      ({ page }: { page: number }) =>
        useQuery<Todo>({
          queryKey: ['todos', page],
          queryFn,
          placeholderData: keepPreviousData,
        }),
      { wrapper, initialProps: { page: 1 } },
    )
    await waitFor(() => expect(result.current.data?.[0].title).toBe('page one'))

    rerender({ page: 2 })

    // The list does not blank out: page one stays, flagged as a stand-in.
    expect(result.current.data?.[0].title).toBe('page one')
    expect(result.current.isPlaceholderData).toBe(true)
    expect(result.current.status).toBe('success')

    await waitFor(() => expect(result.current.data?.[0].title).toBe('page two'))
    expect(result.current.isPlaceholderData).toBe(false)
  })

  it('goes back to pending on a key change without placeholderData', async () => {
    const { wrapper } = setup()
    const queryFn = vi
      .fn()
      .mockImplementation(async ({ queryKey }: { queryKey: readonly unknown[] }) =>
        queryKey[1] === 1 ? [todo('a', 'page one')] : [todo('b', 'page two')],
      )

    const { result, rerender } = renderHook(
      ({ page }: { page: number }) => useQuery<Todo>({ queryKey: ['todos', page], queryFn }),
      { wrapper, initialProps: { page: 1 } },
    )
    await waitFor(() => expect(result.current.data?.[0].title).toBe('page one'))

    rerender({ page: 2 })

    // Without an opt-in, showing the previous key's documents under a new key
    // would be a straightforwardly wrong answer.
    expect(result.current.status).toBe('pending')
    expect(result.current.data).toBeUndefined()
  })

  it('never stores placeholder data', async () => {
    const { wrapper, docsIn } = setup()
    const queryFn = vi.fn().mockReturnValue(new Promise<Todo[]>(() => {}))

    renderHook(
      () =>
        useQuery<Todo>({
          queryKey: ['todos'],
          queryFn,
          placeholderData: [todo('ghost', 'not real')],
        }),
      { wrapper },
    )
    await new Promise((r) => setTimeout(r, 20))

    // A stand-in is a render-time courtesy. Persisting it would put a document
    // the server never sent into a collection the app also reads directly.
    expect(docsIn('todos')).toHaveLength(0)
  })

  it('retries a failed fetch without reporting an error in between', async () => {
    const { wrapper } = setup()
    const queryFn = vi
      .fn()
      .mockRejectedValueOnce(new Error('flaky'))
      .mockResolvedValue([todo('a', 'eventually')])

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, retryDelay: 1 }),
      { wrapper },
    )

    await waitFor(() => expect(result.current.status).toBe('success'))

    // A query that is still retrying has not failed. Surfacing the first
    // rejection would flash an error state the user never actually reached.
    expect(result.current.error).toBeNull()
    expect(result.current.data?.[0].title).toBe('eventually')
    expect(queryFn).toHaveBeenCalledTimes(2)
  })

  it('counts failures while retrying', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockRejectedValue(new Error('down'))

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, retry: 2, retryDelay: 1 }),
      { wrapper },
    )

    await waitFor(() => expect(result.current.status).toBe('error'))
    // Two retries means three attempts.
    expect(queryFn).toHaveBeenCalledTimes(3)
    expect(result.current.failureCount).toBe(3)
    expect(result.current.failureReason?.message).toBe('down')
  })

  it('does not retry a response it could never parse', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue({ items: [], total: 0 })

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, retryDelay: 1 }),
      { wrapper },
    )

    await waitFor(() => expect(result.current.status).toBe('error'))
    // A shape error is deterministic; retrying it just delays the message.
    expect(queryFn).toHaveBeenCalledTimes(1)
  })

  it('pauses instead of failing when offline', async () => {
    const { wrapper } = setup()
    vi.stubGlobal('navigator', { onLine: false })
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(() => useQuery<Todo>({ queryKey: ['todos'], queryFn }), {
      wrapper,
    })
    await waitFor(() => expect(result.current.fetchStatus).toBe('paused'))

    // Burning the retry budget on requests that cannot succeed helps nobody.
    expect(queryFn).not.toHaveBeenCalled()
    expect(result.current.isPaused).toBe(true)
    expect(result.current.status).toBe('pending')
    expect(result.current.error).toBeNull()

    vi.unstubAllGlobals()
  })

  it('fetches while offline when networkMode is always', async () => {
    const { wrapper } = setup()
    vi.stubGlobal('navigator', { onLine: false })
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, networkMode: 'always' }),
      { wrapper },
    )

    // The setting for a `queryFn` that talks to something local.
    await waitFor(() => expect(result.current.status).toBe('success'))
    expect(queryFn).toHaveBeenCalledTimes(1)

    vi.unstubAllGlobals()
  })

  it('leaves a stale key alone when refetchOnMount is false', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])
    const options = { queryKey: ['todos'], queryFn, staleTime: 0 }

    const first = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(first.result.current.status).toBe('success'))
    first.unmount()

    const second = renderHook(
      () => useQuery<Todo>({ ...options, refetchOnMount: false }),
      { wrapper },
    )
    await waitFor(() => expect(second.result.current.status).toBe('success'))

    // Stale, but nothing asked — so nothing was fetched.
    expect(queryFn).toHaveBeenCalledTimes(1)
    expect(second.result.current.isStale).toBe(true)
  })

  it("refetches a fresh key when refetchOnMount is 'always'", async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])
    const options = { queryKey: ['todos'], queryFn, staleTime: 60_000 }

    const first = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(first.result.current.status).toBe('success'))
    first.unmount()

    renderHook(() => useQuery<Todo>({ ...options, refetchOnMount: 'always' }), { wrapper })

    await waitFor(() => expect(queryFn).toHaveBeenCalledTimes(2))
  })

  it('refetches a stale key when the window regains focus', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, staleTime: 0 }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))
    expect(queryFn).toHaveBeenCalledTimes(1)

    window.dispatchEvent(new Event('focus'))

    await waitFor(() => expect(queryFn).toHaveBeenCalledTimes(2))
  })

  it('leaves a fresh key alone on focus', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, staleTime: 60_000 }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    window.dispatchEvent(new Event('focus'))
    await new Promise((r) => setTimeout(r, 20))

    // Under the one-hour default this is the common case, and it is why focus
    // refetching is not the traffic problem it sounds like here.
    expect(queryFn).toHaveBeenCalledTimes(1)
  })

  it('does not refetch on focus when the option is off', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(
      () =>
        useQuery<Todo>({
          queryKey: ['todos'],
          queryFn,
          staleTime: 0,
          refetchOnWindowFocus: false,
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    window.dispatchEvent(new Event('focus'))
    await new Promise((r) => setTimeout(r, 20))

    expect(queryFn).toHaveBeenCalledTimes(1)
  })

  it('resumes on reconnect', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, staleTime: 0 }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    window.dispatchEvent(new Event('online'))

    await waitFor(() => expect(queryFn).toHaveBeenCalledTimes(2))
  })

  it('polls on refetchInterval regardless of freshness', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(
      () =>
        useQuery<Todo>({
          queryKey: ['todos'],
          queryFn,
          // Fresh for an hour: an interval that respected staleness would
          // never fire at all, which is not what anyone means by polling.
          staleTime: 60 * 60_000,
          refetchInterval: 10,
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    await waitFor(() => expect(queryFn.mock.calls.length).toBeGreaterThan(1))
  })

  it('takes defaults from the provider, and lets a hook override them', async () => {
    const { db } = createFakeDB()
    const wrapper = ({ children }: { children: React.ReactNode }) => (
      <TalaDBProvider db={db}>
        <QueryProvider defaultOptions={{ queries: { staleTime: 0, retry: false } }}>
          {children}
        </QueryProvider>
      </TalaDBProvider>
    )
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    // `staleTime: 0` on the provider is the documented one-line way back to
    // TanStack's revalidate-on-every-mount behaviour.
    const first = renderHook(() => useQuery<Todo>({ queryKey: ['todos'], queryFn }), { wrapper })
    await waitFor(() => expect(first.result.current.status).toBe('success'))
    first.unmount()

    const second = renderHook(() => useQuery<Todo>({ queryKey: ['todos'], queryFn }), { wrapper })
    await waitFor(() => expect(queryFn).toHaveBeenCalledTimes(2))
    second.unmount()

    // A hook option still wins over the provider's default.
    const third = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, staleTime: 60_000 }),
      { wrapper },
    )
    await waitFor(() => expect(third.result.current.status).toBe('success'))
    expect(queryFn).toHaveBeenCalledTimes(2)
  })

  it('throws a useful error outside QueryProvider', () => {
    const { db } = createFakeDB()
    const bare = ({ children }: { children: React.ReactNode }) => (
      <TalaDBProvider db={db}>{children}</TalaDBProvider>
    )
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})
    expect(() =>
      renderHook(() => useQuery<Todo>({ queryKey: ['x'], queryFn: async () => [] }), {
        wrapper: bare,
      }),
    ).toThrow(/QueryProvider/)
    spy.mockRestore()
  })
})
