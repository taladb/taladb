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
 * Reads, writes, queue observability, retry policy, and hydration are exported
 * from this subpath; see the local-first query guide for the backend contract.
 */

export { QueryProvider, useQueryContext } from './context'
export type { QueryProviderProps, QueryContextValue } from './context'

export {
  useQuery,
  keepPreviousData,
  resetForgetTimers,
  DEFAULT_STALE_TIME,
} from './useQuery'
export { replaceEqualDeep } from './structural'
export { runShared, cancelShared, isShared, inflightKey, resetInflight } from './inflight'
export { shouldRetry, retryDelayMs, isOffline, DEFAULT_RETRY } from './retry'
export { resolveCollectionName, resetCollectionWarnings } from './collection'
export type { ResolveCollectionInput } from './collection'
export { extractDocuments } from './shape'
export {
  defineParams,
  normalizeParams,
  isBoundParams,
  eq,
  ne,
  gt,
  gte,
  lt,
  lte,
  contains,
  oneOf,
  shape,
} from './params'
export type {
  ParamSpec,
  ParamOp,
  ParamValue,
  ParamPrimitive,
  BoundParams,
  ValuesOf,
} from './params'
export { warnOnce, noteIgnoredOptions, resetWarnings, isDev } from './dev'
export type { ExtractInput, Extracted } from './shape'
export { useMutation, resetMutationScopes } from './useMutation'
export { useSyncStatus } from './useSyncStatus'

export { writeLocal, writeConfirmed } from './mutate'
export type { LocalWriteResult, Route } from './mutate'

export { drainOnce, backoffMs } from './drain'
export type { DrainStats, DrainDeps } from './drain'

export { sendWrite, toPendingWrite } from './send'
export type { SendResult } from './send'

export { hydrate } from './hydrate'
export type { HydrateResult } from './hydrate'

export {
  checkResponseId,
  checkResponseBody,
  checkClassification,
  checkMutationFilter,
} from './validate'

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

export { resolveUrl, defaultClassify, DEFAULT_METHODS } from './endpoint'

export {
  ensureEnvelopeIndexes,
  openQueryCollection,
  readQueryRecord,
  writeQueryRecord,
  deleteQueryRecord,
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
  Doc,
  RefetchTrigger,
  RetryOption,
  RetryDelayOption,
  NetworkMode,
  QueryDefaults,
  QueryFunctionContext,
  BaseQueryOptions,
  UseQueryOptions,
  UseDocumentQueryOptions,
  UseEnvelopeQueryOptions,
  AnyQueryOptions,
  ResultShape,
  QueryResult,
  QueryResultCommon,
  MutationMode,
  MutationOperation,
  InsertVariables,
  UpdateVariables,
  DeleteVariables,
  MutationContext,
  MutationCallbacks,
  UseMutationOptions,
  MutationResultCommon,
  MutationResult,
  MutationOp,
  FailedWrite,
  SyncStatus,
} from './types'
