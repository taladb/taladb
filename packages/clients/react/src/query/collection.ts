import { warnOnce } from './dev'
import type { QueryKey } from './types'

/**
 * Work out which collection a query's documents belong in.
 *
 * This is the one option TanStack Query has no analogue for, and the one most
 * worth getting right: it is not a cache label, it is the physical destination.
 * `hydrate` writes every fetched document into `db.collection(name)`, so a wrong
 * value does not degrade a cache — it pours server documents into a collection
 * the application reads for something else, or conjures one nobody declared.
 * There is no cache-miss failure mode here to soften the landing.
 *
 * So inference is allowed, but never trusted: the provider's `collections`
 * registry is the list of legal answers, and a guess that is not on it is an
 * error rather than a new collection.
 */
export interface ResolveCollectionInput {
  /** Explicit option from the call site. Always wins. */
  collection?: string
  queryKey: QueryKey
  /** Provider-level override, for keys that do not lead with the collection. */
  resolve?: (queryKey: QueryKey) => string | undefined
  /** Registered collection names, from the `TalaDBProvider` registry. */
  registered: string[]
}

export function resolveCollectionName({
  collection,
  queryKey,
  resolve,
  registered,
}: ResolveCollectionInput): string {
  if (collection !== undefined) return collection

  const resolved = resolve?.(queryKey)
  const inferred = resolved ?? (typeof queryKey[0] === 'string' ? queryKey[0] : undefined)

  if (inferred === undefined || inferred === '') {
    throw new Error(
      `${describe(queryKey)} has no collection: its first key segment is not a string, so there ` +
        'is nothing to infer from. Pass `collection: \'…\'` on the query, or a ' +
        '`resolveCollection` function to <QueryProvider>.',
    )
  }

  // An empty registry is legal — `collections` is optional on TalaDBProvider —
  // and there is then nothing to check against. Refusing to work would punish
  // the smallest applications hardest, so this warns instead.
  if (registered.length === 0) {
    warnOnce(
      inferred,
      `${describe(queryKey)} is storing documents in the '${inferred}' collection, inferred from ` +
        'its first key segment. No collections are registered on <TalaDBProvider>, so this could ' +
        'not be checked — register them to get schema validation and a real error on a typo.',
    )
    return inferred
  }

  if (!registered.includes(inferred)) {
    throw new Error(
      `${describe(queryKey)} would store documents in a '${inferred}' collection, inferred from ` +
        `its first key segment, but the registered collections are: ${registered.join(', ')}. ` +
        'Either pass `collection` explicitly, or give <QueryProvider> a `resolveCollection` ' +
        'function if your query keys do not start with the collection name.',
    )
  }

  return inferred
}

/** Identifies the offending query in an error without dumping the whole key. */
function describe(queryKey: QueryKey): string {
  let rendered: string
  try {
    rendered = JSON.stringify(queryKey)
  } catch {
    rendered = '[unserialisable key]'
  }
  return `useQuery(${rendered})`
}

/** @deprecated Use `resetWarnings` from `./dev`, which clears every notice. */
export { resetWarnings as resetCollectionWarnings } from './dev'
