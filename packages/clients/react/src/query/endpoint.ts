import type { SyncOp, WriteOutcome } from './types'

/** The verb each operation uses unless the caller overrides it. */
export const DEFAULT_METHODS: Record<SyncOp, string> = {
  insert: 'POST',
  update: 'PUT',
  delete: 'DELETE',
}

/**
 * Resolve a URL template for one write.
 *
 * A template rather than a function because a queued write is sent long after
 * the component that made it is gone — possibly after a reload — so the route
 * has to be storable alongside the document. `:id` and `:collection`
 * interpolate.
 *
 * An insert has no id in the path: `/api/todos/:id` becomes `/api/todos`. That
 * matches how REST endpoints are actually shaped, so one template covers all
 * three verbs instead of forcing a branch at every call site.
 */
export function resolveUrl(template: string, collection: string, id: string, type: SyncOp): string {
  const withCollection = template.split(':collection').join(collection)
  if (type === 'insert') {
    // Drop a trailing id segment; an embedded `:id` still interpolates, for the
    // rare endpoint shaped like `/api/:id/todos`.
    const trimmed = withCollection.replace(/\/:id\/?$/, '')
    return trimmed.split(':id').join(id)
  }
  return withCollection.split(':id').join(id)
}

/**
 * How a response is read when the application has not supplied a `classify`.
 *
 * Deliberately conservative: only a 409 is assumed to mean "already applied",
 * because that is the one convention universal enough to guess at. Everything
 * else follows the HTTP status class.
 */
export function defaultClassify(response: Response): WriteOutcome {
  if (response.ok) return 'ok'
  if (response.status === 409) return 'applied'
  if (response.status >= 500 || response.status === 408 || response.status === 429) return 'retry'
  return 'terminal'
}
