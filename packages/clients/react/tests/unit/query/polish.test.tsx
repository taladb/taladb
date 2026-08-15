/**
 * The last of the surface: error boundaries, key hashing, forgetting a result
 * set, and the options this layer accepts without acting on.
 *
 * The accept-and-ignore tests are the ones worth reading. A drop-in that throws
 * on an unknown option is not a drop-in, but silently ignoring one is a trap —
 * so each is pinned to warn exactly once, with a reason.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import React from 'react'
import { cleanup, render, renderHook, screen, waitFor } from '@testing-library/react'
import type { Document } from 'taladb'
import { TalaDBProvider } from '../../../src/context'
import { QueryProvider } from '../../../src/query/context'
import { useQuery, resetForgetTimers } from '../../../src/query/useQuery'
import { resetWarnings } from '../../../src/query/dev'
import { resetInflight } from '../../../src/query/inflight'
import { readQueryRecord } from '../../../src/query/queries'
import { openQueryCollection } from '../../../src/query/queries'
import { createFakeDB } from '../../helpers/fakeQueryDB'

interface Todo extends Document {
  _id?: string
  title: string
}

const todo = (id: string, title: string): Todo => ({ _id: id, title })

function setup() {
  const { db, docsIn } = createFakeDB()
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db}>
      <QueryProvider defaultOptions={{ queries: { retry: false } }}>{children}</QueryProvider>
    </TalaDBProvider>
  )
  return { wrapper, docsIn, db }
}

beforeEach(() => {
  resetWarnings()
  resetForgetTimers()
})

afterEach(() => {
  cleanup()
  resetInflight()
  resetForgetTimers()
})

/** A minimal error boundary — the thing `throwOnError` exists to reach. */
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

function Query({ options }: { options: Record<string, unknown> }) {
  const { status } = useQuery<Todo>(options as never)
  return <p data-testid="status">{status}</p>
}

describe('throwOnError', () => {
  it('rethrows during render so an error boundary can catch it', async () => {
    const { wrapper: Wrapper } = setup()
    const queryFn = vi.fn().mockRejectedValue(new Error('boom'))
    const onCatch = vi.fn()
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})

    render(
      <Wrapper>
        <Boundary onCatch={onCatch}>
          <Query options={{ queryKey: ['todos'], queryFn, throwOnError: true }} />
        </Boundary>
      </Wrapper>,
    )

    await waitFor(() => expect(onCatch).toHaveBeenCalled())
    expect((onCatch.mock.calls[0][0] as Error).message).toBe('boom')
    expect(screen.getByText('caught')).toBeDefined()

    spy.mockRestore()
    cleanup()
  })

  it('defers to a predicate, so only some failures reach the boundary', async () => {
    const { wrapper: Wrapper } = setup()
    const queryFn = vi.fn().mockRejectedValue(new Error('just a 404'))
    const onCatch = vi.fn()

    render(
      <Wrapper>
        <Boundary onCatch={onCatch}>
          <Query
            options={{
              queryKey: ['todos'],
              queryFn,
              throwOnError: (error: Error) => !error.message.includes('404'),
            }}
          />
        </Boundary>
      </Wrapper>,
    )

    // Handled inline instead: the component renders the error state itself.
    await waitFor(() => expect(screen.getByTestId('status').textContent).toBe('error'))
    expect(onCatch).not.toHaveBeenCalled()

    cleanup()
  })
})

describe('queryKeyHashFn', () => {
  it('decides which queries share a record', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])
    // Ignores everything after the first segment, so both keys are one query.
    const queryKeyHashFn = (key: readonly unknown[]) => JSON.stringify(key[0])

    const first = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos', 1], queryFn, queryKeyHashFn }),
      { wrapper },
    )
    await waitFor(() => expect(first.result.current.status).toBe('success'))
    first.unmount()

    const second = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos', 2], queryFn, queryKeyHashFn }),
      { wrapper },
    )
    await waitFor(() => expect(second.result.current.status).toBe('success'))

    // A different key, but the same record — so still fresh, and no second fetch.
    expect(queryFn).toHaveBeenCalledTimes(1)
  })
})

describe('forgetAfter', () => {
  it('drops the result set after nothing has used the key', async () => {
    const { wrapper, db, docsIn } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result, unmount } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, forgetAfter: 5 }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))
    unmount()

    const queries = await openQueryCollection(db)
    await waitFor(async () =>
      expect(await readQueryRecord(queries, 'todos', ['todos'])).toBeNull(),
    )

    // The documents are untouched. They live in the application's own
    // collection, which it also reads directly — sweeping them would delete
    // real user data rather than reclaim a cache.
    expect(docsIn('todos')).toHaveLength(1)
  })

  it('cancels the timer when the key comes back', async () => {
    const { wrapper, db } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])
    const options = { queryKey: ['todos'], queryFn, forgetAfter: 30 }

    const first = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(first.result.current.status).toBe('success'))
    first.unmount()

    // Remounted well inside the window: a route the user keeps returning to
    // must never expire.
    const second = renderHook(() => useQuery<Todo>(options), { wrapper })
    await waitFor(() => expect(second.result.current.status).toBe('success'))
    await new Promise((r) => setTimeout(r, 60))

    const queries = await openQueryCollection(db)
    expect(await readQueryRecord(queries, 'todos', ['todos'])).not.toBeNull()
  })

  it('remembers indefinitely by default', async () => {
    const { wrapper, db } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result, unmount } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))
    unmount()
    await new Promise((r) => setTimeout(r, 30))

    // A record is what makes a warm start warm, and a cache that survives
    // reload has no memory pressure to relieve.
    const queries = await openQueryCollection(db)
    expect(await readQueryRecord(queries, 'todos', ['todos'])).not.toBeNull()
  })
})

