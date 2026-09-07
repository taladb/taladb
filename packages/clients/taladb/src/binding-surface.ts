/**
 * The slice of this package that a native binding needs at type-check time:
 * the webhook runtime plus the shared type declarations, and nothing that
 * touches Node.
 *
 * `@taladb/react-native` maps the bare `taladb` specifier here through its
 * tsconfig `paths`, so its type-check never reaches `config.ts` — which reads
 * `process.env` and `node:fs`, neither of which exists on a device. Consumers
 * of the published package resolve the real entry point instead, and it
 * re-exports everything below.
 *
 * Keep this file free of runtime imports beyond `./webhook`: anything added
 * here lands in the React Native type-check.
 */
export * from './webhook';
export type {
  AggregatePipeline,
  AggregateStage,
  Document,
  Filter,
  HybridSearchOptions,
  HybridSearchResult,
  TextSearchOptions,
  TextSearchResult,
  Update,
  Value,
  VectorIndexOptions,
  VectorMetric,
  VectorSearchResult,
} from './types';

export { createVectorClient } from './vector-client';
export type { VectorClient, VectorQueryOptions, VectorRebuildOptions, VectorBuildProgress, VectorIndexStatus, VectorQueryResult, VectorRecall, VectorGraphOptions, VectorQuantization } from './vector-client';
