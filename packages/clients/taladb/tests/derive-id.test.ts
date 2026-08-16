import { describe, expect, it } from 'vitest'
import { deriveDocId, isDocId } from '../src/derive-id'

describe('deriveDocId', () => {
  it('is deterministic', () => {
    expect(deriveDocId('products', 'sku-123')).toBe(deriveDocId('products', 'sku-123'))
  })

  it('separates collections so the same remote key cannot collide', () => {
    expect(deriveDocId('products', '1')).not.toBe(deriveDocId('orders', '1'))
  })

  it('always produces something isDocId accepts', () => {
    for (const key of ['sku-123', '265', '', 'a'.repeat(200), '💥']) {
      expect(isDocId(deriveDocId('products', key))).toBe(true)
    }
  })

  it('separates preimage boundaries', () => {
    // Without the 0x00 separator these two would hash identically.
    expect(deriveDocId('ab', 'c')).not.toBe(deriveDocId('a', 'bc'))
  })

  it('produces a well-formed 26-character ULID', () => {
    const id = deriveDocId('products', 'sku-123')
    expect(id).toHaveLength(26)
    expect(id).toMatch(/^[0-7][0-9A-HJKMNP-TV-Z]{25}$/)
  })

  /**
   * The cross-language contract. These vectors are duplicated verbatim in
   * `packages/core/src/document.rs::derive_doc_id_cross_language_vectors` and the
   * two suites **must** stay in lockstep.
   *
   * If Rust and TypeScript ever disagree here, two clients assign different `_id`s
   * to the same remote row, and the replica silently forks into duplicate
   * documents — with no error raised anywhere. This test is the only thing
   * standing between that bug and production.
   */
  it('matches the Rust derive_doc_id byte-for-byte', () => {
    const vectors: Array<[collection: string, key: string, expected: string]> = [
      ['products', 'sku-123', '56GC678DQYWW1Z98HPYJ90WVKH'],
      ['products', '1', '7Z6Y6H8NG96ZGN4PJVDP18CSY2'],
      ['orders', '1', '5VYD63GDV5KCDXV2A0SYRWSKVZ'],
      ['', '', '6J535PJ40THJQQH49BE174M53Z'],
      // Multi-byte UTF-8: the hash runs over UTF-8 bytes, not UTF-16 code units.
      // Getting this wrong would pass every ASCII test and fail only in production,
      // on exactly the catalogs most likely to contain non-ASCII keys.
      ['products', 'sku-ñ-💡', '5NZ0PGNM2CF0BTHN0AAX8CWPAA'],
    ]

    for (const [collection, key, expected] of vectors) {
      const label = `deriveDocId(${JSON.stringify(collection)}, ${JSON.stringify(key)})`
      expect(deriveDocId(collection, key), label).toBe(expected)
    }
  })
})

describe('isDocId', () => {
  it('accepts a canonical ULID', () => {
    expect(isDocId('56GC678DQYWW1Z98HPYJ90WVKH')).toBe(true)
  })

  /**
   * The natural keys people actually pass through from a server. Each of these
   * reaching the engine is the failure `deriveDocId` exists to prevent.
   */
  it('rejects natural keys', () => {
    for (const value of ['265', 'sku-123', 'the-book-of-dragons', '', '1']) {
      expect(isDocId(value), value).toBe(false)
    }
  })

  it('rejects a UUID, which is the same length family but not the alphabet', () => {
    expect(isDocId('eeaec0d9-bf6b-4631-947a-c87d8a61f0ff')).toBe(false)
  })

  it('rejects non-strings', () => {
    for (const value of [undefined, null, 265, {}, [], new Date()]) {
      expect(isDocId(value)).toBe(false)
    }
  })

  it('rejects the wrong length either side of 26', () => {
    expect(isDocId('56GC678DQYWW1Z98HPYJ90WVK')).toBe(false)
    expect(isDocId('56GC678DQYWW1Z98HPYJ90WVKHH')).toBe(false)
  })

  it('rejects the four characters Crockford excludes', () => {
    // I, L, O and U are omitted to avoid confusion with 1 and 0.
    for (const char of ['I', 'L', 'O', 'U']) {
      expect(isDocId(`${char}6GC678DQYWW1Z98HPYJ90WVKH`), char).toBe(false)
    }
  })

  /**
   * These two mirror the Rust decoder rather than the ULID spec, and that is
   * deliberate — see DOC_ID_PATTERN. Tightening either one would start
   * rejecting ids the engine stores without complaint.
   */
  it('accepts lowercase, because the Rust lookup table does', () => {
    expect(isDocId('56gc678dqyww1z98hpyj90wvkh')).toBe(true)
  })

  it('accepts a leading character above 7, because the decoder lets it overflow', () => {
    expect(isDocId('Z6GC678DQYWW1Z98HPYJ90WVKH')).toBe(true)
  })
})
