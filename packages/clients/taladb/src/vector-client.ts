/** Shared vector API for Node, browser workers, and the React Native job executor. */
export type VectorQuantization = 'none' | 'scalar' | 'binary';
export interface VectorGraphOptions {
  m?: number;
  efConstruction?: number;
  quantization?: VectorQuantization;
}
export interface VectorQueryOptions {
  mode?: 'auto' | 'exact' | 'ann';
  efSearch?: number;
  scoreThreshold?: number;
  offset?: number;
  groupBy?: string;
  groupSize?: number;
  oversampling?: number;
}
export interface VectorBuildProgress {
  id: string;
  state: 'building' | 'ready' | 'cancelled' | 'failed';
  processed: number;
  total: number;
  revision: number;
  error: string | null;
}
export interface VectorRebuildOptions extends VectorGraphOptions {
  /** Insertions per native/worker call (1–1024); defaults to 32 for mobile responsiveness. */
  batchSize?: number;
  signal?: AbortSignal;
  onProgress?: (progress: VectorBuildProgress) => void;
}
export interface VectorIndexStatus {
  field: string;
  state: 'flat' | 'ready' | 'stale' | 'rebuildRequired';
  persistent: boolean;
  indexedVectors: number;
  totalVectors: number;
  deletedNodes: number;
  revision: number;
  indexRevision: number | null;
  options: Required<VectorGraphOptions> | null;
  build: VectorBuildProgress | null;
}
export interface VectorQueryResult<T> {
  hits: { document: T; score: number }[];
  execution: {
    path: 'exact' | 'hnsw';
    reason: string;
    revision: number;
    efSearch: number | null;
    distanceComputations: number;
  };
  /** Offset pagination uses live snapshots. Writes between pages can change ordering. */
  nextOffset: number | null;
}
export interface VectorRecall {
  recallAtK: number;
  queries: number;
  topK: number;
  exactMs: number;
  annMs: number;
}
export interface VectorClient<T> {
  searchVectors(field: string, vector: ArrayLike<number>, topK: number, filter?: Record<string, unknown>, options?: VectorQueryOptions): Promise<VectorQueryResult<T>>;
  /** Exact range search. Returns every matching document above the score threshold. */
  findWithin(field: string, vector: ArrayLike<number>, scoreThreshold: number, filter?: Record<string, unknown>): Promise<VectorQueryResult<T>>;
  vectorIndexStatus(field: string): Promise<VectorIndexStatus>;
  rebuildVectorIndex(field: string, options?: VectorRebuildOptions): Promise<VectorBuildProgress>;
  beginVectorBuild(field: string, options?: VectorGraphOptions): Promise<VectorBuildProgress>;
  stepVectorBuild(field: string, id: string, batchSize?: number): Promise<VectorBuildProgress>;
  cancelVectorBuild(field: string, id: string): Promise<VectorBuildProgress>;
  measureVectorRecall(field: string, queries: ArrayLike<number>[], topK: number, filter?: Record<string, unknown>, options?: VectorQueryOptions): Promise<VectorRecall>;
}

function integer(value: number, name: string, min = 0, max = 0xffffffff): void {
  if (!Number.isSafeInteger(value) || value < min || value > max) throw new Error(`${name} must be an integer between ${min} and ${max}`);
}
function vectorValues(vector: ArrayLike<number>): number[] {
  const values = Array.from(vector);
  if (values.length === 0 || values.some(v => !Number.isFinite(v))) throw new Error('vector must contain finite numbers');
  return values;
}
function queryOptions(options: VectorQueryOptions): VectorQueryOptions {
  if (options.efSearch !== undefined) integer(options.efSearch, 'efSearch', 1);
  if (options.offset !== undefined) integer(options.offset, 'offset');
  if (options.groupSize !== undefined) integer(options.groupSize, 'groupSize', 1);
  if (options.oversampling !== undefined) integer(options.oversampling, 'oversampling', 1, 100);
  if (options.scoreThreshold !== undefined && !Number.isFinite(options.scoreThreshold)) throw new Error('scoreThreshold must be finite');
  return options;
}
function abortError(): Error {
  const error = new Error('Vector index rebuild cancelled');
  error.name = 'AbortError';
  return error;
}

