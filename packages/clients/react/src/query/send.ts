import type { PendingWrite, ResolvedBackend, WriteOutcome } from './types'
import { checkClassification, checkResponseBody, checkResponseId, isDev } from './validate'

export interface SendResult {
  outcome: WriteOutcome
  /** The canonical document, when the response carried one. */
  document: Record<string, unknown> | null
  status: number
}

/**
 * Send one queued write to the application's backend and classify the result.
 *
 * Shared by `remote-first` mutations and the drain loop, so both agree on what
 * a 409 means and neither invents its own retry policy.
 */
export async function sendWrite(
  backend: ResolvedBackend,
  op: PendingWrite,
  signal?: AbortSignal,
  attempt = 0,
): Promise<SendResult> {
  const doFetch = backend.fetch ?? globalThis.fetch
  const headers = new Headers(backend.headers ? await backend.headers() : undefined)
  const body = op.type === 'delete' ? undefined : JSON.stringify(op.document)
  if (body !== undefined && !headers.has('content-type')) {
    headers.set('content-type', 'application/json')
  }

  const response = await doFetch(backend.url(op), {
    method: backend.method[op.type],
    headers,
    body,
    signal,
  })

  const outcome = backend.classify(response, op)
  const document = await readDocument(response, outcome)

  if (isDev()) {
    checkClassification(op, response.status, outcome)
    if (outcome === 'ok') {
      checkResponseId(op, document, attempt)
      checkResponseBody(op, document)
    }
  }

  return { outcome, document, status: response.status }
}

/**
 * Read the canonical document from a successful response.
 *
 * A body is only meaningful on `ok`: `applied` means the server had already
 * done this and has nothing new to say, and the failure outcomes carry an error
 * rather than a document. A malformed or empty body is not an error here —
 * contract rule 4 is enforced by the dev validator, which can explain it
 * properly, rather than by throwing inside the drain.
 */
async function readDocument(
  response: Response,
  outcome: WriteOutcome,
): Promise<Record<string, unknown> | null> {
  if (outcome !== 'ok') return null
  try {
    const parsed: unknown = await response.json()
    return parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null
  } catch {
    return null
  }
}

/** Describe a stored document as the write the backend should receive. */
export function toPendingWrite(
  collection: string,
  id: string,
  type: PendingWrite['type'],
  document: Record<string, unknown> | null,
): PendingWrite {
  return { collection, id, type, document: type === 'delete' ? null : document }
}
