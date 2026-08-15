import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { Document } from 'taladb'
import { useTalaDB } from '../context'
import { useQueryContext } from './context'
import type { FailedWrite, SyncStatus, SyncOp } from './types'

interface Slice {
  pending: number
  failed: FailedWrite[]
}

const QUEUED = { _sync: { $in: ['pending', 'failed'] } }

/**
 * What the write queue is doing, live.
 *
 * Optimistic writes are the point of this layer and its biggest hazard: a
 * request that fails after the user has navigated away has no UI left to fail
 * into. Without something rendering this, a write can be lost with no symptom
 * at all — which is how local-first libraries lose people's trust. Show
 * `pending` somewhere quiet and `failed` somewhere loud.
 *
 * A terminal failure is never retried on its own. It needs a decision:
 * `retry` when the cause has been fixed server-side, `discard` to stop trying.
 */
export function useSyncStatus(): SyncStatus {
  const db = useTalaDB()
  const { collections, draining, requestDrain } = useQueryContext()

  const slicesRef = useRef(new Map<string, Slice>())
  const [version, bump] = useState(0)
  const [online, setOnline] = useState(() =>
    typeof navigator === 'undefined' ? true : navigator.onLine,
  )

  useEffect(() => {
    if (typeof window === 'undefined') return
    const update = () => setOnline(navigator.onLine)
    window.addEventListener('online', update)
    window.addEventListener('offline', update)
    return () => {
      window.removeEventListener('online', update)
      window.removeEventListener('offline', update)
    }
  }, [])

  // One live subscription per managed collection, merged into a single view.
  // Subscribing rather than polling means a failure appears the moment the
  // drain records it, not on the next render for some other reason.
  useEffect(() => {
    const unsubscribes = collections.map((name) => {
      const collection = db.collection<Document>(name)
      return collection.subscribe(QUEUED as never, (docs) => {
        slicesRef.current.set(name, summarise(name, docs))
        bump((n) => n + 1)
      })
    })
    return () => {
      for (const unsubscribe of unsubscribes) unsubscribe()
    }
  }, [db, collections])

  const { pending, failed } = useMemo(() => {
    let total = 0
    const failures: FailedWrite[] = []
    for (const slice of slicesRef.current.values()) {
      total += slice.pending
      failures.push(...slice.failed)
    }
    return { pending: total, failed: failures }
    // `version` is the dependency that matters: the ref holds the data, and a
    // subscription firing is what bumps it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version])

  const findCollection = useCallback(
    async (id: string) => {
      for (const name of collections) {
        const collection = db.collection<Document>(name)
        if ((await collection.findOne({ _id: id })) !== null) return collection
      }
      return null
    },
    [db, collections],
  )

  const retry = useCallback(
    (id: string) => {
      void (async () => {
        const collection = await findCollection(id)
        if (collection === null) return
        await collection.updateOne(
          { _id: id },
          { $set: { _sync: 'pending', _attempt: 0, _error: null, _retry_at: 0 } } as never,
        )
        requestDrain()
      })()
    },
    [findCollection, requestDrain],
  )

  const discard = useCallback(
    (id: string) => {
      void (async () => {
        const collection = await findCollection(id)
        if (collection === null) return
        const doc = await collection.findOne({ _id: id })
        if (doc === null) return

        // An insert the server never accepted leaves nothing behind. Any other
        // discarded write keeps the document and marks it settled — the local
        // values stay until the next fetch of a query containing it replaces
        // them, because there is no previous version stored to revert to.
        if (doc._op === 'insert') {
          await collection.deleteOne({ _id: id })
        } else {
          await collection.updateOne(
            { _id: id },
            { $set: { _sync: 'synced', _op: null, _attempt: 0, _error: null, _retry_at: 0 } } as never,
          )
        }
      })()
    },
    [findCollection],
  )

  return { pending, failed, draining, online, retry, discard }
}

function summarise(collection: string, docs: Document[]): Slice {
  let pending = 0
  const failed: FailedWrite[] = []
  for (const doc of docs) {
    if (doc._sync === 'pending') {
      pending++
    } else if (doc._sync === 'failed') {
      failed.push({
        collection,
        id: doc._id as string,
        op: (doc._op ?? 'update') as SyncOp,
        error: (doc._error as string) ?? 'Unknown error',
        attempts: (doc._attempt as number) ?? 0,
      })
    }
  }
  return { pending, failed }
}
