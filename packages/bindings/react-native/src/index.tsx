/** TalaDB React Native public API. CRUD is synchronous through the JSI host. */
import {
  createWebhookDispatcher,
  type WebhookConfig,
  type WebhookDispatcher,
  type WebhookEvent,
  type WebhookStats,
} from 'taladb';
import NativeTalaDB from './NativeTalaDB';

export const TalaDBModule = {
  /** Open the native database. The config controls durability/encryption only. */
  initialize: (dbName: string, configJson?: string) =>
    NativeTalaDB.initialize(dbName, configJson),
  close: () => NativeTalaDB.close(),
};

export interface Document {
  _id?: string;
  [key: string]: unknown;
}

export type InsertDocument<T extends Document> = Omit<T, '_id'> & { _id?: string };
export type Filter = Record<string, unknown>;
export type Update = Record<string, unknown>;

export interface Collection<T extends Document = Document> {
  insert(doc: InsertDocument<T>): string;
  insertMany(docs: InsertDocument<T>[]): string[];
  find(filter?: Filter): T[];
  findOne(filter: Filter): T | null;
  updateOne(filter: Filter, update: Update): boolean;
  updateMany(filter: Filter, update: Update): number;
  deleteOne(filter: Filter): boolean;
  deleteMany(filter: Filter): number;
  count(filter?: Filter): number;
  createIndex(field: string): void;
  dropIndex(field: string): void;
  createFtsIndex(field: string): void;
  dropFtsIndex(field: string): void;
}

export interface OpenDBOptions {
  /** Runtime-agnostic outbound change webhook, delivered with global `fetch`. */
  webhook?: WebhookConfig;
}

export interface DB {
  collection<T extends Document = Document>(name: string): Collection<T>;
  webhookStats(): WebhookStats;
  flushWebhook(timeoutMs?: number): Promise<boolean>;
  close(): Promise<void>;
}

interface JsiTalaDB {
  insert(collection: string, doc: Object): string;
  insertMany(collection: string, docs: Object[]): string[];
  find(collection: string, filter: Object | null): Object[];
  findOne(collection: string, filter: Object | null): Object | null;
  updateOne(collection: string, filter: Object, update: Object): boolean;
  updateMany(collection: string, filter: Object, update: Object): number;
  deleteOne(collection: string, filter: Object): boolean;
  deleteMany(collection: string, filter: Object): number;
  count(collection: string, filter: Object | null): number;
  createIndex(collection: string, field: string): void;
  dropIndex(collection: string, field: string): void;
  createFtsIndex(collection: string, field: string): void;
  dropFtsIndex(collection: string, field: string): void;
}

function native(): JsiTalaDB {
  const host = (globalThis as { __TalaDB__?: JsiTalaDB }).__TalaDB__;
  if (!host) throw new Error('TalaDB is not initialized; await TalaDBModule.initialize() first');
  return host;
}

const EMPTY_STATS: WebhookStats = { pending: 0, delivered: 0, failed: 0, dropped: 0 };

function emitPostImages<T extends Document>(
  webhook: WebhookDispatcher | null,
  collection: string,
  ids: string[],
  op: 'insert' | 'update',
  committedAt: number,
): void {
  if (!webhook?.reports(collection) || ids.length === 0) return;
  const docs = native().find(collection, { _id: { $in: ids } }) as T[];
  const byId = new Map(docs.map((doc) => [doc._id, doc]));
  for (const id of ids) {
    const document = byId.get(id);
    if (document) webhook.emit({ op, collection, id, document, committedAt } as WebhookEvent);
  }
}

function emitDeletes<T extends Document>(
  webhook: WebhookDispatcher | null,
  collection: string,
  docs: T[],
  committedAt: number,
): void {
  if (!webhook?.reports(collection)) return;
  for (const document of docs) {
    if (typeof document._id !== 'string') continue;
    // DELETE carries the pre-image: after commit there is no post-image to read.
    webhook.emit({
      op: 'delete',
      collection,
      id: document._id,
      document,
      committedAt,
    } as WebhookEvent);
  }
}

function collection<T extends Document>(
  colName: string,
  webhook: WebhookDispatcher | null,
): Collection<T> {
  return {
    insert(doc) {
      const id = native().insert(colName, doc as Object);
      const committedAt = Date.now();
      emitPostImages<T>(webhook, colName, [id], 'insert', committedAt);
      return id;
    },
    insertMany(docs) {
      const ids = native().insertMany(colName, docs as Object[]);
      const committedAt = Date.now();
      emitPostImages<T>(webhook, colName, ids, 'insert', committedAt);
      return ids;
    },
    find: (filter) => native().find(colName, filter ?? null) as T[],
    findOne: (filter) => native().findOne(colName, filter) as T | null,
    updateOne(filter, update) {
      const before = native().findOne(colName, filter) as T | null;
      const changed = native().updateOne(colName, filter, update);
      const committedAt = Date.now();
      if (changed && typeof before?._id === 'string') {
        emitPostImages<T>(webhook, colName, [before._id], 'update', committedAt);
      }
      return changed;
    },
    updateMany(filter, update) {
      const before = native().find(colName, filter) as T[];
      const changed = native().updateMany(colName, filter, update);
      const committedAt = Date.now();
      if (changed > 0) {
        emitPostImages<T>(
          webhook,
          colName,
          before.map((doc) => doc._id).filter((id): id is string => typeof id === 'string'),
          'update',
          committedAt,
        );
      }
      return changed;
    },
    deleteOne(filter) {
      const before = native().findOne(colName, filter) as T | null;
      const deleted = native().deleteOne(colName, filter);
      const committedAt = Date.now();
      if (deleted && before) emitDeletes(webhook, colName, [before], committedAt);
      return deleted;
    },
    deleteMany(filter) {
      const before = native().find(colName, filter) as T[];
      const deleted = native().deleteMany(colName, filter);
      const committedAt = Date.now();
      if (deleted > 0) emitDeletes(webhook, colName, before, committedAt);
      return deleted;
    },
    count: (filter) => native().count(colName, filter ?? null),
    createIndex: (field) => native().createIndex(colName, field),
    dropIndex: (field) => native().dropIndex(colName, field),
    createFtsIndex: (field) => native().createFtsIndex(colName, field),
    dropFtsIndex: (field) => native().dropFtsIndex(colName, field),
  };
}

/** Get a synchronous DB handle after `TalaDBModule.initialize(dbName)`. */
export function openDB(_dbName: string, options?: OpenDBOptions): DB {
  const webhook = createWebhookDispatcher(options?.webhook);
  return {
    collection: <T extends Document>(name: string) => collection<T>(name, webhook),
    webhookStats: () => webhook?.stats() ?? { ...EMPTY_STATS },
    flushWebhook: (timeoutMs) => webhook?.flush(timeoutMs) ?? Promise.resolve(true),
    close: async () => {
      await webhook?.flush();
      await NativeTalaDB.close();
    },
  };
}
