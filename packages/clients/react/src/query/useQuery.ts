import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { Document } from 'taladb'
import { useCollection } from '../useCollection'
import { useFind } from '../useFind'
import { useQueryContext } from './context'
import { hydrate } from './hydrate'
import { hashQueryKey } from './keys'
import { ensureEnvelopeIndexes, isStale, readQueryRecord, writeQueryRecord } from './queries'
import type { Enveloped } from './envelope'
import type { QueryResult, UseQueryOptions } from './types'

/** Matches nothing, for a query whose result set is empty or not yet known. */
const MATCH_NONE = { _id: { $in: [] as string[] } }

/**
 * Read a slice of a collection, hydrated from a server and revalidated in the
 * background.
 *
 * `data` is resolved through the local database rather than from the fetch
 * response, so a local write to any document in the set re-renders this query
 * immediately — there is no cache to invalidate, because the collection *is*
 * the state. Documents arrive in the order the server returned them.
 *
 * On a cold key the hook fetches and reports `loading`. On a warm key it
 * renders what it has straight away and revalidates behind that, reporting
 * `stale` while it does — a reload never shows a spinner over data the device
 * already holds.
 */
export function useQuery<T extends Document>(options: UseQueryOptions<T>): QueryResult<T> {
  const { collection: name, key, ttl = 0, enabled = true } = options
  const { queries, register } = useQueryContext()
  const collection = useCollection<T>(name)

  const [ids, setIds] = useState<string[]>([])
  const [loading, setLoading] = useState(true)
  const [stale, setStale] = useState(false)
  const [error, setError] = useState<unknown | null>(null)

  // Serialised, so an inline key literal does not restart the effect each render.
  const keyHash = hashQueryKey(key)

  // Read through refs so `fetch` may be an inline arrow without re-triggering.
  const fetchRef = useRef(options.fetch)
  fetchRef.current = options.fetch
  const ttlRef = useRef(ttl)
  ttlRef.current = ttl

  useEffect(() => {
    register(name)
  }, [register, name])

  const mountedRef = useRef(true)
  useEffect(() => {
    mountedRef.current = true
    return () => {
      mountedRef.current = false
    }
  }, [])

  const revalidate = useCallback(
    async (signal: AbortSignal): Promise<void> => {
      if (queries === null) return
      const docs = await fetchRef.current({ signal })
      if (signal.aborted) return

      const now = Date.now()
      await hydrate(collection, docs, now)
      const nextIds = docs.map((doc) => doc._id as string)
      await writeQueryRecord(queries, name, key, nextIds, now, ttlRef.current)

      if (signal.aborted || !mountedRef.current) return
      setIds(nextIds)
      setStale(false)
      setError(null)
    },
    // `key` is covered by keyHash; including the array itself would defeat it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [queries, collection, name, keyHash],
  )

  useEffect(() => {
    if (!enabled || queries === null) return
    const controller = new AbortController()

    void (async () => {
      try {
        await ensureEnvelopeIndexes(collection as never)
        const record = await readQueryRecord(queries, name, key)
        if (controller.signal.aborted) return

        if (record === null) {
          // Cold: nothing to show, so this is the one case that reports loading.
          setLoading(true)
          await revalidate(controller.signal)
        } else {
          // Warm: render immediately, then refresh behind it if it has aged out.
          if (mountedRef.current) {
            setIds(record.ids)
            setLoading(false)
          }
          if (isStale(record, Date.now())) {
            if (mountedRef.current) setStale(true)
            await revalidate(controller.signal)
          }
        }
      } catch (cause: unknown) {
        // A failed refresh must not clear data the device already holds.
        if (!controller.signal.aborted && mountedRef.current) setError(cause)
      } finally {
        if (!controller.signal.aborted && mountedRef.current) {
          setLoading(false)
          setStale(false)
        }
      }
    })()

    return () => controller.abort()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, queries, collection, name, keyHash, revalidate])

  const refetch = useCallback(async (): Promise<void> => {
    const controller = new AbortController()
    try {
      setStale(true)
      await revalidate(controller.signal)
    } catch (cause: unknown) {
      if (mountedRef.current) setError(cause)
    } finally {
      if (mountedRef.current) setStale(false)
    }
  }, [revalidate])

  // Live view of the documents this query resolved to.
  const filter = useMemo(() => (ids.length === 0 ? MATCH_NONE : { _id: { $in: ids } }), [ids])
  const { data: found } = useFind<T>(collection, filter as never)

  const data = useMemo(() => {
    const byId = new Map<string, T>()
    for (const doc of found) byId.set(doc._id as string, doc)
    const ordered: T[] = []
    for (const id of ids) {
      const doc = byId.get(id)
      // A tombstone is a delete this device has queued but the server has not
      // confirmed. It is still stored — that is what keeps the queued operation
      // alive — but it must not be rendered.
      if (doc !== undefined && (doc as Enveloped<T>)._op !== 'delete') ordered.push(doc)
    }
    return ordered
  }, [found, ids])

  return { data, loading, stale, error, refetch }
}
