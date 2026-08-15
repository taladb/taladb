/**
 * Query-key hashing.
 *
 * The property under test is the one the whole read path rests on: equal keys
 * must produce one record, unequal keys must never collide onto one. Both
 * failures are silent — a collision serves one query's documents to another,
 * and a false split refetches forever and never reads its own cache.
 */
import { describe, it, expect } from 'vitest'
import { hashQueryKey, queryRecordId, QUERY_COLLECTION } from '../../../src/query/keys'

describe('hashQueryKey', () => {
  it('is stable across property order', () => {
    // A component re-rendering from the same props can build either.
    expect(hashQueryKey(['todos', { a: 1, b: 2 }])).toBe(hashQueryKey(['todos', { b: 2, a: 1 }]))
  })

  it('sorts nested objects too', () => {
    expect(hashQueryKey([{ outer: { x: 1, y: 2 } }])).toBe(hashQueryKey([{ outer: { y: 2, x: 1 } }]))
  })

  it('keeps array order significant', () => {
    expect(hashQueryKey([['a', 'b']])).not.toBe(hashQueryKey([['b', 'a']]))
  })

  it('distinguishes an explicitly-unset filter from an absent one', () => {
    // JSON.stringify drops undefined properties, which would collapse these.
    expect(hashQueryKey([{ status: undefined }])).not.toBe(hashQueryKey([{}]))
  })

  it('distinguishes types that share a string form', () => {
    expect(hashQueryKey([1])).not.toBe(hashQueryKey(['1']))
    expect(hashQueryKey([null])).not.toBe(hashQueryKey(['null']))
    expect(hashQueryKey([true])).not.toBe(hashQueryKey(['true']))
  })

  it('distinguishes keys of different length', () => {
    expect(hashQueryKey(['todos'])).not.toBe(hashQueryKey(['todos', undefined]))
  })

  it('refuses a key it cannot hash faithfully', () => {
    expect(() => hashQueryKey([() => 1])).toThrow(TypeError)
    expect(() => hashQueryKey([Symbol('s')])).toThrow(TypeError)
  })
})

describe('queryRecordId', () => {
  it('is deterministic, so a refetch overwrites its own record', () => {
    expect(queryRecordId('todos', ['todos', { done: false }])).toBe(
      queryRecordId('todos', ['todos', { done: false }]),
    )
  })

  it('separates the same key used against different collections', () => {
    expect(queryRecordId('todos', ['all'])).not.toBe(queryRecordId('notes', ['all']))
  })

  it('separates different keys against the same collection', () => {
    expect(queryRecordId('todos', [{ done: true }])).not.toBe(
      queryRecordId('todos', [{ done: false }]),
    )
  })

  it('cannot be confused by a key containing the separator', () => {
    // A naive `collection + key` concatenation would collide these.
    expect(queryRecordId('a', ['b c'])).not.toBe(queryRecordId('a b', ['c']))
  })

  it('produces a 26-character Crockford ULID the engine will accept', () => {
    expect(queryRecordId('todos', ['x'])).toMatch(/^[0-9ABCDEFGHJKMNPQRSTVWXYZ]{26}$/)
  })
})

describe('QUERY_COLLECTION', () => {
  it('does not start with an underscore', () => {
    // The engine rejects such names with InvalidName — they are reserved.
    expect(QUERY_COLLECTION.startsWith('_')).toBe(false)
  })
})
