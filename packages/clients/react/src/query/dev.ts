/**
 * Development-time notices.
 *
 * Some TanStack options are accepted here and do nothing — a drop-in that
 * *throws* on an unknown option is not a drop-in. But silence is a trap, so
 * every such option says so once, loudly enough to find and quietly enough to
 * live with.
 */

const seen = new Set<string>()

export function isDev(): boolean {
  return typeof process !== 'undefined' && process.env?.NODE_ENV !== 'production'
}

/**
 * Warn once per key for the life of the session.
 *
 * A hook warning on every render is noise that gets filtered out, which is the
 * same as not warning at all.
 */
export function warnOnce(key: string, message: string): void {
  if (!isDev() || seen.has(key)) return
  seen.add(key)
  console.warn(`[@taladb/react/query] ${message}`)
}

/** Test seam — the warn-once set is module state that would leak between cases. */
export function resetWarnings(): void {
  seen.clear()
}

/**
 * TanStack options this layer accepts and does not act on.
 *
 * Each entry says what it would have done and why it does not, because "we
 * ignore this" without a reason is indistinguishable from a bug.
 */
const IGNORED: Record<string, string> = {
  notifyOnChangeProps:
    'is not implemented. It is a render optimisation with no effect on correctness; ' +
    'every result field is computed either way.',
  structuralSharing:
    'is always on and cannot be disabled. Turning it off would make `data` a new reference on ' +
    'every database snapshot, and a live query produces those far more often than a polling ' +
    'cache does.',
}

/** Announce any accept-and-ignore option present on this call. */
export function noteIgnoredOptions(options: Record<string, unknown>): void {
  if (!isDev()) return
  for (const [option, explanation] of Object.entries(IGNORED)) {
    if (options[option] === undefined) continue
    warnOnce(`ignored:${option}`, `\`${option}\` ${explanation}`)
  }
}
