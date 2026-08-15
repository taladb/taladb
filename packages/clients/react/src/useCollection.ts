import { useMemo, useRef } from 'react'
import type { Collection, CollectionOptions, Document } from 'taladb'
import { useTalaDB, useCollectionOptions } from './context'

/**
 * Returns a stable `Collection<T>` handle from the nearest `<TalaDBProvider>`.
 *
 * The collection is opened **with its registered options** — the `schema`,
 * `syncSchema` and `migrateDocument` declared in the provider's `collections`
 * prop, or the `options` passed here (which win). That is what makes a write
 * through `useWrite` hard-fail on an invalid document and carry its `_v`
 * shape version, exactly as `db.collection(name, { … })` does. Without it the
 * hooks resolve a bare, unconfigured handle and silently skip validation.
 *
 * The returned collection is memoised — the same object reference is returned on
 * every render unless the db instance or collection name changes. Pass it
 * directly to `useFind` or `useFindOne` without wrapping in `useMemo`. Options
 * are read when the handle is first created and treated as static configuration,
 * so an inline `{ schema }` object cannot thrash live-query subscriptions.
 *
 * @param name     The collection name (e.g. `'articles'`).
 * @param options  Per-call options; overrides the provider's registry entry.
 *
 * @example
 * // Registered once on the provider — every hook below it picks this up:
 * <TalaDBProvider name="app.db" collections={{ articles: { schema: Article } }}>
 *
 * const articles = useCollection<Article>('articles')
 * const { data, loading } = useFind(articles, { locale: 'en' })
 */
export function useCollection<T extends Document>(
  name: string,
  options?: CollectionOptions<T>,
): Collection<T> {
  const db = useTalaDB()
  const registry = useCollectionOptions()

  // Read through a ref so an inline options object does not change the handle's
  // identity on every render (which would tear down and re-run live queries).
  const explicit = useRef(options)
  explicit.current = options

  return useMemo(
    () => db.collection<T>(name, explicit.current ?? registry.get<T>(name)),
    [db, name, registry],
  )
}
