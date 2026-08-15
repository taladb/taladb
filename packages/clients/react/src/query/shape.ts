import type { Doc, QueryKey, ResultShape } from './types'

/**
 * Work out which documents a `queryFn` response contains.
 *
 * This is the deepest mismatch with TanStack Query and the one no rename can
 * absorb: their `data` *is* whatever `queryFn` returned, ours is rebuilt from
 * the local collection. So a response has to reduce to documents, and the three
 * shapes that reduce cleanly are recognised without configuration:
 *
 * - `T[]`  — a list. The common case.
 * - `T`    — one document, for a detail view.
 * - anything else — an envelope, which must say which part of itself is
 *   documents via `documents`, and how to put itself back together via
 *   `assemble`.
 *
 * A response that is none of those fails loudly. It is the one case a codemod
 * cannot fix, so the runtime has to explain it.
 */
export interface ExtractInput {
  raw: unknown
  collection: string
  queryKey: QueryKey
  documents?: (raw: unknown) => Doc[]
}

export interface Extracted {
  documents: Doc[]
  shape: ResultShape
}

export function extractDocuments({
  raw,
  collection,
  queryKey,
  documents,
}: ExtractInput): Extracted {
  if (documents !== undefined) {
    const extracted = documents(raw)
    if (!Array.isArray(extracted)) {
      throw new Error(
        `${describe(queryKey)}: \`documents\` must return an array of documents, but returned ` +
          `${typeName(extracted)}. It selects the part of the response that is stored in the ` +
          `'${collection}' collection.`,
      )
    }
    return { documents: extracted, shape: 'envelope' }
  }

  if (Array.isArray(raw)) return { documents: raw as Doc[], shape: 'array' }

  if (isDocumentLike(raw)) return { documents: [raw], shape: 'document' }

  throw new Error(
    `${describe(queryKey)}: \`queryFn\` returned ${typeName(raw)}, which is neither a document ` +
      'nor a list of documents.\n\n' +
      "Unlike TanStack Query, `data` here is resolved from the '" +
      collection +
      "' collection rather than from the response, so the response has to say which part of " +
      'itself is documents. For an envelope like `{ items, total }`, add:\n\n' +
      '  documents: (raw) => raw.items,\n' +
      '  assemble:  (docs, raw) => ({ ...raw, items: docs }),\n\n' +
      'A document is an object with a string `_id`. If yours uses a different identifier, map it ' +
      'in `queryFn` before returning.',
  )
}

/**
 * A single document, for the detail-view shape.
 *
 * Requires a string `_id` rather than accepting any object, because "any
 * object" would swallow the envelope case — `{ items, total }` is an object
 * too — and silently store the envelope itself as a document.
 */
function isDocumentLike(raw: unknown): raw is Doc {
  return (
    typeof raw === 'object' &&
    raw !== null &&
    !Array.isArray(raw) &&
    typeof (raw as { _id?: unknown })._id === 'string'
  )
}

/** Names a value the way an error message should: concrete, and never `[object Object]`. */
function typeName(value: unknown): string {
  if (value === null) return 'null'
  if (Array.isArray(value)) return 'an array'
  if (typeof value === 'object') {
    const keys = Object.keys(value as object)
    const shown = keys.slice(0, 4).join(', ')
    return keys.length === 0
      ? 'an empty object'
      : `an object with keys { ${shown}${keys.length > 4 ? ', …' : ''} }`
  }
  return `a ${typeof value}`
}

function describe(queryKey: QueryKey): string {
  let rendered: string
  try {
    rendered = JSON.stringify(queryKey)
  } catch {
    rendered = '[unserialisable key]'
  }
  return `useQuery(${rendered})`
}
