/**
 * The three fixture components, rendered.
 *
 * These are the acceptance test for §0 of PLAN-query-parity.md: TanStack v5
 * code whose only edit is its import. The components are not adapted here —
 * if an assertion fails, the fix belongs in the library, not in the fixture.
 *
 * Expected to fail until P1–P4 land. See ./README.md.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest'
import React from 'react'
import { cleanup, render, screen, waitFor } from '@testing-library/react'
import { TalaDBProvider } from '../../src/context'
import { QueryProvider } from '../../src/query/context'
import { createFakeDB } from '../helpers/fakeQueryDB'
import { TodoList } from './fixtures/TodoList'
import { TodoDetail } from './fixtures/TodoDetail'
import { TodoPage } from './fixtures/TodoPage'
import type { Todo } from './fixtures/todo'
import { id as docId } from '../helpers/docId'

const todo = (id: string, title: string, dueAt: string | null = null): Todo => ({
  _id: docId(id),
  title,
  done: false,
  dueAt,
})

/** Stands in for the network. Resolves whatever the current route is set to. */
function stubFetch(routes: Record<string, unknown>) {
  const calls: string[] = []
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      calls.push(url)
      const body = routes[url]
      if (body === undefined) {
        return { ok: false, status: 404, json: async () => ({}) } as unknown as Response
      }
      return { ok: true, status: 200, json: async () => body } as unknown as Response
    }),
  )
  return calls
}

function renderWithProviders(ui: React.ReactElement) {
  const { db, docsIn } = createFakeDB()
  // `retry: false` is what a migrating app's test harness sets, in exactly the
  // place it used to set it on `new QueryClient({ defaultOptions })`. Without
  // it the default three retries make every failure assertion wait out seven
  // seconds of backoff.
  const wrap = (child: React.ReactElement) => (
    <TalaDBProvider db={db}>
      <QueryProvider defaultOptions={{ queries: { retry: false } }}>{child}</QueryProvider>
    </TalaDBProvider>
  )
  const result = render(wrap(ui))
  return {
    ...result,
    docsIn,
    // Re-wrapped: Testing Library's own `rerender` replaces the entire tree
    // with what it is given, which would drop the providers and take the
    // database with them.
    rerender: (next: React.ReactElement) => result.rerender(wrap(next)),
  }
}

beforeEach(() => {
  vi.unstubAllGlobals()
})

afterEach(() => {
  // Explicit, because Testing Library only registers its own auto-cleanup when
  // the runner exposes global `afterEach` — and this config does not enable
  // vitest's globals. Without it, a `getByText` in one test happily matches the
  // previous test's DOM and the assertion after it fails somewhere confusing.
  cleanup()
  vi.unstubAllGlobals()
})

describe('TodoList — the default read', () => {
  it('shows a spinner, then the list, and stores the documents', async () => {
    stubFetch({ '/api/todos': [todo('a', 'buy milk'), todo('b', 'call mum')] })

    const { docsIn } = renderWithProviders(<TodoList />)

    // `isPending` on the first paint, before the collection has been read.
    expect(screen.getByRole('status').textContent).toBe('Loading…')
    await waitFor(() => expect(screen.getByText('buy milk')).toBeDefined())
    expect(screen.getByText('call mum')).toBeDefined()

    // Inferred from queryKey[0], with no `collection` option in the component.
    expect(docsIn('todos')).toHaveLength(2)
  })

  it('renders the error message when the fetch fails', async () => {
    stubFetch({}) // every route 404s

    renderWithProviders(<TodoList />)

    await waitFor(() => expect(screen.getByRole('alert')).toBeDefined())
    expect(screen.getByRole('alert').textContent).toContain('Failed to load todos')
  })
})

describe('TodoDetail — a single document', () => {
  it('does not fetch while disabled', async () => {
    const calls = stubFetch({ '/api/todos/a': todo('a', 'buy milk') })

    renderWithProviders(<TodoDetail todoId={null} />)

    await waitFor(() => expect(screen.getByText('Pick a todo.')).toBeDefined())
    expect(calls).toEqual([])
  })

  it('renders the selected view of one document', async () => {
    const past = new Date(Date.now() - 86_400_000).toISOString()
    stubFetch({ '/api/todos/a': todo('a', 'buy milk', past) })

    const { docsIn } = renderWithProviders(<TodoDetail todoId="a" />)

    await waitFor(() => expect(screen.getByText('buy milk')).toBeDefined())
    // `select` derived it; the stored document keeps every field.
    expect(screen.getByTestId('overdue')).toBeDefined()
    expect(docsIn('todos')).toHaveLength(1)
  })
})

describe('TodoPage — pagination', () => {
  it('keeps the previous page visible while the next one loads', async () => {
    stubFetch({
      '/api/todos?page=1': [todo('a', 'page one')],
      '/api/todos?page=2': [todo('b', 'page two')],
    })

    const { rerender, container } = renderWithProviders(<TodoPage page={1} />)
    await waitFor(() => expect(screen.getByText('page one')).toBeDefined())

    rerender(<TodoPage page={2} />)

    // No spinner: `keepPreviousData` holds page one on screen. This is the
    // assertion that fails loudly if `placeholderData` is unimplemented, since
    // the query would go pending and unmount the list entirely.
    expect(screen.queryByRole('status')).toBeNull()
    expect(screen.getByText('page one')).toBeDefined()
    expect(container.querySelector('[data-stale="true"]')).not.toBeNull()

    await waitFor(() => expect(screen.getByText('page two')).toBeDefined())
    expect(container.querySelector('[data-stale="false"]')).not.toBeNull()
  })
})
