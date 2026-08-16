/**
 * The mutation fixtures, rendered.
 *
 * TanStack v5 components whose only edits are the import and the two options
 * this layer needs — `collection`, and either `url` or (for `immediate`)
 * `mutationFn`. The components are not adapted here: if an assertion fails, the
 * fix belongs in the library.
 *
 * Expected to fail until PLAN-mutation-parity.md M1–M2 land.
 */
import { describe, it, expect, vi } from 'vitest'
import React from 'react'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { TalaDBProvider } from '../../src/context'
import { QueryProvider } from '../../src/query/context'
import { createFakeDB } from '../helpers/fakeQueryDB'
import { CreateTodo } from './fixtures/CreateTodo'
import { ToggleTodo } from './fixtures/ToggleTodo'
import { CheckoutOrder } from './fixtures/CheckoutOrder'
import type { Order, Todo } from './fixtures/todo'
import { id as docId } from '../helpers/docId'

const todo = (id: string, title: string, done = false): Todo => ({
  _id: docId(id),
  title,
  done,
  dueAt: null,
})

function renderWithProviders(ui: React.ReactElement) {
  const { db, docsIn } = createFakeDB()
  return {
    ...render(
      <TalaDBProvider db={db}>
        <QueryProvider defaultOptions={{ queries: { retry: false } }}>{ui}</QueryProvider>
      </TalaDBProvider>,
    ),
    docsIn,
    db,
  }
}

describe('CreateTodo — optimistic by default', () => {
  it('stores the document and fires onSuccess without waiting for the network', async () => {
    const onCreated = vi.fn()
    const { docsIn } = renderWithProviders(<CreateTodo onCreated={onCreated} />)

    fireEvent.change(screen.getByLabelText('Title'), { target: { value: 'Buy milk' } })
    fireEvent.submit(screen.getByRole('button'))

    // No network stub at all: under `optimistic` the local commit is what the
    // UI reacts to, and the request happens behind it.
    await waitFor(() => expect(onCreated).toHaveBeenCalledTimes(1))
    expect(docsIn('todos')).toHaveLength(1)
    expect(docsIn('todos')[0].title).toBe('Buy milk')
  })

  it('never disables the button, because nothing is being waited on', async () => {
    const { docsIn } = renderWithProviders(<CreateTodo onCreated={() => {}} />)
    const button = screen.getByRole('button')

    fireEvent.change(screen.getByLabelText('Title'), { target: { value: 'Buy milk' } })
    fireEvent.submit(button)

    await waitFor(() => expect(docsIn('todos')).toHaveLength(1))
    // `isPending` exists and stays false — a spinner here would be a lie.
    expect(button).not.toHaveProperty('disabled', true)
    expect(button.textContent).toBe('Add')
  })
})

describe('ToggleTodo — an update with per-call callbacks', () => {
  it('writes the patch and reports success', async () => {
    const { db, docsIn } = createFakeDB()
    await db.collection('todos').insert(todo('a', 'Buy milk') as never)
    const onToggled = vi.fn()

    render(
      <TalaDBProvider db={db}>
        <QueryProvider>
          <ToggleTodo todo={todo('a', 'Buy milk')} onToggled={onToggled} />
        </QueryProvider>
      </TalaDBProvider>,
    )

    fireEvent.click(screen.getByRole('checkbox'))

    await waitFor(() => expect(onToggled).toHaveBeenCalledWith(docId('a')))
    await waitFor(() =>
      expect(screen.getByTestId(`status-${docId('a')}`).textContent).toBe('success'),
    )
    // An update sends only what changed; the rest of the document survives.
    const stored = docsIn('todos')[0]
    expect(stored.done).toBe(true)
    expect(stored.title).toBe('Buy milk')
  })
})

describe('CheckoutOrder — immediate, the mode mutationFn belongs to', () => {
  it('waits for the server, then stores what it returned', async () => {
    const placed: Order = { _id: 'o1', total: 42, items: ['a'] }
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: true, status: 201, json: async () => placed }) as never),
    )
    const onPlaced = vi.fn()
    const { docsIn } = renderWithProviders(
      <CheckoutOrder cart={{ _id: 'o1', total: 42, items: ['a'] }} onPlaced={onPlaced} />,
    )

    fireEvent.click(screen.getByRole('button'))

    await waitFor(() => expect(onPlaced).toHaveBeenCalledWith('o1'))
    expect(screen.getByTestId('placed').textContent).toBe('o1')
    expect(docsIn('orders')).toHaveLength(1)

    vi.unstubAllGlobals()
  })

  it('stores nothing when the server refuses', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok: false, status: 402, json: async () => ({}) }) as never),
    )
    const onPlaced = vi.fn()
    const { docsIn } = renderWithProviders(
      <CheckoutOrder cart={{ _id: 'o1', total: 42, items: ['a'] }} onPlaced={onPlaced} />,
    )

    fireEvent.click(screen.getByRole('button'))

    await waitFor(() => expect(screen.getByRole('alert')).toBeDefined())
    // Nothing was written, so there is nothing to roll back — the reason this
    // is the right mode for an irreversible write.
    expect(docsIn('orders')).toHaveLength(0)
    expect(onPlaced).not.toHaveBeenCalled()

    vi.unstubAllGlobals()
  })
})
