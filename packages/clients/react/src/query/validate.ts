import type { PendingWrite, WriteOutcome } from './types'

/**
 * Runtime checks for the backend contract, in development builds only.
 *
 * Every rule here can only be met by the application's server, and every
 * violation fails in a way that is quiet and delayed: a duplicated row, a local
 * copy that drifts, a queue that stalls. A documentation page describing them
 * is read once, if ever. A console warning naming the rule and its consequence
 * is read at the moment it matters.
 *
 * Each call site is guarded by `process.env.NODE_ENV !== 'production'`, so a
 * production bundle drops these entirely.
 */
const PREFIX = '[@taladb/react/query]'

export const isDev = (): boolean =>
  typeof process === 'undefined' || process.env.NODE_ENV !== 'production'

/**
 * Rules 1 and 2 — client ids are authoritative, and writes are idempotent.
 *
 * Same detection, different consequence: on a first attempt a reassigned id
 * means every write will duplicate from now on; on a retry it means this write
 * *just* duplicated, because the server treated the retry as a new record.
 */
export function checkResponseId(op: PendingWrite, body: Record<string, unknown> | null, attempt: number): void {
  if (body === null) return
  const returned = body._id
  if (typeof returned !== 'string' || returned === op.id) return

  console.warn(
    attempt > 0
      ? `${PREFIX} Contract rule 2 (idempotent writes): a retried ${op.type} on ` +
          `"${op.collection}" came back with a different id (sent ${op.id}, received ${returned}). ` +
          'The server treated the retry as a new record, so this write has just been duplicated. ' +
          'Make the endpoint upsert on the supplied `_id`.'
      : `${PREFIX} Contract rule 1 (client ids are authoritative): the endpoint for ` +
          `"${op.collection}" returned id ${returned} for a document sent as ${op.id}. ` +
          'Every write will duplicate on the server. Accept `_id` from the body and do not reassign it.',
  )
}

/** Rule 4 — the response body is the canonical document. */
export function checkResponseBody(op: PendingWrite, body: Record<string, unknown> | null): void {
  if (op.type === 'delete' || body !== null) return
  console.warn(
    `${PREFIX} Contract rule 4 (canonical response body): the endpoint for "${op.collection}" ` +
      `answered a ${op.type} with no JSON document. Server-set fields — timestamps, defaults, ` +
      'computed columns — will be missing locally until the next refetch, with no symptom in between. ' +
      'Return the stored document.',
  )
}

/** Rule 3 — error classes are explicit, and a 4xx is not retryable. */
export function checkClassification(op: PendingWrite, status: number, outcome: WriteOutcome): void {
  if (outcome === 'retry' && status >= 400 && status < 500 && status !== 408 && status !== 429) {
    console.warn(
      `${PREFIX} Contract rule 3 (error classes): classify() returned "retry" for status ${status} ` +
        `on a ${op.type} to "${op.collection}". A 4xx will fail identically forever, so this write ` +
        'will be retried until the tab closes. Return "terminal" for a client error, or "applied" for a 409.',
    )
  }
  if (outcome === 'terminal' && status >= 500) {
    console.warn(
      `${PREFIX} Contract rule 3 (error classes): classify() returned "terminal" for status ${status}. ` +
        'A 5xx may recover; parking it as failed needs a human to retry what a back-off would have fixed.',
    )
  }
}

/** Rule 6 — mutation filters must be `_id`-based. */
export function checkMutationFilter(where: unknown): void {
  if (where === null || typeof where !== 'object') return
  const fields = Object.keys(where as Record<string, unknown>)
  if (fields.length === 1 && fields[0] === '_id') return
  console.warn(
    `${PREFIX} A mutation filter must be { _id }, not { ${fields.join(', ')} }. On a secondary ` +
      'browser tab a filter-based write is forwarded to the primary and re-evaluated there against ' +
      'authoritative data, so it can affect a different set of documents than the UI just showed.',
  )
}

/** Rule 7 — the drain must not run on a secondary tab. */
export function warnDrainNotPrimary(): void {
  console.warn(
    `${PREFIX} A drain cycle was requested on a secondary tab and skipped. This is expected and ` +
      'harmless. Running it there would re-send writes every cycle, because a secondary reads a ' +
      "pending set that lags, and its own bookkeeping lands at the primary later.",
  )
}