/** Binding adapter used by TalaDB's Node, browser, and React Native packages. */
export function createVectorClient<T>(send: (request: Record<string, unknown>) => Promise<any>): VectorClient<T> {
  const client: VectorClient<T> = {
    searchVectors: async (field, vector, topK, filter, options = {}) => {
      integer(topK, 'topK');
      return send({ op: 'search', field, query: vectorValues(vector), topK, filter, options: queryOptions(options) });
    },
    findWithin: (field, vector, scoreThreshold, filter) => client.searchVectors(field, vector, 0xffffffff, filter, { mode: 'exact', scoreThreshold }),
    vectorIndexStatus: field => send({ op: 'status', field }),
    beginVectorBuild: (field, options) => send({ op: 'beginBuild', field, options }),
    stepVectorBuild: async (field, id, batchSize = 32) => {
      integer(batchSize, 'batchSize', 1, 1024);
      return send({ op: 'stepBuild', field, id, batchSize });
    },
    cancelVectorBuild: (field, id) => send({ op: 'cancelBuild', field, id }),
    rebuildVectorIndex: async (field, options = {}) => {
      const { signal, onProgress, batchSize = 32, ...graphOptions } = options;
      integer(batchSize, 'batchSize', 1, 1024);
      if (signal?.aborted) throw abortError();
      let progress = await client.beginVectorBuild(field, Object.keys(graphOptions).length ? graphOptions : undefined);
      try {
        onProgress?.(progress);
        while (progress.state === 'building') {
          // Yield on every platform so UI callbacks and AbortSignal delivery run
          // between bounded calls. Native/Node execute each call off the JS thread.
          await new Promise<void>(resolve => setTimeout(resolve, 0));
          if (signal?.aborted) throw abortError();
          progress = await client.stepVectorBuild(field, progress.id, batchSize);
          onProgress?.(progress);
        }
        if (progress.state === 'failed') throw new Error(progress.error ?? 'Vector rebuild failed');
        if (progress.state === 'cancelled') throw abortError();
        return progress;
      } catch (error) {
        if (progress.state === 'building') await client.cancelVectorBuild(field, progress.id);
        throw error;
      }
    },
    measureVectorRecall: async (field, queries, topK, filter, options = {}) => {
      integer(topK, 'topK', 1);
      return send({ op: 'recall', field, queries: queries.map(vectorValues), topK, filter, options: queryOptions(options) });
    },
  };
  return client;
}

export function vectorIndexRequest(field: string, options: {
  dimensions: number; metric?: string; indexType?: string;
  hnswM?: number; hnswEfConstruction?: number; quantization?: VectorQuantization;
}): Record<string, unknown> {
  integer(options.dimensions, 'dimensions', 1);
  if (options.metric && !['cosine', 'dot', 'euclidean'].includes(options.metric)) throw new Error('invalid vector metric');
  if (options.indexType && !['flat', 'hnsw'].includes(options.indexType)) throw new Error('invalid vector indexType');
  if (options.quantization && options.quantization !== 'none' && options.indexType !== 'hnsw') throw new Error('quantization requires an HNSW index');
  if (options.indexType === 'hnsw') {
    const m = options.hnswM ?? 32;
    const efConstruction = options.hnswEfConstruction ?? 200;
    integer(m, 'hnswM', 2, 128);
    integer(efConstruction, 'hnswEfConstruction', m, 100_000);
    if (options.metric === 'dot') throw new Error('HNSW requires cosine or euclidean');
    if (options.quantization === 'binary' && options.metric && options.metric !== 'cosine') throw new Error('binary quantization requires cosine');
  }
  return { op: 'create', field, dimensions: options.dimensions, metric: options.metric,
    options: options.indexType === 'hnsw' ? { m: options.hnswM ?? 32, efConstruction: options.hnswEfConstruction ?? 200, quantization: options.quantization ?? 'none' } : null };
}
