import type { Document, Filter } from 'taladb'

/**
 * Request parameters, declared once and used three times.
 *
 * An application already has `?category=book&page=1&sort=-createdAt`. Restating
 * `category` a second time as an engine predicate is duplication that drifts, so
 * one declaration feeds the query key, the request, and the local filter.
 *
 * **Nothing is inferred.** The obvious convention — "a param whose name matches
 * a document field is a predicate, anything else is pagination" — has to be
 * rejected, because there is no reliable way to know a collection's field names:
 * schemas are Standard Schema values with no portable key enumeration, and
 * sampling documents fails exactly when it matters most, on a cold empty
 * collection. Guessing wrong there is silent: `page: 1` treated as a predicate
 * matches nothing and renders an empty screen with `status: 'success'`.
 *
 * So an undeclared param contributes no local predicate. It still reaches the
 * key and the request; it simply does not narrow the local read. The local
 * filter is then *broader* than the server's, never narrower — and being
 * broader costs nothing, because the query's stored id list still bounds the
 * result.
 */

export type ParamOp = '$eq' | '$ne' | '$gt' | '$gte' | '$lt' | '$lte' | '$in' | '$contains'

export type ParamPrimitive = string | number | boolean
export type ParamValue = ParamPrimitive | ParamPrimitive[] | null | undefined

/**
 * What one request parameter means.
 *
 * `TValue` is carried only for inference, so `defineParams` can type its own
 * call signature from the declaration.
 */
export interface ParamSpec<TValue = ParamValue> {
  readonly kind: 'predicate' | 'shape'
  readonly field?: string
  readonly op?: ParamOp
  /** Phantom. Never read at runtime. */
  readonly __value?: TValue
}

const predicate = <TValue>(field: string, op: ParamOp): ParamSpec<TValue> => ({
  kind: 'predicate',
  field,
  op,
})

/** `?status=open` → `{ status: 'open' }` */
export const eq = (field: string): ParamSpec<ParamPrimitive> =>
  predicate<ParamPrimitive>(field, '$eq')

/** `?exclude=archived` → `{ status: { $ne: 'archived' } }` */
export const ne = (field: string): ParamSpec<ParamPrimitive> =>
  predicate<ParamPrimitive>(field, '$ne')

/** `?minPrice=10` → `{ price: { $gte: 10 } }` */
export const gte = (field: string): ParamSpec<number | string> =>
  predicate<number | string>(field, '$gte')

/** `?maxPrice=99` → `{ price: { $lte: 99 } }` */
export const lte = (field: string): ParamSpec<number | string> =>
  predicate<number | string>(field, '$lte')

/** `?after=2026-01-01` → `{ createdAt: { $gt: … } }` */
export const gt = (field: string): ParamSpec<number | string> =>
  predicate<number | string>(field, '$gt')

/** `?before=2026-01-01` → `{ createdAt: { $lt: … } }` */
export const lt = (field: string): ParamSpec<number | string> =>
  predicate<number | string>(field, '$lt')

/** `?search=dune` → `{ title: { $contains: 'dune' } }` */
export const contains = (field: string): ParamSpec<string> => predicate<string>(field, '$contains')

/** `?tags=a,b` → `{ tags: { $in: ['a', 'b'] } }` */
export const oneOf = (field: string): ParamSpec<ParamPrimitive[]> =>
  predicate<ParamPrimitive[]>(field, '$in')

/**
 * A parameter that shapes the *response* rather than selecting documents —
 * `page`, `perPage`, `sort`, `cursor`.
 *
 * These have no local meaning at all: the stored id list already *is* the slice,
 * in the server's order. Declaring them explicitly is what turns a typo'd param
 * name into something visible rather than a param that quietly stops filtering.
 */
export const shape = (): ParamSpec<ParamPrimitive> => ({ kind: 'shape' })

/** A parameter set with its values bound, ready for a request and a query. */
export interface BoundParams<TValues = Record<string, ParamValue>> {
  /** The values as given. */
  readonly values: TValues
  /**
   * The request half. Empty and `null` values are dropped; arrays are
   * comma-joined; booleans become `true`/`false`.
   */
  toSearchParams(): URLSearchParams
  /**
   * The collection half — `undefined` when no declared parameter is set, which
   * is the case for a plain object.
   */
  toFilter(): Filter<Document> | undefined
  /** Declared parameters of kind `shape` that currently have a value. */
  shapeParams(): string[]
}

type Spec = Record<string, ParamSpec<never>>

/** The value type a declaration accepts. Every parameter is optional. */
export type ValuesOf<S> = {
  [K in keyof S]?: S[K] extends ParamSpec<infer V> ? V : never
}

/**
 * Declare a parameter set.
 *
 * Returns a function that binds values to it. The value type is inferred from
 * the declaration, so a typo is a compile error rather than a parameter that
 * silently stops filtering — protection a stringly-typed `?category=` never
 * gives you.
 */
export function defineParams<S extends Record<string, ParamSpec<never>>>(
  spec: S,
): (values: ValuesOf<S>) => BoundParams<ValuesOf<S>> {
  return (values) => bindParams(spec as Spec, values as Record<string, ParamValue>) as never
}

/** True for anything already bound — a declaration's output, not a plain object. */
export function isBoundParams(value: unknown): value is BoundParams {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as BoundParams).toFilter === 'function'
  )
}

/**
 * Accept either form.
 *
 * A plain object gets the request half and no local predicates — which is the
 * safe default, and the reason upgrading to `defineParams` is additive rather
 * than a rewrite.
 */
export function normalizeParams(
  params: BoundParams | Record<string, ParamValue> | undefined,
): BoundParams | undefined {
  if (params === undefined) return undefined
  if (isBoundParams(params)) return params
  return bindParams({}, params)
}

function bindParams(spec: Spec, values: Record<string, ParamValue>): BoundParams {
  return {
    values,
    toSearchParams: () => {
      const search = new URLSearchParams()
      for (const [name, value] of Object.entries(values)) {
        if (value === undefined || value === null) continue
        // An empty array means "no constraint", not "match nothing", so it is
        // dropped rather than sent as `?tags=`.
        if (Array.isArray(value)) {
          if (value.length > 0) search.set(name, value.join(','))
          continue
        }
        search.set(name, String(value))
      }
      return search
    },
    toFilter: () => buildFilter(spec, values),
    shapeParams: () =>
      Object.keys(values).filter(
        (name) => spec[name]?.kind === 'shape' && values[name] !== undefined,
      ),
  }
}

function buildFilter(spec: Spec, values: Record<string, ParamValue>): Filter<Document> | undefined {
  const clauses: Record<string, unknown>[] = []

  for (const [name, value] of Object.entries(values)) {
    if (value === undefined || value === null) continue
    const declared = spec[name]
    // Undeclared, or declared as shaping the response: no local predicate.
    if (declared === undefined || declared.kind !== 'predicate') continue
    if (declared.field === undefined || declared.op === undefined) continue
    if (Array.isArray(value) && value.length === 0) continue

    clauses.push({
      [declared.field]: declared.op === '$eq' ? value : { [declared.op]: value },
    })
  }

  if (clauses.length === 0) return undefined
  if (clauses.length === 1) return clauses[0] as Filter<Document>
  // `$and` rather than a merged object, because two parameters may legitimately
  // constrain the same field — `minPrice` and `maxPrice` both target `price`.
  return { $and: clauses } as Filter<Document>
}
