/**
 * The freshness timer, against a clock that wakes early.
 *
 * `setTimeout` is allowed to fire a hair before its deadline — the platform
 * rounds the delay down to whole milliseconds — and Node does so for roughly 1
 * in 100 timers. That is enough to matter here: the re-render the timer forces
 * does not re-run the effect that scheduled it, so a wake-up that lands even 1ms
 * early leaves `isStale` reporting `false` for as long as the query stays on
 * screen, with nothing left to correct it.
 *
 * The early margin is exaggerated to 20ms so the case is deterministic rather
 * than a 1-in-100 flake. Ordinary render latency absorbs a 1ms miss most of the
 * time, which is exactly why the real bug surfaced as an occasional red CI run.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import React from 'react'
import { cleanup, renderHook, waitFor } from '@testing-library/react'
import type { Document } from 'taladb'
import { TalaDBProvider } from '../../../src/context'
import { QueryProvider } from '../../../src/query/context'
import { useQuery, resetForgetTimers } from '../../../src/query/useQuery'
import { resetWarnings } from '../../../src/query/dev'
import { resetInflight } from '../../../src/query/inflight'
import { createFakeDB } from '../../helpers/fakeQueryDB'

interface Todo extends Document {
  _id?: string
  title: string
}

function setup() {
  const { db } = createFakeDB()
  const wrapper = ({ children }: { children: React.ReactNode }) => (
    <TalaDBProvider db={db}>
      <QueryProvider defaultOptions={{ queries: { retry: false } }}>{children}</QueryProvider>
    </TalaDBProvider>
  )
  return { wrapper }
}

/** How far ahead of its deadline every timer fires, in ms. */
const EARLY_BY = 20

const realSetTimeout = globalThis.setTimeout

beforeEach(() => {
  resetWarnings()
  resetForgetTimers()
  globalThis.setTimeout = ((fn: () => void, ms?: number, ...rest: unknown[]) =>
    realSetTimeout(
      fn,
      typeof ms === 'number' && ms > EARLY_BY ? ms - EARLY_BY : ms,
      ...rest,
    )) as never
})

afterEach(() => {
  globalThis.setTimeout = realSetTimeout
  cleanup()
  resetInflight()
  resetForgetTimers()
})

describe('isStale with an early-waking timer', () => {
  it('re-checks and still flips once the window has really passed', async () => {
    const { wrapper } = setup()
    const queryFn = vi.fn().mockResolvedValue([{ _id: 'a', title: 'one' }])

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

    // Before the fix this stayed `false` forever: the timer woke 20ms early,
    // recomputed `isStale` as `false`, and nothing rescheduled it.
    await waitFor(() => expect(result.current.isStale).toBe(true), { timeout: 5000 })
  })
})
