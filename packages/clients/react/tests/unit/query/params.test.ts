/**
 * Request parameters, declared once.
 *
 * The rule that matters most is what happens to a parameter *nobody declared*:
 * it reaches the request and the key, and contributes no local predicate. That
 * asymmetry is deliberate. A local filter that is broader than the server's
 * shows a document the server would have excluded — recoverable. One that is
 * narrower shows an empty screen over a full database — not.
 */
import { describe, it, expect } from 'vitest'
import {
  defineParams,
  normalizeParams,
  isBoundParams,
  contains,
  eq,
  gte,
  lte,
  oneOf,
  shape,
} from '../../../src/query/params'

const books = defineParams({
  category: eq('category'),
  minPrice: gte('price'),
  maxPrice: lte('price'),
  search: contains('title'),
  tags: oneOf('tags'),
  page: shape(),
  sort: shape(),
})

describe('toSearchParams', () => {
  it('serialises what the request needs', () => {
    const bound = books({ category: 'book', minPrice: 10, page: 2 })
    expect(bound.toSearchParams().toString()).toBe('category=book&minPrice=10&page=2')
  })

  it('drops undefined and null rather than sending empty values', () => {
    const bound = books({ category: 'book', search: undefined })
    expect(bound.toSearchParams().toString()).toBe('category=book')
  })

  it('comma-joins arrays and drops empty ones', () => {
    expect(books({ tags: ['a', 'b'] }).toSearchParams().toString()).toBe('tags=a%2Cb')
    // An empty array means "no constraint", not "match nothing".
    expect(books({ tags: [] }).toSearchParams().toString()).toBe('')
  })

  it('works on a plain object too', () => {
    const bound = normalizeParams({ category: 'book', page: 1 })!
    // The request half comes free without declaring anything — which is what
    // makes upgrading to `defineParams` additive rather than a rewrite.
    expect(bound.toSearchParams().toString()).toBe('category=book&page=1')
  })
})

describe('toFilter', () => {
  it('maps a declared equality parameter onto its field', () => {
    expect(books({ category: 'book' }).toFilter()).toEqual({ category: 'book' })
  })

  it('maps operators onto the field they target', () => {
    expect(books({ minPrice: 10 }).toFilter()).toEqual({ price: { $gte: 10 } })
    expect(books({ search: 'dune' }).toFilter()).toEqual({ title: { $contains: 'dune' } })
    expect(books({ tags: ['a'] }).toFilter()).toEqual({ tags: { $in: ['a'] } })
  })

  it('combines with $and, so two parameters may target one field', () => {
    // A merged object would lose one of these: both are `price`.
    expect(books({ minPrice: 10, maxPrice: 99 }).toFilter()).toEqual({
      $and: [{ price: { $gte: 10 } }, { price: { $lte: 99 } }],
    })
  })

  it('ignores parameters declared as shaping the response', () => {
    // `page: 1` as a predicate would be `{ page: 1 }` — matching nothing, and
    // rendering an empty screen with `status: 'success'`.
    expect(books({ page: 2, sort: '-createdAt' }).toFilter()).toBeUndefined()
  })

  it('ignores parameters nobody declared', () => {
    const bound = normalizeParams({ category: 'book', page: 1 })!
    // Broader than the server's filter, never narrower. The stored id list
    // still bounds the result, so being broader costs nothing.
    expect(bound.toFilter()).toBeUndefined()
  })

  it('returns undefined when nothing is set', () => {
    expect(books({}).toFilter()).toBeUndefined()
    expect(books({ category: undefined }).toFilter()).toBeUndefined()
  })

  it('skips an empty array rather than matching nothing', () => {
    expect(books({ tags: [] }).toFilter()).toBeUndefined()
  })
})

describe('shapeParams', () => {
  it('lists the response-shaping parameters that are set', () => {
    // Used to warn when an offline fallback cannot honour them — there is no
    // page 2 of a local query.
    expect(books({ category: 'book', page: 2 }).shapeParams()).toEqual(['page'])
    expect(books({ category: 'book' }).shapeParams()).toEqual([])
  })
})

describe('normalizeParams', () => {
  it('passes bound parameters through untouched', () => {
    const bound = books({ category: 'book' })
    expect(normalizeParams(bound)).toBe(bound)
  })

  it('recognises bound parameters', () => {
    expect(isBoundParams(books({}))).toBe(true)
    expect(isBoundParams({ category: 'book' })).toBe(false)
    expect(isBoundParams(null)).toBe(false)
  })

  it('is undefined for undefined', () => {
    expect(normalizeParams(undefined)).toBeUndefined()
  })
})

describe('values', () => {
  it('keeps the values as given, for the query key', () => {
    const values = { category: 'book', page: 2 }
    expect(books(values).values).toEqual(values)
  })
})
