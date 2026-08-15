/**
 * `useMutation`'s shape, pinned against TanStack Query v5.
 *
 * Expected to fail until PLAN-mutation-parity.md M1–M2 land. See ./README.md
 * for how to read a failure.
 *
 * The residue tests at the bottom are the ones worth reading first: they assert
 * the *error message* for the case that cannot work, rather than pretending it
 * does.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { renderHook, waitFor } from '@testing-library/react'
import { TalaDBProvider } from '../../src/context'
import { QueryProvider } from '../../src/query/context'
import { useMutation } from '../../src/query/useMutation'
import { createFakeDB } from '../helpers/fakeQueryDB'
import type { Todo } from './fixtures/todo'

/**
 * Every field TanStack v5's `UseMutationResult` documents as stable.
 *
 * `context` is deliberately absent — it flows between callbacks, not onto the
 * result — and so is `mutationKey`, which is an option rather than a result.
 */
const REQUIRED_RESULT_KEYS = [
  'data',
  'error',
  'failureCount',
  'failureReason',
  'isError',
  'isIdle',
  'isPending',
  'isPaused',
  'isSuccess',
  'mutate',
  'mutateAsync',
  'reset',
  'status',
  'submittedAt',
  'variables',
] as const

function setup() {
  const { db, docsIn } = createFakeDB()
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db}>
      <QueryProvider>{children}</QueryProvider>
    </TalaDBProvider>
  )
  return { wrapper, docsIn }
}

// The options a migrated call site passes — `mutate(variables)`, not an op
// union. Cast at the call site so each failure points at what it is about.
const migrated = (options: Record<string, unknown>) =>
  useMutation<Todo>(options as never) as unknown as Record<string, unknown>

describe('mutation result shape', () => {
  it('exposes every stable field of a TanStack v5 mutation result', () => {
    const { wrapper } = setup()

    const { result } = renderHook(
      () => migrated({ collection: 'todos', url: '/api/todos/:id' }),
      { wrapper },
    )

    const missing = REQUIRED_RESULT_KEYS.filter((key) => !(key in result.current))
    expect(missing, `missing from the mutation result: ${missing.join(', ')}`).toEqual([])
  })

  it('starts idle, which useQuery has no equivalent of', () => {
    const { wrapper } = setup()
    const { result } = renderHook(
      () => migrated({ collection: 'todos', url: '/api/todos/:id' }),
      { wrapper },
    )

    // A mutation that has never run is not "pending" — nobody asked for
    // anything yet. Migrated code branches on this to render a clean form.
    expect(result.current.status).toBe('idle')
    expect(result.current.isIdle).toBe(true)
    expect(result.current.isPending).toBe(false)
    expect(result.current.data).toBeUndefined()
  })
})

describe('mutate(variables)', () => {
  it('takes the document, not an operation union', async () => {
    const { wrapper, docsIn } = setup()
    const { result } = renderHook(
      () => migrated({ collection: 'todos', url: '/api/todos/:id' }),
      { wrapper },
    )

    await (result.current.mutateAsync as (vars: unknown) => Promise<unknown>)({
      title: 'Buy milk',
      done: false,
      dueAt: null,
    })

    // TanStack's signature exactly: the variables are the thing being written.
    await waitFor(() => expect(docsIn('todos')).toHaveLength(1))
    expect(docsIn('todos')[0].title).toBe('Buy milk')
  })

  it('records the variables it was last called with', async () => {
    const { wrapper } = setup()
    const { result } = renderHook(
      () => migrated({ collection: 'todos', url: '/api/todos/:id' }),
      { wrapper },
    )

    await (result.current.mutateAsync as (vars: unknown) => Promise<unknown>)({
      title: 'Buy milk',
      done: false,
      dueAt: null,
    })

    await waitFor(() =>
      expect((result.current.variables as { title?: string })?.title).toBe('Buy milk'),
    )
  })

  it('reports success without ever going pending under the default mode', async () => {
    const { wrapper } = setup()
    const { result } = renderHook(
      () => migrated({ collection: 'todos', url: '/api/todos/:id' }),
      { wrapper },
    )

    await (result.current.mutateAsync as (vars: unknown) => Promise<unknown>)({
      title: 'Buy milk',
      done: false,
      dueAt: null,
    })

    // `isPending` never goes true under `optimistic` — the UI does not wait for
    // a network round trip it is not making. The field still has to exist,
    // because migrated code disables buttons on it.
    await waitFor(() => expect(result.current.status).toBe('success'))
    expect(result.current.isPending).toBe(false)
  })

  it('resets back to idle', async () => {
    const { wrapper } = setup()
    const { result } = renderHook(
      () => migrated({ collection: 'todos', url: '/api/todos/:id' }),
      { wrapper },
    )

    await (result.current.mutateAsync as (vars: unknown) => Promise<unknown>)({
      title: 'Buy milk',
      done: false,
      dueAt: null,
    })
    await waitFor(() => expect(result.current.status).toBe('success'))
    ;(result.current.reset as () => void)()

    await waitFor(() => expect(result.current.status).toBe('idle'))
    expect(result.current.variables).toBeUndefined()
  })
})

