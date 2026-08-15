import { useCallback, useEffect, useRef, useState } from 'react'
import type { Document } from 'taladb'
import { useCollection } from '../useCollection'
import { useQueryContext } from './context'
import { DEFAULT_METHODS, defaultClassify, resolveUrl } from './endpoint'
import { newDocId } from './keys'
import { writeConfirmed, writeLocal } from './mutate'
import type { MutationOp, MutationResult, UseMutationOptions } from './types'
import { checkMutationFilter, isDev } from './validate'

/**
 * Write to a collection.
 *
 * Routing lives here, at the call site, because it varies per resource:
 *
 * ```ts
 * const { mutate } = useMutation<Todo>({
 *   collection: 'todos',
 *   url: '/api/v2/todos/:id',
 *   method: { update: 'PATCH' },
 * })
 * ```
 *
 * `url` is a template rather than a function for one reason: under `optimistic`
 * and `queued` the request is sent by the drain loop, long after this component
 * is gone — after a route change, a reload, a week. A template is data and can
 * be stored on the queued document; a closure cannot.
 *
 * Auth and error policy stay on `<QueryProvider>`. `headers` in particular has
 * to be resolved at send time, so a write queued offline carries a current
 * token instead of the expired one it was made with.
 *
 * `where` is `{ _id }` by construction. On a secondary browser tab a
 * filter-based write is forwarded to the primary and re-evaluated there against
 * authoritative data, so it could touch a different set of documents than the
 * UI just showed.
 */
export function useMutation<T extends Document>(options: UseMutationOptions): MutationResult<T> {
  const { collection: name, url, method, mode = 'optimistic' } = options
  const { backend, register, requestDrain } = useQueryContext()
  const collection = useCollection<T>(name)

  const [pending, setPending] = useState(false)
  const [error, setError] = useState<unknown | null>(null)

  // Declared on mount, not on first write, so a queue left from a previous
  // session drains as soon as this component appears.
  useEffect(() => {
    register(name)
  }, [register, name])

  const mountedRef = useRef(true)
  const setIfMounted = <V,>(set: (v: V) => void, value: V) => {
    if (mountedRef.current) set(value)
  }

  const mutateAsync = useCallback(
    async (op: MutationOp<T>): Promise<void> => {
      setIfMounted(setError, null)
      if (isDev() && op.type !== 'insert') checkMutationFilter(op.where)

      try {
        if (mode !== 'immediate') {
          await writeLocal(collection, op, Date.now(), { url, method })
          // `queued` waits for the provider's schedule; `optimistic` asks for a
          // cycle now. Either way the write is already committed and durable.
          if (mode === 'optimistic') requestDrain()
          return
        }

        // `immediate`: an ordinary fetch, then the local write. Nothing is
        // stored until the server agrees, so there is nothing to roll back.
        if (url === undefined) {
          throw new Error(
            "useMutation({ mode: 'immediate' }) needs a `url` — the request is sent " +
              'from here, so there is nothing to route it by otherwise.',
          )
        }

        setIfMounted(setPending, true)
        // The id exists before the request, not after: client ids are
        // authoritative, and a retried insert only avoids duplicating if it
        // carries the same id both times.
        const id = op.type === 'insert' ? (op.doc._id ?? newDocId(name)) : op.where._id
        const body =
          op.type === 'delete'
            ? null
            : op.type === 'insert'
              ? { ...op.doc, _id: id }
              : { ...op.set, _id: id }

        const verb = method?.[op.type] ?? backend?.method[op.type] ?? DEFAULT_METHODS[op.type]
        const headers = new Headers(backend?.headers ? await backend.headers() : undefined)
        if (body !== null && !headers.has('content-type')) {
          headers.set('content-type', 'application/json')
        }

        const doFetch = backend?.fetch ?? globalThis.fetch
        const response = await doFetch(resolveUrl(url, name, id, op.type), {
          method: verb,
          headers,
          body: body === null ? undefined : JSON.stringify(body),
        })

        const outcome = backend?.classify
          ? backend.classify(response, { collection: name, id, type: op.type, document: body })
          : defaultClassify(response)

        if (outcome === 'retry' || outcome === 'terminal') {
          throw new Error(
            `The server rejected this ${op.type} with status ${response.status}. ` +
              'Nothing was written locally.',
          )
        }

        if (op.type === 'delete') {
          await collection.deleteOne({ _id: id } as never)
        } else {
          // The response body is canonical, so server-set fields land without a
          // second fetch. A 409 carries none — the server had already done it.
          const canonical = await readJson(response)
          await writeConfirmed(collection, (canonical ?? body) as T, Date.now())
        }
      } catch (cause: unknown) {
        setIfMounted(setError, cause)
        throw cause
      } finally {
        setIfMounted(setPending, false)
      }
    },
    [collection, name, mode, url, method, backend, requestDrain],
  )

  const mutate = useCallback(
    (op: MutationOp<T>): void => {
      void mutateAsync(op).catch(() => {
        /* surfaced on `error`; swallowed so it is not an unhandled rejection */
      })
    },
    [mutateAsync],
  )

  return { mutate, mutateAsync, pending, error }
}

async function readJson(response: Response): Promise<Record<string, unknown> | null> {
  try {
    const parsed: unknown = await response.json()
    return parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null
  } catch {
    return null
  }
}
