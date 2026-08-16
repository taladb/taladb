import { deriveDocId } from 'taladb'

/**
 * A storable `_id` for a short, readable test label.
 *
 * Fixtures want to say `'a'`, `'b'`, `'c'`; the engine only accepts ULIDs. These
 * suites run against mock collections that enforce neither, and that gap is how
 * the natural-key bug reached a real app: every unit test passed with ids the
 * database would have refused on contact.
 *
 * Routing labels through `deriveDocId` keeps fixtures readable *and* honest —
 * the mapping is deterministic, so `id('a')` is the same id in every assertion.
 */
export const id = (label: string): string => deriveDocId('test', label)