describe('callbacks', () => {
  it('fires onSuccess on the local commit under the default mode', async () => {
    const { wrapper } = setup()
    const onSuccess = vi.fn()
    const { result } = renderHook(
      () => migrated({ collection: 'todos', url: '/api/todos/:id', onSuccess }),
      { wrapper },
    )

    await (result.current.mutateAsync as (vars: unknown) => Promise<unknown>)({
      title: 'Buy milk',
      done: false,
      dueAt: null,
    })

    // Waiting for the server here would mean never firing for most optimistic
    // writes — the drain may land after this component is gone. `onSynced` is
    // the callback for server confirmation.
    await waitFor(() => expect(onSuccess).toHaveBeenCalledTimes(1))
    const [data, variables] = onSuccess.mock.calls[0]
    expect((data as Todo).title).toBe('Buy milk')
    expect((variables as Todo).title).toBe('Buy milk')
  })

  it('accepts per-call callbacks alongside the hook-level ones', async () => {
    const { wrapper } = setup()
    const hookLevel = vi.fn()
    const perCall = vi.fn()
    const { result } = renderHook(
      () => migrated({ collection: 'todos', url: '/api/todos/:id', onSuccess: hookLevel }),
      { wrapper },
    )

    await (
      result.current.mutateAsync as (vars: unknown, opts: unknown) => Promise<unknown>
    )({ title: 'Buy milk', done: false, dueAt: null }, { onSuccess: perCall })

    // They compose. The per-call form does not replace the hook-level one — it
    // is how a callback depends on which row was clicked.
    await waitFor(() => expect(perCall).toHaveBeenCalledTimes(1))
    expect(hookLevel).toHaveBeenCalledTimes(1)
  })
})

describe('the residue', () => {
  it('refuses mutationFn under the default optimistic mode', () => {
    const { wrapper } = setup()
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})

    // The request is sent by the drain loop, after a route change or a reload,
    // when the closure is long gone. Accepting this would mean silently
    // dropping the write — worse than refusing it.
    expect(() =>
      renderHook(
        () => migrated({ collection: 'todos', mutationFn: async (todo: unknown) => todo }),
        { wrapper },
      ),
    ).toThrow(/mutationFn/)

    spy.mockRestore()
  })

  it('explains the fix in the same message', () => {
    const { wrapper } = setup()
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {})

    let message = ''
    try {
      renderHook(
        () => migrated({ collection: 'todos', mutationFn: async (todo: unknown) => todo }),
        { wrapper },
      )
    } catch (error) {
      message = (error as Error).message
    }

    // A codemod cannot fix this, so the runtime carries the guide: which mode
    // it is in, and what to use instead.
    expect(message).toMatch(/optimistic/)
    expect(message).toMatch(/url/)
    expect(message).toMatch(/immediate/)

    spy.mockRestore()
  })
})
