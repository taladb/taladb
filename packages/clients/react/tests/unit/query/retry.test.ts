/**
 * Read-path retry policy.
 *
 * Separate from the write queue's backoff in `drain.ts` on purpose: a queued
 * write must survive a reload, so its attempt count lives on the document; a
 * read belongs to one mounted component and dies with it.
 */
import { describe, it, expect, vi } from 'vitest'
import { DEFAULT_RETRY, retryDelayMs, shouldRetry, sleep, isOffline } from '../../../src/query/retry'

const error = new Error('boom')

describe('shouldRetry', () => {
  it('retries three times by default', () => {
    expect(DEFAULT_RETRY).toBe(3)
    // Four attempts in total: three failures still retry, the fourth gives up.
    expect(shouldRetry(undefined, 1, error)).toBe(true)
    expect(shouldRetry(undefined, 3, error)).toBe(true)
    expect(shouldRetry(undefined, 4, error)).toBe(false)
  })

  it('honours false and true', () => {
    expect(shouldRetry(false, 1, error)).toBe(false)
    expect(shouldRetry(true, 99, error)).toBe(true)
  })

  it('honours a numeric cap', () => {
    expect(shouldRetry(1, 1, error)).toBe(true)
    expect(shouldRetry(1, 2, error)).toBe(false)
    expect(shouldRetry(0, 1, error)).toBe(false)
  })

  it('defers to a predicate, which is how a 404 stops early', () => {
    const notFound = Object.assign(new Error('not found'), { status: 404 })
    const retry = (_count: number, cause: Error) =>
      (cause as { status?: number }).status !== 404

    expect(shouldRetry(retry, 1, notFound)).toBe(false)
    expect(shouldRetry(retry, 1, error)).toBe(true)
  })
})

describe('retryDelayMs', () => {
  it('backs off exponentially from one second', () => {
    expect(retryDelayMs(undefined, 0, error)).toBe(1000)
    expect(retryDelayMs(undefined, 1, error)).toBe(2000)
    expect(retryDelayMs(undefined, 2, error)).toBe(4000)
  })

  it('caps at thirty seconds', () => {
    // Same curve TanStack uses, so migrated retry timing does not shift.
    expect(retryDelayMs(undefined, 20, error)).toBe(30_000)
  })

  it('accepts a fixed number or a function', () => {
    expect(retryDelayMs(50, 5, error)).toBe(50)
    expect(retryDelayMs((attempt) => attempt * 10, 3, error)).toBe(30)
  })
})

describe('sleep', () => {
  it('resolves after the delay', async () => {
    const controller = new AbortController()
    const started = Date.now()
    await sleep(10, controller.signal)
    expect(Date.now() - started).toBeGreaterThanOrEqual(8)
  })

  it('resolves immediately when already aborted', async () => {
    const controller = new AbortController()
    controller.abort()
    // Resolves rather than rejects: the caller re-checks `aborted` straight
    // after, and a rejection would be indistinguishable from a fetch failure.
    await expect(sleep(10_000, controller.signal)).resolves.toBeUndefined()
  })

  it('gives up as soon as the query is abandoned', async () => {
    const controller = new AbortController()
    const pending = sleep(10_000, controller.signal)
    controller.abort()
    await expect(pending).resolves.toBeUndefined()
  })
})

describe('isOffline', () => {
  it('treats an unknown connection state as online', () => {
    const original = Object.getOwnPropertyDescriptor(globalThis, 'navigator')
    vi.stubGlobal('navigator', undefined)
    expect(isOffline()).toBe(false)
    vi.unstubAllGlobals()
    if (original) Object.defineProperty(globalThis, 'navigator', original)
  })

  it('reports offline when the browser says so', () => {
    vi.stubGlobal('navigator', { onLine: false })
    expect(isOffline()).toBe(true)
    vi.unstubAllGlobals()
  })
})