describe('enabled as a function', () => {
  it('gates the fetch, and re-evaluates as its inputs change', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result, rerender } = renderHook(
      ({ id }: { id: string | null }) =>
        useQuery<Todo>({ queryKey: ['todos', id], queryFn, enabled: () => id !== null }),
      { wrapper, initialProps: { id: null as string | null } },
    )
    await new Promise((r) => setTimeout(r, 20))
    expect(queryFn).not.toHaveBeenCalled()

    // The dependent-query shape: wait for the value, then go.
    rerender({ id: 'a' })
    await waitFor(() => expect(result.current.status).toBe('success'))
    expect(queryFn).toHaveBeenCalledTimes(1)
  })
})

describe('refetchInterval as a function', () => {
  it('is re-evaluated, so polling can stop itself', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])
    let polling = true

    const { result } = renderHook(
      () =>
        useQuery<Todo>({
          queryKey: ['todos'],
          queryFn,
          refetchInterval: () => (polling ? 10 : false),
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))
    await waitFor(() => expect(queryFn.mock.calls.length).toBeGreaterThan(1))

    polling = false
    await result.current.refetch()
    const settled = queryFn.mock.calls.length
    await new Promise((r) => setTimeout(r, 40))

    expect(queryFn.mock.calls.length).toBe(settled)
  })
})

describe('refetch', () => {
  it('resolves with the result of the fetch it just performed', async () => {
    const { wrapper } = setup()
    const queryFn = vi
      .fn()
      .mockResolvedValueOnce([todo('a', 'before')])
      .mockResolvedValueOnce([todo('a', 'after'), todo('b', 'and')])

    const { result } = renderHook(
      () => useQuery<Todo>({ queryKey: ['todos'], queryFn, staleTime: 60_000 }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))

    // Resolved on the next commit, so this is the data it just fetched rather
    // than the render before it.
    const returned = await result.current.refetch()
    expect(returned.data).toHaveLength(2)
    expect(returned.data?.[0].title).toBe('after')
  })

  it('joins the request already in flight when cancelRefetch is false', async () => {
    const { wrapper } = setup()
    let release: (todos: Todo[]) => void = () => {}
    const queryFn = vi.fn().mockImplementation(
      () =>
        new Promise<Todo[]>((resolve) => {
          release = resolve
        }),
    )

    const { result } = renderHook(() => useQuery<Todo>({ queryKey: ['todos'], queryFn }), {
      wrapper,
    })
    await waitFor(() => expect(queryFn).toHaveBeenCalledTimes(1))

    const joined = result.current.refetch({ cancelRefetch: false })
    release([todo('a', 'one')])
    await joined

    // No second request: it waited for the one already on its way.
    expect(queryFn).toHaveBeenCalledTimes(1)
  })

  it('does not report an abandoned request as an error', async () => {
    const { wrapper } = setup()
    let release: (todos: Todo[]) => void = () => {}
    const queryFn = vi
      .fn()
      .mockImplementationOnce(
        () =>
          new Promise<Todo[]>((resolve) => {
            release = resolve
          }),
      )
      .mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(() => useQuery<Todo>({ queryKey: ['todos'], queryFn }), {
      wrapper,
    })
    await waitFor(() => expect(queryFn).toHaveBeenCalledTimes(1))

    // The default: supersede whatever is on its way.
    await result.current.refetch()
    release([todo('z', 'stale')])
    await new Promise((r) => setTimeout(r, 20))

    // Superseding a request is not a failure, and the user must not see one.
    expect(result.current.error).toBeNull()
    expect(result.current.status).toBe('success')
  })
})

describe('isStale', () => {
  it('flips on its own once the freshness window passes', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(
      () =>
        useQuery<Todo>({
          queryKey: ['todos'],
          queryFn,
          staleTime: 150,
          refetchOnWindowFocus: false,
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))
    expect(result.current.isStale).toBe(false)

    // Computed at render, so without a timer it would keep reporting `false`
    // while sitting on screen — and any UI keyed off it would never appear.
    await waitFor(() => expect(result.current.isStale).toBe(true))
  })
})

describe('accepted and ignored options', () => {
  it('warns once for an option that does nothing, and says why', async () => {
    const { wrapper } = setup()
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result, rerender } = renderHook(
      () =>
        useQuery<Todo>({
          queryKey: ['todos'],
          queryFn,
          notifyOnChangeProps: ['data'],
        }),
      { wrapper },
    )
    await waitFor(() => expect(result.current.status).toBe('success'))
    rerender()

    const messages = warn.mock.calls.map((call) => String(call[0]))
    const notices = messages.filter((m) => m.includes('notifyOnChangeProps'))
    // Once, not once per render — a warning on every render is noise that gets
    // filtered out, which is the same as not warning at all.
    expect(notices).toHaveLength(1)
    expect(notices[0]).toMatch(/not implemented/)

    warn.mockRestore()
  })

})
