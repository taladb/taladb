/**
 * `@taladb/react/query` — a local-first data layer over TalaDB.
 *
 * Reads hydrate from the device and revalidate in the background; writes commit
 * locally and drain to the network without blocking the UI. The cache is a real
 * TalaDB collection, so it survives reload with no rehydration step and stays
 * queryable offline — `find`, `$sort`, `aggregate`, and full-text search all
 * work against it.
 *
 * Imported from a subpath so none of it reaches a bundle that only uses the
 * hooks in `@taladb/react`.
 *
 * **Status: in progress.** The type surface, `defineBackend`, and the envelope
 * and query-record layer are in place; the hooks are not implemented yet.
 * See PLAN-query.md.
 */

export { QueryProvider, useQueryContext } from './context'
export type { QueryProviderProps, QueryContextValue } from './context'

export { useQuery } from './useQuery'
export { useMutation } from './useMutation'

export { writeLocal, writeConfirmed } from './mutate'
export type { LocalWriteResult } from './mutate'

export { drainOnce, backoffMs } from './drain'
export type { DrainStats, DrainDeps } from './drain'

export { sendWrite, toPendingWrite } from './send'
export type { SendResult } from './send'

export { hydrate } from './hydrate'
export type { HydrateResult } from './hydrate'

export { defineBackend } from './defineBackend'

export {
  ENVELOPE_FIELDS,
  ENVELOPE_INDEXES,
  reservedFieldsIn,
  stampSynced,
  stampPending,
  coalesce,
} from './envelope'
export type { Enveloped } from './envelope'

export { QUERY_COLLECTION, hashQueryKey, queryRecordId, newDocId } from './keys'

export {
  ensureEnvelopeIndexes,
  openQueryCollection,
  readQueryRecord,
  writeQueryRecord,
  isStale,
} from './queries'

export type {
  // Envelope
  SyncState,
  SyncOp,
  SyncEnvelope,
  QueryRecord,
  // Backend contract
  WriteOutcome,
  PendingWrite,
  BackendDefinition,
  ResolvedBackend,
  // Drain
  DrainPolicy,
  DrainOptions,
  // Hooks
  QueryKey,
  UseQueryOptions,
  QueryResult,
  MutationMode,
  UseMutationOptions,
  MutationResult,
  MutationOp,
  FailedWrite,
  SyncStatus,
} from './types'
