import type { RetryDelayOption, RetryOption } from './types'

/**
 * Read-path retry policy.
 *
 * Deliberately separate from the write queue's backoff in `drain.ts`. They look
 * alike and mean different things: a queued write must survive a reload and a
 * week offline, so its attempt count lives on the document. A read is
 * in-flight state belonging to one mounted component, and is abandoned the
 * moment that component unmounts.
 */

/** TanStack's default: three retries, so four attempts in all. */
export const DEFAULT_RETRY = 3

/**
 * Whether to try again after `failureCount` failures.
 *
 * `failureCount` is the number of attempts that have already failed, so a
 * numeric `retry` of 3 keeps going while it is 1, 2 or 3 and gives up at 4.
 */
export function shouldRetry(
  retry: RetryOption | undefined,
  failureCount: number,
  error: Error,
): boolean {
  const setting = retry ?? DEFAULT_RETRY
  if (typeof setting === 'function') return setting(failureCount, error)
  if (typeof setting === 'boolean') return setting
  return failureCount <= setting
}

/**
 * How long to wait before attempt `attemptIndex + 1`.
 *
 * Exponential from one second, capped at thirty — the same curve TanStack uses,
 * so a migrated app's retry timing does not quietly change.
 */
export function retryDelayMs(
  retryDelay: RetryDelayOption | undefined,
  attemptIndex: number,
  error: Error,
): number {
  if (typeof retryDelay === 'number') return retryDelay
  if (typeof retryDelay === 'function') return retryDelay(attemptIndex, error)
  return Math.min(1000 * 2 ** attemptIndex, 30_000)
}

/**
 * Sleep, but give up the moment the query is abandoned.
 *
 * Resolves rather than rejects on abort: the caller re-checks `signal.aborted`
 * straight after, and a rejection here would be indistinguishable from the
 * fetch failure it is waiting to retry.
 */
export function sleep(ms: number, signal: AbortSignal): Promise<void> {
  if (signal.aborted) return Promise.resolve()
  return new Promise<void>((resolve) => {
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', onAbort)
      resolve()
    }, ms)
    function onAbort() {
      clearTimeout(timer)
      resolve()
    }
    signal.addEventListener('abort', onAbort, { once: true })
  })
}

/** Whether the browser believes it is offline. Unknown counts as online. */
export function isOffline(): boolean {
  return typeof navigator !== 'undefined' && navigator.onLine === false
}
