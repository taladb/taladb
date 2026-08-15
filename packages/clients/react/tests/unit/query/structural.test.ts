/**
 * Structural sharing.
 *
 * The point is referential, not semantic: `toEqual` would pass on every one of
 * these whether or not the function did anything. What matters is `toBe` — that
 * an unchanged value keeps its previous reference, so a migrated
 * `useEffect(…, [data])` fires on real changes rather than on every snapshot.
 */
import { describe, it, expect } from 'vitest'
import { replaceEqualDeep } from '../../../src/query/structural'

describe('replaceEqualDeep', () => {
  it('keeps the previous reference for an equal array', () => {
    const previous = [{ _id: 'a', title: 'one' }]
    const next = [{ _id: 'a', title: 'one' }]

    expect(replaceEqualDeep(previous, next)).toBe(previous)
  })

  it('returns the new value when anything changed', () => {
    const previous = [{ _id: 'a', title: 'one' }]
    const next = [{ _id: 'a', title: 'two' }]

    const result = replaceEqualDeep(previous, next)
    expect(result).not.toBe(previous)
    expect(result).toEqual(next)
  })

  it('reuses the unchanged members of a changed array', () => {
    const first = { _id: 'a', title: 'one' }
    const previous = [first, { _id: 'b', title: 'two' }]
    const next = [{ _id: 'a', title: 'one' }, { _id: 'b', title: 'CHANGED' }]

    const result = replaceEqualDeep(previous, next)
    // The array is new, but the document that did not change is not — which is
    // what keeps a memoised list row from re-rendering.
    expect(result).not.toBe(previous)
    expect(result[0]).toBe(first)
    expect(result[1]).toEqual({ _id: 'b', title: 'CHANGED' })
  })

  it('notices a removed key', () => {
    const previous = { a: 1, b: 2 }
    const next = { a: 1 }
    expect(replaceEqualDeep(previous, next)).not.toBe(previous)
  })

  it('notices an added key', () => {
    const previous = { a: 1 }
    const next = { a: 1, b: 2 }
    expect(replaceEqualDeep(previous, next)).toEqual({ a: 1, b: 2 })
    expect(replaceEqualDeep(previous, next)).not.toBe(previous)
  })

  it('handles nesting', () => {
    const previous = { items: [{ _id: 'a' }], total: 1 }
    const next = { items: [{ _id: 'a' }], total: 1 }
    expect(replaceEqualDeep(previous, next)).toBe(previous)
  })

  it('does not rebuild a class instance as a bare object', () => {
    const previous = { at: new Date(0) }
    const next = { at: new Date(0) }

    // Walking a Date's keys would produce `{}` and destroy its prototype — a
    // far worse outcome than missing the reuse.
    const result = replaceEqualDeep(previous, next)
    expect(result.at).toBeInstanceOf(Date)
    expect(result.at.getTime()).toBe(0)
  })

  it('passes through mismatched types', () => {
    expect(replaceEqualDeep([1], { a: 1 })).toEqual({ a: 1 })
    expect(replaceEqualDeep({ a: 1 }, null)).toBeNull()
    expect(replaceEqualDeep(undefined, [1])).toEqual([1])
  })

  it('treats an empty array as equal to an empty array', () => {
    const previous: never[] = []
    expect(replaceEqualDeep(previous, [])).toBe(previous)
  })
})
