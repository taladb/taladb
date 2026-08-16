/**
 * Deterministic document ids for rows that already have an identity elsewhere.
 *
 * `insert` accepts an `_id`, but it must be a ULID — ids are 16 raw bytes on
 * disk and every index key ends with one, so `'sku-1'` cannot be stored as an
 * id and is rejected. Hashing the origin's primary key into a ULID bridges the
 * two: the same `(collection, key)` always maps to the same document.
 *
 * That is what makes hydrating from a server **idempotent** — fetch a page,
 * `insertMany` it, and re-running the same fetch cannot produce a second copy of
 * a row, because the second insert is refused as a duplicate id rather than
 * minting a new document. It is also **resumable** (a bootstrap walk can restart
 * mid-way) and **safe to run concurrently** (an on-demand fetch and a background
 * walk touching the same row converge on one document rather than two).
 *
 * @example
 * const rows = await fetch('/api/products').then((r) => r.json())
 * await products.insertMany(
 *   rows.map((row) => ({ ...row, _id: deriveDocId('products', row.sku) })),
 * )
 *
 * ## This must stay byte-identical to the Rust `derive_doc_id`
 *
 * The same rows are addressed from both sides. If the two implementations ever
 * disagree, two clients assign different `_id`s to the same remote row and the
 * replica silently forks into duplicates — with no error anywhere. The shared
 * test vectors in `derive-id.test.ts` and `packages/core/src/document.rs` exist to
 * make that impossible to do by accident; keep them in lockstep.
 *
 * FNV-1a is used over a stronger hash precisely *because* it is short enough to
 * port between the two languages without ambiguity. It is non-cryptographic, which
 * is fine here: the input is a primary key from an origin the client already
 * trusts, not adversarial input.
 */

/** FNV-1a (128-bit) parameters, per the reference specification. */
const FNV1A128_OFFSET_BASIS = 0x6c62272e07bb014262b821756295c58dn
const FNV1A128_PRIME = 0x0000000001000000000000000000013bn
const MASK_128 = (1n << 128n) - 1n

/** Crockford base32 — the ULID alphabet. */
const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'

/** Shared across calls; a bootstrap walk derives one id per row over 100k rows. */
const UTF8 = new TextEncoder()

/**
 * Encode a 128-bit value as a 26-character Crockford-base32 ULID string.
 *
 * 26 × 5 = 130 bits, so the leading character carries only the top 3 bits (and is
 * therefore always `0`–`7`); the remaining 25 characters carry 5 bits each.
 */
function encodeUlid(value: bigint): string {
  let out = ''
  for (let i = 25; i >= 0; i--) {
    out += CROCKFORD[Number((value >> BigInt(i * 5)) & 31n)]
  }
  return out
}

/**
 * Derive a stable `_id` for a row replicated from a remote origin.
 *
 * `collection` is part of the preimage, so the same remote id in two different
 * collections cannot collide.
 *
 * @example
 * deriveDocId('products', 'sku-123')  // → '56GC678DQYWW1Z98HPYJ90WVKH', always
 *
 * ## Ordering caveat
 *
 * The result is a hash, so its ULID timestamp prefix is **not** chronological.
 * Documents written with a derived id do not come back in insertion order from an
 * unsorted `find()`; reads over replicated collections must carry an explicit
 * sort. Documents written via `insert`/`insertMany` are unaffected — they still
 * get monotonic ULIDs.
 */
/**
 * Crockford base32, the ULID alphabet — `I`, `L`, `O` and `U` are excluded.
 *
 * Deliberately case-insensitive and deliberately *not* range-checked on the
 * leading character, because that is exactly what the Rust decoder does: it
 * lower-cases through a lookup table and shifts 26 × 5 = 130 bits into a `u128`,
 * letting the top two bits fall off rather than reporting an overflow. A
 * stricter check here would reject ids the engine stores happily, which is a
 * worse failure than the one this is here to prevent.
 */
const DOC_ID_PATTERN = /^[0-9A-HJKMNP-TV-Za-hjkmnp-tv-z]{26}$/

/**
 * Whether `value` is usable as a document `_id`.
 *
 * Ids are 16 raw bytes on disk, so only a ULID qualifies — a natural key like
 * `'sku-1'`, `'265'` or a UUID is rejected on insert. Use this to check a value
 * before writing it, and {@link deriveDocId} to turn a natural key into one.
 *
 * @example
 * isDocId(deriveDocId('products', 'sku-123'))  // true
 * isDocId('sku-123')                           // false
 * isDocId(crypto.randomUUID())                 // false — a UUID is not a ULID
 */
export function isDocId(value: unknown): value is string {
  return typeof value === 'string' && DOC_ID_PATTERN.test(value)
}

export function deriveDocId(collection: string, key: string): string {
  // A 0x00 separator keeps the preimage unambiguous: without it, ('ab', 'c') and
  // ('a', 'bc') would hash identically.
  const bytes = [...UTF8.encode(collection), 0, ...UTF8.encode(key)]
  let hash = FNV1A128_OFFSET_BASIS
  for (const byte of bytes) {
    hash ^= BigInt(byte)
    hash = (hash * FNV1A128_PRIME) & MASK_128
  }
  return encodeUlid(hash)
}
