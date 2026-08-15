/**
 * TalaDB React Native — TurboModule spec.
 *
 * This file is the Codegen source. It defines the native interface that
 * both the iOS JSI HostObject (TalaDB.mm) and the Android JNI bridge
 * (TalaDBModule.kt) must implement.
 */
import type { TurboModule } from 'react-native';
import { TurboModuleRegistry } from 'react-native';

export interface Spec extends TurboModule {
  /**
   * Open (or create) a TalaDB database file at the platform default path.
   * Must be called once at app startup before using `collection()`.
   *
   * @param configJson Optional JSON-serialised durability/encryption config.
   */
  initialize(dbName: string, configJson?: string): Promise<void>;

  /** Close the database and flush all pending writes. */
  close(): Promise<void>;

  // ------------------------------------------------------------------
  // Collection CRUD — all methods are synchronous via JSI
  // ------------------------------------------------------------------

  /** Insert a document. Honours a supplied `_id`, otherwise creates a ULID. */
  insert(collection: string, doc: Object): string;

  /** Insert multiple documents, honouring any supplied `_id` values. */
  insertMany(collection: string, docs: Object[]): string[];

  /** Find documents matching the filter. */
  find(collection: string, filter: Object | null): Object[];

  /** Find a single document or null. */
  findOne(collection: string, filter: Object | null): Object | null;

  /** Update the first matching document. Returns true if updated. */
  updateOne(collection: string, filter: Object, update: Object): boolean;

  /** Update all matching documents. Returns the count updated. */
  updateMany(collection: string, filter: Object, update: Object): number;

  /** Delete the first matching document. Returns true if deleted. */
  deleteOne(collection: string, filter: Object): boolean;

  /** Delete all matching documents. Returns the count deleted. */
  deleteMany(collection: string, filter: Object): number;

  /** Count documents matching the filter. */
  count(collection: string, filter: Object | null): number;

  // ------------------------------------------------------------------
  // Index management
  // ------------------------------------------------------------------

  createIndex(collection: string, field: string): void;
  dropIndex(collection: string, field: string): void;
  createFtsIndex(collection: string, field: string): void;
  dropFtsIndex(collection: string, field: string): void;

  // ------------------------------------------------------------------
  // Vector index + similarity search
  //
  // Sync CRUD is routed through the JSI HostObject; these stubs exist so
  // TurboModule Codegen sees the full surface area. At runtime, all calls
  // hit `global.__TalaDB__` directly — the spec types are advisory.
  // ------------------------------------------------------------------

  createVectorIndex(
    collection: string,
    field: string,
    dimensions: number,
    opts: Object | null,
  ): void;
  dropVectorIndex(collection: string, field: string): void;
  upgradeVectorIndex(collection: string, field: string): void;

  /**
   * Hybrid similarity search. `query` accepts a `Float32Array` (zero-copy
   * fast path) or `number[]` — the HostObject handles both at runtime.
   */
  findNearest(
    collection: string,
    field: string,
    query: Array<number>,
    topK: number,
    filter: Object | null,
  ): Array<Object>;

  // ------------------------------------------------------------------
  // Async variants — work runs on a background OS thread. Use these for
  // large collection scans or unfiltered vector searches to keep the JS
  // thread responsive.
  // ------------------------------------------------------------------

  findNearestAsync(
    collection: string,
    field: string,
    query: Array<number>,
    topK: number,
    filter: Object | null,
  ): Promise<Array<Object>>;

  findAsync(collection: string, filter: Object | null): Promise<Array<Object>>;
}

export default TurboModuleRegistry.getEnforcing<Spec>('TalaDB');
