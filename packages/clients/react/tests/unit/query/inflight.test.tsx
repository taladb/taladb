/**
 * One request per key, however many components ask.
 *
 * This is the gap a sequential test suite cannot see: every other test here
 * mounts one hook at a time, so the first writes a record the rest read warm,
 * and three identical requests would look like one. These mount together on
 * purpose.
 */
import { describe, it, expect, vi, afterEach } from 'vitest'
import React from 'react'
import { cleanup, render, renderHook, screen, waitFor } from '@testing-library/react'
import type { Document } from 'taladb'
import { TalaDBProvider } from '../../../src/context'
import { QueryProvider } from '../../../src/query/context'
import { useQuery, resetForgetTimers } from '../../../src/query/useQuery'
import { resetInflight, isShared, runShared, inflightKey } from '../../../src/query/inflight'
import { createFakeDB } from '../../helpers/fakeQueryDB'
import { id } from '../../helpers/docId'

interface Todo extends Document {
  _id?: string
  title: string
}

const todo = (label: string, title: string): Todo => ({ _id: id(label), title })

afterEach(() => {
  cleanup()
  resetInflight()
  resetForgetTimers()
})

function Providers({ db, children }: { db: never; children: React.ReactNode }) {
  return (
    <TalaDBProvider db={db}>
      <QueryProvider defaultOptions={{ queries: { retry: false } }}>{children}</QueryProvider>
    </TalaDBProvider>
  )
}

function Row({ queryFn }: { queryFn: () => Promise<Todo[]> }) {
  const { data, status } = useQuery<Todo>({ queryKey: ['todos'], queryFn })
  return <span data-testid="row">{status === 'success' ? data.length : status}</span>
}

describe('request deduplication', () => {
  it('fetches once for three components sharing a key', async () => {
    const { db } = createFakeDB()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    render(
      <Providers db={db as never}>
        <Row queryFn={queryFn} />
        <Row queryFn={queryFn} />
        <Row queryFn={queryFn} />
      </Providers>,
    )

    await waitFor(() => expect(screen.getAllByTestId('row')[0].textContent).toBe('1'))

    // A list, a count badge and a header on one screen is an ordinary shape.
    // Without sharing this is three identical requests.
    expect(queryFn).toHaveBeenCalledTimes(1)
    for (const row of screen.getAllByTestId('row')) expect(row.textContent).toBe('1')
  })

  it('does not share across different keys', async () => {
    const { db } = createFakeDB()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    function Other() {
      const { status } = useQuery<Todo>({ queryKey: ['notes'], queryFn })
      return <span data-testid="other">{status}</span>
    }

    render(
      <Providers db={db as never}>
        <Row queryFn={queryFn} />
        <Other />
      </Providers>,
    )

    await waitFor(() => expect(screen.getByTestId('other').textContent).toBe('success'))
    expect(queryFn).toHaveBeenCalledTimes(2)
  })

  it('clears the slot once the request settles', async () => {
    const { db } = createFakeDB()
    const queryFn = vi.fn().mockResolvedValue([todo('a', 'one')])

    const { result } = renderHook(() => useQuery<Todo>({ queryKey: ['todos'], queryFn }), {
      wrapper: ({ children }) => <Providers db={db as never}>{children}</Providers>,
    })
    await waitFor(() => expect(result.current.status).toBe('success'))

    // Otherwise the next mount would join a request that finished long ago.
    expect(isShared(inflightKey('todos', ''))).toBe(false)
  })
})

describe('runShared', () => {
  it('abandons the work only when the last caller leaves', async () => {
    let observed: AbortSignal | undefined
    const first = new AbortController()
    const second = new AbortController()
    const work = (signal: AbortSignal) =>
      new Promise<string>((resolve) => {
        observed = signal
        setTimeout(() => resolve('done'), 20)
      })

    const a = runShared('k', work, first.signal)
    const b = runShared('k', work, second.signal)

    first.abort()
    // One component unmounting must not cancel a fetch another is waiting on.
    expect(observed?.aborted).toBe(false)

    await expect(b).resolves.toBe('done')
    await expect(a).resolves.toBe('done')
  })

  it('abandons the work when every caller leaves', async () => {
    let observed: AbortSignal | undefined
    const first = new AbortController()
    const second = new AbortController()
    const work = (signal: AbortSignal) =>
      new Promise<string>((resolve) => {
        observed = signal
        setTimeout(() => resolve('done'), 50)
      })

    void runShared('k2', work, first.signal).catch(() => {})
    void runShared('k2', work, second.signal).catch(() => {})

    first.abort()
    second.abort()

    expect(observed?.aborted).toBe(true)
  })

  it('runs the work once, not once per caller', async () => {
    const work = vi.fn((_: AbortSignal) => Promise.resolve('done'))
    const controller = new AbortController()

    const results = await Promise.all([
      runShared('k3', work, controller.signal),
      runShared('k3', work, controller.signal),
    ])

    expect(work).toHaveBeenCalledTimes(1)
    expect(results).toEqual(['done', 'done'])
  })
})
