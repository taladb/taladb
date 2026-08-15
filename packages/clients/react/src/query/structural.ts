/**
 * Structural sharing: keep the previous reference when the new value is equal.
 *
 * Without this, `data` is a fresh array on every database snapshot — the live
 * query re-runs, rebuilds its documents, and hands back objects that are deeply
 * equal but referentially new. Migrated code is full of `useEffect(…, [data])`
 * and `useMemo(…, [data])`, and every one of them would fire on each snapshot,
 * including snapshots that changed nothing relevant to this query. TanStack does
 * the same thing for the same reason.
 *
 * The result is that a component re-renders only when something it can actually
 * observe has changed, and that `data === data` across renders holds as long as
 * the contents hold.
 */
export function replaceEqualDeep<T>(previous: unknown, next: T): T {
  if (previous === next) return next

  const bothArrays = Array.isArray(previous) && Array.isArray(next)
  if (bothArrays || (isPlainObject(previous) && isPlainObject(next))) {
    const previousItems = previous as Record<string, unknown>
    const nextItems = next as unknown as Record<string, unknown>
    const previousKeys = Object.keys(previousItems)
    const nextKeys = Object.keys(nextItems)

    let equal = previousKeys.length === nextKeys.length
    const merged = (bothArrays ? [] : {}) as Record<string, unknown>

    for (const key of nextKeys) {
      const child = replaceEqualDeep(previousItems[key], nextItems[key])
      // Reference equality after the recursive call *is* the equality test:
      // the child either reused the previous reference or produced a new one.
      if (equal && child !== previousItems[key]) equal = false
      merged[key] = child
    }

    return (equal ? previous : merged) as T
  }

  return next
}

/**
 * Plain objects only.
 *
 * A `Date`, `Map`, or class instance is compared by reference: walking its keys
 * would rebuild it as a bare object and quietly destroy its prototype, which is
 * a far worse failure than a missed reuse.
 */
function isPlainObject(value: unknown): value is Record<string, unknown> {
  if (typeof value !== 'object' || value === null) return false
  const proto = Object.getPrototypeOf(value)
  return proto === Object.prototype || proto === null
}
