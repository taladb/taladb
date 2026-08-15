/**
 * Collection resolution.
 *
 * `collection` is not a cache label — it is the physical destination for every
 * fetched document. So inference from `queryKey[0]` is allowed, but it is
 * checked against the registry rather than trusted, and the interesting cases
 * are all about *refusing*.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { resolveCollectionName, resetCollectionWarnings } from '../../../src/query/collection'

beforeEach(() => {
  resetCollectionWarnings()
})

describe('resolveCollectionName', () => {
  it('prefers an explicit collection over everything else', () => {
    const name = resolveCollectionName({
      collection: 'archive',
      queryKey: ['todos'],
      resolve: () => 'resolved',
      registered: ['todos'],
    })
    // Explicit is unchecked: the caller said it outright, and an app may
    // deliberately store a response somewhere the registry does not describe.
    expect(name).toBe('archive')
  })

  it('infers from the first key segment', () => {
    expect(
      resolveCollectionName({ queryKey: ['todos', { done: true }], registered: ['todos'] }),
    ).toBe('todos')
  })

  it('prefers the provider resolver over the first segment', () => {
    const name = resolveCollectionName({
      queryKey: ['api', 'v2', 'todos'],
      resolve: (key) => key[2] as string,
      registered: ['todos'],
    })
    expect(name).toBe('todos')
  })

  it('falls back to inference when the resolver returns undefined', () => {
    const name = resolveCollectionName({
      queryKey: ['todos'],
      resolve: () => undefined,
      registered: ['todos'],
    })
    expect(name).toBe('todos')
  })

  it('throws when the inferred name is not registered, listing what is', () => {
    expect(() =>
      resolveCollectionName({ queryKey: ['api', 'v2', 'todos'], registered: ['todos', 'lists'] }),
    ).toThrow(/todos, lists/)
  })

  it('throws when the first segment is not a string', () => {
    expect(() =>
      resolveCollectionName({ queryKey: [{ scope: 'todos' }], registered: ['todos'] }),
    ).toThrow(/collection/i)
  })

  it('throws on an empty key', () => {
    expect(() => resolveCollectionName({ queryKey: [], registered: ['todos'] })).toThrow(
      /collection/i,
    )
  })

  it('names the offending query in the error', () => {
    expect(() => resolveCollectionName({ queryKey: ['books', 7], registered: ['todos'] })).toThrow(
      /\["books",7\]/,
    )
  })

  it('warns rather than throwing when nothing is registered', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})

    // An empty registry is legal — `collections` is optional — so there is
    // nothing to check against. Refusing here would punish the smallest apps.
    expect(resolveCollectionName({ queryKey: ['todos'], registered: [] })).toBe('todos')
    expect(warn).toHaveBeenCalledTimes(1)
    expect(warn.mock.calls[0][0]).toMatch(/todos/)

    warn.mockRestore()
  })

  it('warns once per collection, not once per render', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {})

    for (let i = 0; i < 5; i++) resolveCollectionName({ queryKey: ['todos'], registered: [] })
    resolveCollectionName({ queryKey: ['lists'], registered: [] })

    // A warning on every render is noise that gets filtered out, which is the
    // same as not warning at all.
    expect(warn).toHaveBeenCalledTimes(2)
    warn.mockRestore()
  })
})
