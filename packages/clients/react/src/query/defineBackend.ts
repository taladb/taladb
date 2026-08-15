import type { BackendDefinition, ResolvedBackend, SyncOp } from './types'

/**
 * The verb each operation uses unless the backend says otherwise. Chosen so a
 * REST service can route on method alone — the same mapping the change webhook
 * already uses, so a receiver written for one works with the other.
 */
const DEFAULT_METHODS: Record<SyncOp, string> = {
  insert: 'POST',
  update: 'PUT',
  delete: 'DELETE',
}

/**
 * Describe the backend the drain loop should talk to.
 *
 * The library owns a local queue and a `fetch`; it owns no transport, no
 * cursors, and no conflict resolution. Everything it needs to know about the
 * server goes through here, so nothing is guessed.
 *
 * The backend must hold up five rules, or the queue misbehaves in ways no
 * amount of client code can fix. In development these are checked at runtime
 * and reported with the rule they broke:
 *
 * 1. **Client ids are authoritative** — accept `_id` from the body, never
 *    reassign it. Everything else depends on this one.
 * 2. **Writes are idempotent** — a retry after a timeout must not duplicate.
 * 3. **Error classes are explicit** — see {@link BackendDefinition.classify}.
 * 4. **The response body is the canonical document** — so server-set fields
 *    reach the client without a second fetch.
 * 5. **No cross-document ordering requirements** — per-document order only.
 *
 * @example
 * const backend = defineBackend({
 *   url: (op) => op.type === 'insert'
 *     ? `/api/${op.collection}`
 *     : `/api/${op.collection}/${op.id}`,
 *   headers: () => ({ Authorization: `Bearer ${getToken()}` }),
 *   classify: (res) =>
 *     res.ok             ? 'ok'
 *   : res.status === 409 ? 'applied'
 *   : res.status >= 500  ? 'retry'
 *   :                      'terminal',
 * })
 */
export function defineBackend(definition: BackendDefinition): ResolvedBackend {
  return {
    ...definition,
    method: { ...DEFAULT_METHODS, ...definition.method },
  }
}
