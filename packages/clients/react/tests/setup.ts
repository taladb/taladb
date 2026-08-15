/**
 * Shared teardown for every test file.
 *
 * Two things here are easy to forget per-file and expensive to forget:
 *
 * **Unmounting.** Testing Library registers its own auto-cleanup only when the
 * runner exposes a global `afterEach`, and these configs do not enable vitest
 * globals. Without it, components from one test stay mounted into the next —
 * so `getByText` matches the previous test's DOM, and hooks keep holding
 * resources against a database that no longer exists.
 *
 * **Module state.** Request sharing, the forget timers, and the warn-once
 * registry are all module-level by design: they have to outlive the components
 * that created them. That makes them leak across tests unless reset. The worst
 * case is silent — a test whose `queryFn` never resolves leaves an in-flight
 * entry that every later test with the same key joins, and they hang rather
 * than fail with anything readable.
 *
 * Wired through `setupFiles` in both vitest configs rather than repeated in
 * each file, because the failure mode of forgetting it is a mystery rather than
 * an error.
 */
import { afterEach, vi } from 'vitest'
import { cleanup } from '@testing-library/react'
import { resetInflight } from '../src/query/inflight'
import { resetForgetTimers } from '../src/query/useQuery'
import { resetMutationScopes } from '../src/query/useMutation'
import { resetWarnings } from '../src/query/dev'

afterEach(() => {
  cleanup()
  // A leaked `vi.stubGlobal('navigator', …)` does not merely affect the next
  // assertion — React DOM reads `navigator`, so every later render in the file
  // returns null and the failures point nowhere near the cause.
  vi.unstubAllGlobals()
  resetInflight()
  resetForgetTimers()
  resetMutationScopes()
  resetWarnings()
})
