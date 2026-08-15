import { useCallback, useState } from 'react'
import type { Document, Filter, Update } from 'taladb'
import { useCollection } from './useCollection'

/** A single write intent. Discriminated on `type`. */
export type WriteOp<T extends Document> =
  | { type: 'insert'; doc: Omit<T, '_id'> }
  | { type: 'update'; where: Filter<T>; set: Partial<Omit<T, '_id'>> }
  | { type: 'delete'; where: Filter<T> }

export interface UseWriteOptions {
  /** Collection the write targets. */
  collection: string
}

export interface WriteResult<T extends Document> {
  /** Fire-and-forget write. Errors surface on `error`, never thrown to render. */
  write: (op: WriteOp<T>) => void
  /** Awaitable write. Resolves once the write commits; rejects on error. */
  writeAsync: (op: WriteOp<T>) => Promise<void>
  /** A write is in flight. */
  pending: boolean
  /** Most recent write error. */
  error: unknown | null
}

/**
 * Write hook over a TalaDB collection.
 *
 * The write is local, immediate, durable, and reactive — every `useFind` /
 * `useFindOne` / `useAggregate` subscribed to the collection re-renders once it
 * commits. There is no network step in this hook and no rollback to reason
 * about: the database is on the device, and the write either committed or threw.
 *
 * Named `useWrite`, not `useMutation`, because it is exactly a local write and
 * nothing more. The React Query-shaped name belongs to a hook that also owns a
 * network round-trip; this one would only borrow the expectation and then fail
 * to meet it.
 *
 * If the application has enabled the change webhook (`openDB({ webhook })`),
 * the resulting HTTP request is dispatched by the client after the commit,
 * outside this hook and outside `pending` — a webhook is a notification, not
 * part of the write's success.
 *
 * @example
 * const { write, pending } = useWrite<Order>({ collection: 'orders' })
 * write({ type: 'update', where: { _id }, set: { status: 'shipped' } })
 */
export function useWrite<T extends Document>(options: UseWriteOptions): WriteResult<T> {
  const { collection } = options
  const col = useCollection<T>(collection)

  const [pending, setPending] = useState(false)
  const [error, setError] = useState<unknown | null>(null)

  const apply = useCallback(
    async (op: WriteOp<T>): Promise<void> => {
      switch (op.type) {
        case 'insert':
          await col.insert(op.doc)
          return
        case 'update':
          await col.updateOne(op.where, { $set: op.set } as Update<T>)
          return
        case 'delete':
          await col.deleteOne(op.where)
          return
      }
    },
    [col],
  )

  const writeAsync = useCallback(
    async (op: WriteOp<T>): Promise<void> => {
      setPending(true)
      setError(null)
      try {
        await apply(op)
      } catch (e) {
        setError(e)
        throw e
      } finally {
        setPending(false)
      }
    },
    [apply],
  )

  const write = useCallback(
    (op: WriteOp<T>): void => {
      void writeAsync(op).catch(() => {
        /* surfaced on `error`; swallow so it doesn't become an unhandled rejection */
      })
    },
    [writeAsync],
  )

  return { write, writeAsync, pending, error }
}
