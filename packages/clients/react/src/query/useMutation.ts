import { useCallback, useEffect, useRef, useState } from 'react'
import type { Document } from 'taladb'
import { useCollection } from '../useCollection'
import { useQueryContext } from './context'
import { newDocId } from './keys'
import { writeConfirmed, writeLocal } from './mutate'
import { sendWrite, toPendingWrite } from './send'
import type { MutationOp, MutationResult, UseMutationOptions } from './types'

/**
 * Write to a collection.
 *
 * Under the default `optimistic` mode the write commits locally and returns;
 * the network happens in the drain loop, and `pending` stays false throughout.
 * That is the point — the UI never waits on a round trip it does not need to
 * wait on, and the write survives a reload, a crash, or a flight.
 *
 * `remote-first` is the deliberate exception: it awaits the request and writes
 * locally only once the server has confirmed. Use it where showing success
 * before the server agrees would mislead — checkout, payment, anything the user
 * would act on irreversibly.
 *
 * `where` is `{ _id }` by construction rather than an arbitrary filter. On a
 * secondary browser tab a filter-based write is forwarded to the primary and
 * re-evaluated there against authoritative data, so it can touch a different
 * set of documents than the optimistic UI just showed.
 */
export function useMutation<T extends Document>(
  options: UseMutationOptions,
): MutationResult<T> {
  const { collection: name, mode = 'optimistic' } = options
  const { backend, register, requestDrain } = useQueryContext()
  const collection = useCollection<T>(name)

  const [pending, setPending] = useState(false)
  const [error, setError] = useState<unknown | null>(null)

  // Declared here rather than on first write, so a queue left over from a
  // previous session drains as soon as the component mounts.
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
      try {
        if (mode === 'optimistic') {
          await writeLocal(collection, op, Date.now())
          // The write is committed and durable; this only asks the drain to
          // look sooner than its next scheduled cycle.
          requestDrain()
          return
        }

        if (backend === null) {
          throw new Error(
            "useMutation({ mode: 'remote-first' }) needs a backend. Pass one to " +
              '<QueryProvider backend={defineBackend({ … })}>.',
          )
        }

        setIfMounted(setPending, true)
        // The id must exist before the request, not after: client ids are
        // authoritative, and a retried insert is only idempotent if it carries
        // the same id both times.
        const id = op.type === 'insert' ? (op.doc._id ?? newDocId(name)) : op.where._id
        const document =
          op.type === 'delete'
            ? null
            : op.type === 'insert'
              ? { ...op.doc, _id: id }
              : { ...op.set, _id: id }

        const result = await sendWrite(backend, toPendingWrite(name, id, op.type, document))

        if (result.outcome === 'retry' || result.outcome === 'terminal') {
          throw new Error(
            `The server rejected this ${op.type} with status ${result.status}. ` +
              'Nothing was written locally.',
          )
        }

        // The response body is canonical, so server-set fields land without a
        // second fetch. `applied` carries none — the server had already done it.
        if (op.type === 'delete') {
          await collection.deleteOne({ _id: id } as never)
        } else {
          await writeConfirmed(collection, (result.document ?? document) as T, Date.now())
        }
      } catch (cause: unknown) {
        setIfMounted(setError, cause)
        throw cause
      } finally {
        setIfMounted(setPending, false)
      }
    },
    [collection, name, mode, backend, requestDrain],
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
