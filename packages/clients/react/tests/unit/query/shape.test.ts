/**
 * Response-shape detection.
 *
 * `data` here is rebuilt from the local collection rather than handed back from
 * the response, so every response has to reduce to documents. The interesting
 * cases are the boundaries: an envelope must not be mistaken for a document,
 * and the response that reduces to nothing must fail with an error that
 * explains the one thing a codemod cannot fix.
 */
import { describe, it, expect } from 'vitest'
import { extractDocuments } from '../../../src/query/shape'

const base = { collection: 'todos', queryKey: ['todos'] as const }

describe('extractDocuments', () => {
  it('recognises a list of documents', () => {
    const raw = [{ _id: 'a' }, { _id: 'b' }]
    expect(extractDocuments({ ...base, raw })).toEqual({ documents: raw, shape: 'array' })
  })

  it('recognises an empty list', () => {
    // Not a failure: "the server has nothing for this key" is an answer.
    expect(extractDocuments({ ...base, raw: [] })).toEqual({ documents: [], shape: 'array' })
  })

  it('recognises a single document', () => {
    const raw = { _id: 'a', title: 'one' }
    expect(extractDocuments({ ...base, raw })).toEqual({ documents: [raw], shape: 'document' })
  })

  it('does not mistake an envelope for a document', () => {
    // The reason detection keys on a string `_id` rather than "is an object":
    // this would otherwise be stored as a document in its own right.
    expect(() => extractDocuments({ ...base, raw: { items: [], total: 0 } })).toThrow(
      /neither a document nor a list/,
    )
  })

  it('does not treat a numeric _id as a document', () => {
    // Ids are ULIDs on disk. A numeric id needs mapping in `queryFn`, and
    // saying so beats storing a document the engine will later reject.
    expect(() => extractDocuments({ ...base, raw: { _id: 7, title: 'one' } })).toThrow(
      /neither a document nor a list/,
    )
  })

  it('explains the envelope escape hatch by name', () => {
    let message = ''
    try {
      extractDocuments({ ...base, raw: { items: [{ _id: 'a' }], total: 1 } })
    } catch (error) {
      message = (error as Error).message
    }

    // This is the case no rename can absorb, so the runtime carries the guide.
    expect(message).toMatch(/documents:/)
    expect(message).toMatch(/assemble:/)
    expect(message).toMatch(/todos/)
    // Naming the keys it actually saw is what turns the message into a diff.
    expect(message).toMatch(/items, total/)
  })

  it('uses `documents` when supplied, whatever the response shape', () => {
    const raw = { items: [{ _id: 'a' }], total: 1 }
    expect(
      extractDocuments({ ...base, raw, documents: (r) => (r as typeof raw).items }),
    ).toEqual({ documents: [{ _id: 'a' }], shape: 'envelope' })
  })

  it('rejects a `documents` that does not return an array', () => {
    expect(() =>
      extractDocuments({ ...base, raw: { item: { _id: 'a' } }, documents: () => ({}) as never }),
    ).toThrow(/must return an array/)
  })

  it('names the query in every error', () => {
    expect(() =>
      extractDocuments({ collection: 'books', queryKey: ['books', 3], raw: 'nope' }),
    ).toThrow(/useQuery\(\["books",3\]\)/)
  })

  it('describes a primitive response plainly', () => {
    expect(() => extractDocuments({ ...base, raw: 42 })).toThrow(/returned a number/)
    expect(() => extractDocuments({ ...base, raw: null })).toThrow(/returned null/)
  })
})
