// src/config.browser.ts
var ENDPOINT_FIELDS = [
  "endpoint",
  "insert_endpoint",
  "update_endpoint",
  "delete_endpoint"
];
function validateConfig(config) {
  const sync = config.sync;
  if (!sync) return;
  for (const key of ENDPOINT_FIELDS) {
    const url = sync[key];
    if (url !== void 0 && !url.startsWith("http://") && !url.startsWith("https://")) {
      throw new Error(
        `TalaDB config: invalid endpoint URL "${url}" \u2014 must start with http:// or https://`
      );
    }
  }
}
async function loadConfig(_configPath) {
  return {};
}

// src/webhook.ts
var METHOD = {
  insert: "POST",
  update: "PUT",
  delete: "DELETE"
};
var ENDPOINT_KEYS = [
  "endpoint",
  "insert_endpoint",
  "update_endpoint",
  "delete_endpoint"
];
var LOCALHOST = /* @__PURE__ */ new Set(["localhost", "127.0.0.1", "[::1]", "::1"]);
function isLocalhost(url) {
  try {
    return LOCALHOST.has(new URL(url).hostname);
  } catch {
    return false;
  }
}
function validateWebhookConfig(config) {
  for (const key of ENDPOINT_KEYS) {
    const url = config[key];
    if (url === void 0) continue;
    if (!url.startsWith("http://") && !url.startsWith("https://")) {
      throw new Error(
        `TalaDB webhook: invalid ${key} "${url}" \u2014 must start with http:// or https://`
      );
    }
    if (url.startsWith("http://") && !isLocalhost(url)) {
      console.warn(
        `[TalaDB] webhook ${key} "${url}" uses plaintext HTTP \u2014 document bodies will cross the network unencrypted. Use HTTPS in production.`
      );
    }
  }
  if (config.enabled && !config.endpoint) {
    const perOp = ENDPOINT_KEYS.slice(1).every((k) => config[k] !== void 0);
    if (!perOp) {
      throw new Error(
        "TalaDB webhook: `enabled: true` requires `endpoint` (or all three per-op endpoints)"
      );
    }
  }
}
var DEFAULT_MAX_QUEUE = 512;
var DEFAULT_RETRIES = 3;
var BACKOFF_BASE_MS = 200;
var sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
function stripFields(doc, exclude) {
  if (exclude.size === 0) return doc;
  const out = {};
  for (const [k, v] of Object.entries(doc)) {
    if (!exclude.has(k)) out[k] = v;
  }
  return out;
}
function createWebhookDispatcher(config) {
  if (!config?.enabled) return null;
  validateWebhookConfig(config);
  const fetchFn = config.fetch ?? globalThis.fetch?.bind(globalThis);
  if (!fetchFn) {
    throw new Error(
      "TalaDB webhook: no global fetch available. Pass `fetch` in the webhook config."
    );
  }
  const headers = { "content-type": "application/json", ...config.headers ?? {} };
  const exclude = new Set(config.exclude_fields ?? []);
  const only = config.collections ? new Set(config.collections) : null;
  const maxQueue = config.max_queue ?? DEFAULT_MAX_QUEUE;
  const retries = config.retries ?? DEFAULT_RETRIES;
  const stats = { pending: 0, delivered: 0, failed: 0, dropped: 0 };
  const chains = /* @__PURE__ */ new Map();
  function endpointFor(op) {
    return config[`${op}_endpoint`] ?? config.endpoint;
  }
  function bodyFor(event, at) {
    const base = {
      collection: event.collection,
      id: event.id,
      timestamp: at
    };
    return JSON.stringify(
      event.op === "delete" ? base : { ...base, document: stripFields(event.document, exclude) }
    );
  }
  async function deliver(event, at) {
    const url = endpointFor(event.op);
    const init = {
      method: METHOD[event.op],
      headers,
      body: bodyFor(event, at)
    };
    for (let attempt = 0; attempt <= retries; attempt++) {
      try {
        const res = await fetchFn(url, init);
        if (res.ok) {
          stats.delivered++;
          return;
        }
        if (res.status >= 400 && res.status < 500) {
          stats.failed++;
          console.warn(
            `[TalaDB] webhook ${event.op} ${event.collection}/${event.id} \u2192 ${res.status} ${res.statusText} (not retried)`
          );
          return;
        }
      } catch {
      }
      if (attempt < retries) await sleep(BACKOFF_BASE_MS * 2 ** attempt);
    }
    stats.failed++;
    console.warn(
      `[TalaDB] webhook ${event.op} ${event.collection}/${event.id} failed after ${retries + 1} attempts`
    );
  }
  return {
    reports(collection) {
      if (collection.startsWith("_")) return false;
      return only === null || only.has(collection);
    },
    emit(event) {
      if (!this.reports(event.collection)) return;
      if (stats.pending >= maxQueue) {
        stats.dropped++;
        console.warn(
          `[TalaDB] webhook queue full (${maxQueue}) \u2014 dropped ${event.op} ${event.collection}/${event.id}. The endpoint is not keeping up.`
        );
        return;
      }
      stats.pending++;
      const at = Date.now();
      const key = `${event.collection}\0${event.id}`;
      const prior = chains.get(key) ?? Promise.resolve();
      const next = prior.then(() => deliver(event, at)).finally(() => {
        stats.pending--;
        if (chains.get(key) === next) chains.delete(key);
      });
      chains.set(key, next);
    },
    async flush(timeoutMs = 5e3) {
      const deadline = Date.now() + timeoutMs;
      while (stats.pending > 0) {
        const remaining = deadline - Date.now();
        if (remaining <= 0) break;
        let timer;
        const expired = new Promise((resolve) => {
          timer = setTimeout(resolve, remaining);
        });
        try {
          await Promise.race([Promise.allSettled([...chains.values()]), expired]);
        } finally {
          clearTimeout(timer);
        }
      }
      return stats.pending === 0;
    },
    stats() {
      return { ...stats };
    }
  };
}
function byIds(ids) {
  return ids.length === 1 ? { _id: ids[0] } : { _id: { $in: ids } };
}
function idFromFilter(filter) {
  if (filter === null || typeof filter !== "object") return null;
  const keys = Object.keys(filter);
  if (keys.length !== 1 || keys[0] !== "_id") return null;
  const id = filter._id;
  return typeof id === "string" ? id : null;
}
function wrapCollectionWithWebhook(col, collection, webhook) {
  if (!webhook.reports(collection)) return col;
  async function idsFor(filter, limitOne) {
    const fast = idFromFilter(filter);
    if (fast !== null) return [fast];
    if (limitOne) {
      const doc = await col.findOne(filter);
      return doc && typeof doc._id === "string" ? [doc._id] : [];
    }
    const docs = col.aggregate ? await col.aggregate([{ $match: filter }, { $project: { _id: 1 } }]) : await col.find(filter);
    return docs.map((d) => d._id).filter((id) => typeof id === "string");
  }
  async function emitPostImages(ids, op) {
    if (ids.length === 0) return;
    const docs = await col.find(byIds(ids));
    const byId = /* @__PURE__ */ new Map();
    for (const doc of docs) {
      if (typeof doc._id === "string") byId.set(doc._id, doc);
    }
    for (const id of ids) {
      const doc = byId.get(id);
      if (doc === void 0) webhook.emit({ op: "delete", collection, id });
      else webhook.emit({ op, collection, id, document: doc });
    }
  }
  return {
    ...col,
    async insert(doc) {
      const id = await col.insert(doc);
      await emitPostImages([id], "insert");
      return id;
    },
    async insertMany(docs) {
      const ids = await col.insertMany(docs);
      await emitPostImages(ids, "insert");
      return ids;
    },
    async updateOne(filter, update) {
      const ids = await idsFor(filter, true);
      const changed = await col.updateOne(filter, update);
      if (changed) await emitPostImages(ids, "update");
      return changed;
    },
    async updateMany(filter, update) {
      const ids = await idsFor(filter, false);
      const n = await col.updateMany(filter, update);
      if (n > 0) await emitPostImages(ids, "update");
      return n;
    },
    async deleteOne(filter) {
      const ids = await idsFor(filter, true);
      const deleted = await col.deleteOne(filter);
      if (deleted) {
        for (const id of ids) webhook.emit({ op: "delete", collection, id });
      }
      return deleted;
    },
    async deleteMany(filter) {
      const ids = await idsFor(filter, false);
      const n = await col.deleteMany(filter);
      if (n > 0) {
        for (const id of ids) webhook.emit({ op: "delete", collection, id });
      }
      return n;
    }
  };
}

// src/derive-id.ts
var FNV1A128_OFFSET_BASIS = 0x6c62272e07bb014262b821756295c58dn;
var FNV1A128_PRIME = 0x0000000001000000000000000000013bn;
var MASK_128 = (1n << 128n) - 1n;
var CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
var UTF8 = new TextEncoder();
function encodeUlid(value) {
  let out = "";
  for (let i = 25; i >= 0; i--) {
    out += CROCKFORD[Number(value >> BigInt(i * 5) & 31n)];
  }
  return out;
}
function deriveDocId(collection, key) {
  const bytes = [...UTF8.encode(collection), 0, ...UTF8.encode(key)];
  let hash = FNV1A128_OFFSET_BASIS;
  for (const byte of bytes) {
    hash ^= BigInt(byte);
    hash = hash * FNV1A128_PRIME & MASK_128;
  }
  return encodeUlid(hash);
}

// src/index.ts
var TalaDbValidationError = class extends Error {
  constructor(cause, context) {
    const label = context ? ` (${context})` : "";
    const msg = cause instanceof Error ? cause.message : String(cause);
    super(`TalaDB schema validation failed${label}: ${msg}`);
    this.cause = cause;
    this.name = "TalaDbValidationError";
  }
};
function deepEqual(a, b) {
  if (a === b) return true;
  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
    return a.every((x, i) => deepEqual(x, b[i]));
  }
  const ao = a;
  const bo = b;
  const keys = Object.keys(ao);
  if (keys.length !== Object.keys(bo).length) return false;
  return keys.every(
    (k) => Object.prototype.hasOwnProperty.call(bo, k) && deepEqual(ao[k], bo[k])
  );
}
function applySchema(col, options) {
  const {
    schema,
    validateOnRead = false,
    migrateDocument,
    downgradeDocument,
    syncSchema,
    persistMigrations = false,
    allowFieldRemoval = false,
    retiredFields = []
  } = options;
  const targetVersion = syncSchema?.version ?? 0;
  if (migrateDocument && targetVersion < 1) {
    throw new Error("CollectionOptions.migrateDocument requires syncSchema.version (the migration target)");
  }
  if (downgradeDocument && targetVersion < 1) {
    throw new Error("CollectionOptions.downgradeDocument requires syncSchema.version (the shape this build reads)");
  }
  if (syncSchema && targetVersion < 1 && (syncSchema.renames || syncSchema.defaults)) {
    throw new Error(
      "CollectionOptions.syncSchema.renames/defaults require syncSchema.version >= 1 \u2014 without a version the import migration step never runs and documents missing the renamed/defaulted fields are quarantined instead of upgraded"
    );
  }
  const stampVersion = targetVersion > 0;
  if (!schema && !migrateDocument && !downgradeDocument && !stampVersion) return col;
  const retired = new Set(retiredFields);
  const engineOwned = /* @__PURE__ */ new Set([
    "_id",
    "_v",
    "_changed_at"
  ]);
  const downcastViews = /* @__PURE__ */ new WeakSet();
  function preserveFields(original, next, preserveUnknown, preserveVersion = true) {
    let out = null;
    for (const k of Object.keys(original)) {
      const ownedField = engineOwned.has(k) && (k !== "_v" || preserveVersion);
      const mustRestore = ownedField || preserveUnknown && !retired.has(k) && !(k in next);
      if (!mustRestore) continue;
      if (!ownedField && k in next) continue;
      out ?? (out = { ...next });
      out[k] = original[k];
    }
    return out ?? next;
  }
  function parseWrite(doc, label) {
    try {
      return schema.parse(doc);
    } catch (err) {
      throw new TalaDbValidationError(err, label);
    }
  }
  function assertWritableDocument(doc, label) {
    if (doc && typeof doc === "object" && downcastViews.has(doc)) {
      throw new Error(`${label}: a downgradeDocument result is a read-only compatibility view`);
    }
    const version = doc?._v;
    if (targetVersion > 0 && typeof version === "number" && version > targetVersion) {
      throw new Error(
        `${label}: this client supports schema v${targetVersion}, but the document is v${version}`
      );
    }
  }
  function writableFilter(filter) {
    if (targetVersion < 1) return filter;
    return {
      $and: [
        filter,
        {
          $or: [
            { _v: { $exists: false } },
            { _v: { $lte: targetVersion } }
          ]
        }
      ]
    };
  }
  function assertSafeUpdate(update) {
    const record = update;
    for (const op of ["$set", "$unset", "$inc", "$push", "$pull"]) {
      const fields = record[op];
      if (!fields) continue;
      for (const field of Object.keys(fields)) {
        if (engineOwned.has(field)) {
          throw new Error(`update cannot modify engine-owned field '${field}'`);
        }
      }
    }
  }
  function stamp(doc) {
    if (!stampVersion || doc._v !== void 0) return doc;
    return { ...doc, _v: targetVersion };
  }
  function diffUpdate(original, migrated) {
    const $set = {};
    const $unset = {};
    for (const k of Object.keys(migrated)) {
      if (k === "_id") continue;
      if (!deepEqual(migrated[k], original[k])) $set[k] = migrated[k];
    }
    if (allowFieldRemoval || retired.size > 0) {
      for (const k of Object.keys(original)) {
        if (k !== "_id" && !(k in migrated) && (allowFieldRemoval || retired.has(k))) $unset[k] = true;
      }
    }
    const update = {};
    if (Object.keys($set).length) update.$set = $set;
    if (Object.keys($unset).length) update.$unset = $unset;
    return Object.keys(update).length ? update : null;
  }
  function normalizeRead(doc) {
    const fromVersion = typeof doc._v === "number" ? doc._v : 0;
    if (migrateDocument && fromVersion < targetVersion) {
      const up = { ...migrateDocument(doc, fromVersion), _v: targetVersion };
      return {
        // The migration owns the version transition, while every other
        // engine-owned field continues to come from the stored document.
        value: allowFieldRemoval ? preserveFields(doc, up, false, false) : preserveFields(doc, up, true, false),
        persistable: true
      };
    }
    if (downgradeDocument && fromVersion > targetVersion) {
      const projected = {
        ...downgradeDocument(doc, fromVersion),
        ...doc._id !== void 0 ? { _id: doc._id } : {},
        _v: fromVersion
      };
      downcastViews.add(projected);
      return { value: projected, persistable: false };
    }
    return { value: doc, persistable: true };
  }
  function validateRead(doc) {
    if (!validateOnRead || !schema) return doc;
    try {
      const parsed = schema.parse(doc);
      const fromVersion = typeof doc._v === "number" ? doc._v : 0;
      return preserveFields(doc, parsed, targetVersion > 0 && fromVersion > targetVersion);
    } catch (err) {
      throw new TalaDbValidationError(err, "read");
    }
  }
  async function persistAll(originals, normalized) {
    if (!persistMigrations) return;
    for (let i = 0; i < originals.length; i++) {
      const original = originals[i];
      if (!normalized[i].persistable) continue;
      const migrated = normalized[i].value;
      if (migrated === original || typeof original._id !== "string") continue;
      const update = diffUpdate(original, migrated);
      if (!update) continue;
      try {
        const guards = [{ _id: original._id }];
        if (original._v === void 0) guards.push({ _v: { $exists: false } });
        else guards.push({ _v: original._v });
        if (original._changed_at !== void 0) {
          guards.push({ _changed_at: original._changed_at });
        }
        await col.updateOne({ $and: guards }, update);
      } catch {
      }
    }
  }
  const wrapReads = Boolean(migrateDocument) || Boolean(downgradeDocument) || validateOnRead && Boolean(schema);
  const wrapWrites = Boolean(schema) || stampVersion;
  function pipelinePreservesDocuments(pipeline) {
    return pipeline.every((stage) => !("$group" in stage) && !("$project" in stage));
  }
  function normalizeViewRows(docs) {
    return docs.map((doc) => validateRead(normalizeRead(doc).value));
  }
  return {
    ...col,
    insert: wrapWrites ? async (doc) => {
      assertWritableDocument(doc, "insert");
      if (schema) parseWrite(doc, "insert");
      return col.insert(stamp(doc));
    } : col.insert.bind(col),
    insertMany: wrapWrites ? async (docs) => {
      docs.forEach((doc, i) => assertWritableDocument(doc, `insertMany[${i}]`));
      if (schema) docs.forEach((doc, i) => parseWrite(doc, `insertMany[${i}]`));
      return col.insertMany(docs.map(stamp));
    } : col.insertMany.bind(col),
    updateOne: wrapWrites ? async (filter, update) => {
      assertSafeUpdate(update);
      return col.updateOne(writableFilter(filter), update);
    } : col.updateOne.bind(col),
    updateMany: wrapWrites ? async (filter, update) => {
      assertSafeUpdate(update);
      return col.updateMany(writableFilter(filter), update);
    } : col.updateMany.bind(col),
    deleteOne: stampVersion ? (filter) => col.deleteOne(writableFilter(filter)) : col.deleteOne.bind(col),
    deleteMany: stampVersion ? (filter) => col.deleteMany(writableFilter(filter)) : col.deleteMany.bind(col),
    find: wrapReads ? async (filter) => {
      const docs = await col.find(filter);
      const normalized = docs.map(normalizeRead);
      await persistAll(docs, normalized);
      return normalized.map((n) => validateRead(n.value));
    } : col.find.bind(col),
    findOne: wrapReads ? async (filter) => {
      const doc = await col.findOne(filter);
      if (doc === null) return null;
      const normalized = normalizeRead(doc);
      await persistAll([doc], [normalized]);
      return validateRead(normalized.value);
    } : col.findOne.bind(col),
    aggregate: wrapReads ? async (pipeline) => {
      const docs = await col.aggregate(pipeline);
      return pipelinePreservesDocuments(pipeline) ? normalizeViewRows(docs) : docs;
    } : col.aggregate.bind(col),
    // Live queries feed every @taladb/react hook (useFind, useFindOne,
    // useQueries). Leaving them unwrapped meant React components received the
    // un-migrated shape while a direct find() returned the migrated one.
    subscribe: wrapReads ? (filter, callback, onError) => col.subscribe(
      filter,
      (docs) => {
        const normalized = docs.map(normalizeRead);
        let out;
        try {
          out = normalized.map((n) => validateRead(n.value));
        } catch (err) {
          onError?.(err);
          return;
        }
        callback(out);
        void persistAll(docs, normalized);
      },
      onError
    ) : col.subscribe.bind(col),
    subscribeAggregate: wrapReads ? (pipeline, callback, onError) => col.subscribeAggregate(
      pipeline,
      (docs) => {
        if (!pipelinePreservesDocuments(pipeline)) {
          callback(docs);
          return;
        }
        try {
          callback(normalizeViewRows(docs));
        } catch (error) {
          onError?.(error);
        }
      },
      onError
    ) : col.subscribeAggregate.bind(col)
  };
}
function decorateCollection(raw, name, opts, webhook) {
  const reported = webhook ? wrapCollectionWithWebhook(raw, name, webhook) : raw;
  return opts ? applySchema(reported, opts) : reported;
}
function detectPlatform() {
  if (typeof navigator !== "undefined" && navigator.product === "ReactNative") {
    return "react-native";
  }
  if (globalThis.nativeCallSyncHook !== void 0) {
    return "react-native";
  }
  if (globalThis.window !== void 0 && typeof navigator !== "undefined") {
    return "browser";
  }
  return "node";
}
var WorkerProxy = class {
  constructor(port) {
    this.pending = /* @__PURE__ */ new Map();
    this.nextId = 1;
    this.dead = null;
    this.port = port;
    this.port.onmessage = (e) => {
      const { id, result, error } = e.data;
      const p = this.pending.get(id);
      if (p) {
        this.pending.delete(id);
        if (error === void 0) p.resolve(result);
        else p.reject(new Error(error));
      }
    };
    this.port.start?.();
  }
  send(op, args = {}) {
    if (this.dead) return Promise.reject(this.dead);
    return new Promise((resolve, reject) => {
      const id = this.nextId++;
      this.pending.set(id, { resolve, reject });
      this.port.postMessage({ id, op, ...args });
    });
  }
  /**
   * Reject every in-flight request and refuse new ones. Called when the
   * worker errors or is terminated — without this, pending promises would
   * hang forever (awaiting callers deadlock).
   */
  abort(reason) {
    this.dead = reason;
    for (const [, p] of this.pending) p.reject(reason);
    this.pending.clear();
  }
};
function makePoller(findFn, callback, onError) {
  let active = true;
  let lastJson = "";
  let running = false;
  let rerun = false;
  const poll = async () => {
    if (!active) return;
    if (running) {
      rerun = true;
      return;
    }
    running = true;
    try {
      const docs = await findFn();
      if (!active) return;
      const json = JSON.stringify(docs);
      if (json !== lastJson) {
        lastJson = json;
        callback(docs);
      }
    } catch (error) {
      if (active) onError?.(error);
    } finally {
      running = false;
      if (active) {
        if (rerun) {
          rerun = false;
          void poll();
        } else setTimeout(poll, 300);
      }
    }
  };
  poll();
  return () => {
    active = false;
  };
}
async function createBrowserDB(dbName, webhook, config, passphrase, migrations) {
  const workerUrl = new URL("@taladb/web/worker/taladb.worker.js", "react-native://unreachable");
  const worker = new Worker(workerUrl, { type: "module", name: "taladb" });
  const proxy = new WorkerProxy(worker);
  worker.onerror = (e) => {
    proxy.abort(new Error(`taladb worker error: ${e.message ?? "unknown"}`));
  };
  const configJson = config !== void 0 ? JSON.stringify(config) : void 0;
  try {
    await proxy.send("init", { dbName, configJson, passphrase });
  } catch (e) {
    proxy.abort(e instanceof Error ? e : new Error(String(e)));
    worker.terminate();
    throw e;
  }
  const nudgeCallbacks = /* @__PURE__ */ new Set();
  let channel = null;
  if (typeof BroadcastChannel !== "undefined") {
    channel = new BroadcastChannel(`taladb:${dbName}`);
    channel.onmessage = (e) => {
      if (e.data === "taladb:changed") {
        for (const nudge of nudgeCallbacks) nudge();
      }
    };
  }
  function wrapCollection(name, opts) {
    const s = JSON.stringify;
    const wrapped = {
      insert: (doc) => proxy.send("insert", { collection: name, docJson: s(doc) }),
      insertMany: async (docs) => {
        const json = await proxy.send("insertMany", {
          collection: name,
          docsJson: s(docs)
        });
        return JSON.parse(json);
      },
      find: async (filter) => {
        const json = await proxy.send("find", {
          collection: name,
          filterJson: filter ? s(filter) : "null"
        });
        return JSON.parse(json);
      },
      findOne: async (filter) => {
        const json = await proxy.send("findOne", {
          collection: name,
          filterJson: filter ? s(filter) : "null"
        });
        return JSON.parse(json);
      },
      updateOne: (filter, update) => proxy.send("updateOne", {
        collection: name,
        filterJson: s(filter),
        updateJson: s(update)
      }),
      updateMany: (filter, update) => proxy.send("updateMany", {
        collection: name,
        filterJson: s(filter),
        updateJson: s(update)
      }),
      deleteOne: (filter) => proxy.send("deleteOne", { collection: name, filterJson: s(filter) }),
      deleteMany: (filter) => proxy.send("deleteMany", { collection: name, filterJson: s(filter) }),
      count: (filter) => proxy.send("count", {
        collection: name,
        filterJson: filter ? s(filter) : "null"
      }),
      aggregate: async (pipeline) => {
        const json = await proxy.send("aggregate", {
          collection: name,
          pipelineJson: s(pipeline)
        });
        return JSON.parse(json);
      },
      createIndex: (field) => proxy.send("createIndex", { collection: name, field }),
      dropIndex: (field) => proxy.send("dropIndex", { collection: name, field }),
      createCompoundIndex: (fields) => proxy.send("createCompoundIndex", { collection: name, fieldsJson: JSON.stringify(fields) }),
      dropCompoundIndex: (fields) => proxy.send("dropCompoundIndex", { collection: name, fieldsJson: JSON.stringify(fields) }),
      createFtsIndex: (field) => proxy.send("createFtsIndex", { collection: name, field }),
      dropFtsIndex: (field) => proxy.send("dropFtsIndex", { collection: name, field }),
      createVectorIndex: (field, options) => {
        if (options.indexType === "hnsw") return Promise.reject(new Error("HNSW vector indexes are not available in the browser (requires native threads). Use Node.js or React Native."));
        return proxy.send("createVectorIndex", {
          collection: name,
          field,
          dimensions: options.dimensions,
          metric: options.metric,
          indexType: null,
          hnswM: null,
          hnswEfConstruction: null
        });
      },
      dropVectorIndex: (field) => proxy.send("dropVectorIndex", { collection: name, field }),
      upgradeVectorIndex: (_field) => Promise.reject(new Error("HNSW vector indexes are not available in the browser (requires native threads). Use Node.js or React Native.")),
      listIndexes: async () => {
        const json = await proxy.send("listIndexes", { collection: name });
        return JSON.parse(json);
      },
      findNearest: async (field, vector, topK, filter) => {
        const json = await proxy.send("findNearest", {
          collection: name,
          field,
          queryJson: JSON.stringify(vector),
          topK,
          filterJson: filter ? JSON.stringify(filter) : "null"
        });
        return JSON.parse(json);
      },
      searchText: async (field, query, topK, filter, options) => {
        const json = await proxy.send("searchText", {
          collection: name,
          field,
          query,
          topK,
          filterJson: filter ? JSON.stringify(filter) : "null",
          optionsJson: options ? JSON.stringify(options) : "null"
        });
        return JSON.parse(json);
      },
      hybridSearch: async (text, vector, topK, filter, options) => {
        const json = await proxy.send("hybridSearch", {
          collection: name,
          textField: text.textField,
          text: text.text,
          vectorField: vector.vectorField,
          vectorJson: JSON.stringify(vector.vector),
          topK,
          filterJson: filter ? JSON.stringify(filter) : "null",
          optionsJson: options ? JSON.stringify(options) : "null"
        });
        return JSON.parse(json);
      },
      subscribe: (filter, callback, onError) => nudgedPoller(
        name,
        () => proxy.send("find", {
          collection: name,
          filterJson: filter ? s(filter) : "null"
        }),
        callback,
        onError
      ),
      subscribeAggregate: (pipeline, callback, onError) => nudgedPoller(
        name,
        () => proxy.send("aggregate", {
          collection: name,
          pipelineJson: s(pipeline)
        }),
        callback,
        onError
      )
    };
    return decorateCollection(wrapped, name, opts, webhook);
  }
  function nudgedPoller(collection, fetchJson, callback, onError) {
    let active = true;
    let lastJson = "";
    let lastGeneration = -1;
    let generationSupported = true;
    let timer = null;
    let running = false;
    let rerun = false;
    const poll = async () => {
      if (!active) return;
      if (running) {
        rerun = true;
        return;
      }
      running = true;
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      try {
        let unchanged = false;
        if (generationSupported) {
          try {
            const gen = await proxy.send("writeGeneration", { collection });
            if (gen === lastGeneration) unchanged = true;
            else lastGeneration = gen;
          } catch {
            generationSupported = false;
          }
        }
        if (!unchanged) {
          const json = await fetchJson();
          if (!active) return;
          if (json !== lastJson) {
            lastJson = json;
            callback(JSON.parse(json));
          }
        }
      } catch (error) {
        if (active) onError?.(error);
      } finally {
        running = false;
      }
      if (active) {
        if (rerun) {
          rerun = false;
          void poll();
        } else timer = setTimeout(poll, 300);
      }
    };
    nudgeCallbacks.add(poll);
    poll();
    return () => {
      active = false;
      nudgeCallbacks.delete(poll);
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
    };
  }
  const handle = {
    collection: (name, opts) => wrapCollection(name, opts),
    compact: () => proxy.send("compact"),
    flush: async () => {
      await proxy.send("flush");
    },
    close: async () => {
      channel?.close();
      try {
        await proxy.send("close");
      } finally {
        worker.terminate();
        proxy.abort(new Error("taladb worker closed"));
      }
    },
    listCollectionNames: async () => JSON.parse(await proxy.send("listCollections"))
  };
  if (migrations?.length) {
    await runMigrations(
      handle,
      async () => proxy.send("userVersion"),
      async (v) => {
        await proxy.send("setUserVersion", { version: v });
      },
      migrations
    );
  }
  return handle;
}
async function createNodeDB(dbName, webhook, config, passphrase, migrations) {
  const native = await import("./node-A4LKRSW5.mjs");
  const TalaDBNode = native.TalaDbNode ?? native.TalaDBNode;
  if (!TalaDBNode) throw new Error("@taladb/node loaded but exports no TalaDbNode class \u2014 rebuild the native module");
  const configJson = config !== void 0 ? JSON.stringify(config) : null;
  const db = TalaDBNode.open(dbName, configJson, passphrase ?? null);
  function wrapCollection(name, opts) {
    const col = db.collection(name);
    const wrapped = {
      insert: async (doc) => col.insertAsync ? col.insertAsync(doc) : col.insert(doc),
      insertMany: async (docs) => col.insertManyAsync ? col.insertManyAsync(docs) : col.insertMany(docs),
      find: async (filter) => col.findAsync ? col.findAsync(filter ?? null) : col.find(filter ?? null),
      findOne: async (filter) => col.findOne(filter) ?? null,
      updateOne: async (filter, update) => col.updateOneAsync ? col.updateOneAsync(filter, update) : col.updateOne(filter, update),
      updateMany: async (filter, update) => col.updateManyAsync ? col.updateManyAsync(filter, update) : col.updateMany(filter, update),
      deleteOne: async (filter) => col.deleteOneAsync ? col.deleteOneAsync(filter) : col.deleteOne(filter),
      deleteMany: async (filter) => col.deleteManyAsync ? col.deleteManyAsync(filter) : col.deleteMany(filter),
      count: async (filter) => col.count(filter ?? null),
      aggregate: async (pipeline) => col.aggregate(pipeline),
      createIndex: async (field) => col.createIndex(field),
      dropIndex: async (field) => col.dropIndex(field),
      createCompoundIndex: async (fields) => col.createCompoundIndex(fields),
      dropCompoundIndex: async (fields) => col.dropCompoundIndex(fields),
      createFtsIndex: async (field) => col.createFtsIndex(field),
      dropFtsIndex: async (field) => col.dropFtsIndex(field),
      createVectorIndex: async (field, options) => col.createVectorIndex(field, options.dimensions, options.metric ?? null, options.indexType ?? null, options.hnswM ?? null, options.hnswEfConstruction ?? null),
      dropVectorIndex: async (field) => col.dropVectorIndex(field),
      upgradeVectorIndex: async (field) => col.upgradeVectorIndex(field),
      listIndexes: async () => {
        const json = col.listIndexes();
        return JSON.parse(json);
      },
      findNearest: async (field, vector, topK, filter) => {
        const raw = await col.findNearest(field, vector, topK, filter ?? null);
        return raw;
      },
      searchText: async (field, query, topK, filter, options) => {
        return col.searchText(field, query, topK, filter ?? null, options ?? null);
      },
      hybridSearch: async (text, vector, topK, filter, options) => {
        return col.hybridSearch(text.textField, text.text, vector.vectorField, vector.vector, topK, filter ?? null, options ?? null);
      },
      subscribe: (filter, callback, onError) => makePoller(async () => col.find(filter ?? null), callback, onError),
      subscribeAggregate: (pipeline, callback, onError) => makePoller(async () => wrapped.aggregate(pipeline), callback, onError)
    };
    return decorateCollection(wrapped, name, opts, webhook);
  }
  const handle = {
    collection: (name, opts) => wrapCollection(name, opts),
    compact: async () => db.compact(),
    // Releases the native file handle/lock (no-op on older .node binaries).
    close: async () => db.close?.(),
    flush: db.flush ? async () => {
      db.flush();
    } : void 0,
    listCollectionNames: async () => db.listCollectionNames()
  };
  if (migrations?.length) {
    if (typeof db.userVersion !== "function" || typeof db.setUserVersion !== "function") {
      throw new Error("openDB({ migrations }) requires @taladb/node \u2265 0.9.2 \u2014 rebuild the native module");
    }
    await runMigrations(
      handle,
      async () => db.userVersion(),
      async (v) => db.setUserVersion(v),
      migrations
    );
  }
  return handle;
}
async function createNativeDB(_dbName, webhook, migrations) {
  const maybeNative = globalThis.__TalaDB__;
  if (!maybeNative) {
    throw new Error(
      "@taladb/react-native JSI HostObject not found. Did you call TalaDBModule.initialize() in your app entry point?"
    );
  }
  const native = maybeNative;
  function wrapCollection(name, opts) {
    const wrapped = {
      insert: async (doc) => native.insert(name, doc),
      insertMany: async (docs) => native.insertMany(name, docs),
      find: async (filter) => native.find(name, filter ?? {}),
      findOne: async (filter) => native.findOne(name, filter ?? {}),
      updateOne: async (filter, update) => native.updateOne(name, filter, update),
      updateMany: async (filter, update) => native.updateMany(name, filter, update),
      deleteOne: async (filter) => native.deleteOne(name, filter),
      deleteMany: async (filter) => native.deleteMany(name, filter),
      count: async (filter) => native.count(name, filter ?? {}),
      aggregate: async (pipeline) => native.aggregate(name, pipeline),
      createIndex: async (field) => native.createIndex(name, field),
      dropIndex: async (field) => native.dropIndex(name, field),
      createCompoundIndex: async (fields) => native.createCompoundIndex(name, fields),
      dropCompoundIndex: async (fields) => native.dropCompoundIndex(name, fields),
      createFtsIndex: async (field) => native.createFtsIndex(name, field),
      dropFtsIndex: async (field) => native.dropFtsIndex(name, field),
      createVectorIndex: async (field, options) => {
        const opts2 = {};
        if (options.metric) opts2.metric = options.metric;
        if (options.hnswM || options.hnswEfConstruction) {
          opts2.hnsw = { m: options.hnswM, efConstruction: options.hnswEfConstruction };
        }
        return native.createVectorIndex(name, field, options.dimensions, opts2);
      },
      dropVectorIndex: async (field) => native.dropVectorIndex(name, field),
      upgradeVectorIndex: async (field) => native.upgradeVectorIndex(name, field),
      // The JSI HostObject does not expose index introspection yet; return a
      // correctly-shaped empty result rather than `{}` cast to the interface.
      listIndexes: async () => ({ btree: [], fts: [], vector: [] }),
      findNearest: async (field, vector, topK, filter) => {
        const raw = native.findNearest(name, field, vector, topK, filter ?? null);
        return raw;
      },
      searchText: async (field, query, topK, filter, options) => {
        if (!native.searchText) {
          throw new Error("searchText requires @taladb/react-native \u2265 0.10 \u2014 rebuild the native module");
        }
        return native.searchText(name, field, query, topK, filter ?? null, options ?? null);
      },
      hybridSearch: async (text, vector, topK, filter, options) => {
        if (!native.hybridSearch) {
          throw new Error("hybridSearch requires @taladb/react-native \u2265 0.10 \u2014 rebuild the native module");
        }
        return native.hybridSearch(name, text.textField, text.text, vector.vectorField, vector.vector, topK, filter ?? null, options ?? null);
      },
      subscribe: (filter, callback, onError) => makePoller(async () => native.find(name, filter ?? {}), callback, onError),
      subscribeAggregate: (pipeline, callback, onError) => makePoller(async () => native.aggregate(name, pipeline), callback, onError)
    };
    return decorateCollection(wrapped, name, opts, webhook);
  }
  const handle = {
    collection: (name, opts) => wrapCollection(name, opts),
    compact: async () => native.compact(),
    close: async () => native.close(),
    flush: native.flush ? async () => {
      native.flush();
    } : void 0
  };
  if (migrations?.length) {
    if (typeof native.userVersion !== "function" || typeof native.setUserVersion !== "function") {
      throw new Error(
        "openDB({ migrations }) is not available on this @taladb/react-native binary yet (the JSI HostObject does not expose userVersion/setUserVersion). Update the native module."
      );
    }
    await runMigrations(
      handle,
      async () => native.userVersion(),
      async (v) => native.setUserVersion(v),
      migrations
    );
  }
  return handle;
}
async function runMigrations(db, getVersion, setVersion, migrations) {
  const sorted = [...migrations].sort((a, b) => a.version - b.version);
  for (let i = 0; i < sorted.length; i++) {
    const v = sorted[i].version;
    if (!Number.isInteger(v) || v < 1) {
      throw new Error(`TalaDB migration version must be a positive integer, got ${v}`);
    }
    if (i > 0 && v === sorted[i - 1].version) {
      throw new Error(`TalaDB duplicate migration version ${v}`);
    }
  }
  const current = await getVersion();
  for (const m of sorted) {
    if (m.version <= current) continue;
    await m.up(db);
    await setVersion(m.version);
  }
}
async function openDB(dbName = "taladb.db", options) {
  if (options?.passphrase !== void 0 && options.passphrase.length === 0) {
    throw new Error("TalaDB encryption passphrase must not be empty");
  }
  let resolvedConfig;
  if (options?.config !== void 0) {
    validateConfig(options.config);
    resolvedConfig = options.config;
  } else {
    resolvedConfig = await loadConfig(options?.configPath);
  }
  if (options?.durability) {
    resolvedConfig = {
      ...resolvedConfig,
      durability: { ...resolvedConfig?.durability, ...options.durability }
    };
  }
  const webhook = createWebhookDispatcher(options?.webhook ?? resolvedConfig?.webhook);
  const platform = detectPlatform();
  const migrations = options?.migrations;
  let db;
  switch (platform) {
    case "browser":
      db = await createBrowserDB(dbName, webhook, resolvedConfig, options?.passphrase, migrations);
      break;
    case "react-native":
      if (options?.passphrase !== void 0) {
        throw new Error("On React Native, pass the passphrase in the config JSON to TalaDBModule.initialize(); refusing to assume the already-open native database is encrypted");
      }
      db = await createNativeDB(dbName, webhook, migrations);
      break;
    case "node":
      db = await createNodeDB(dbName, webhook, resolvedConfig, options?.passphrase, migrations);
      break;
  }
  return webhook ? attachWebhook(db, webhook) : db;
}
function attachWebhook(db, webhook) {
  return {
    ...db,
    webhookStats: () => webhook.stats(),
    flushWebhook: (timeoutMs) => webhook.flush(timeoutMs),
    async close() {
      await webhook.flush();
      await db.close();
    }
  };
}
export {
  TalaDbValidationError,
  applySchema,
  createWebhookDispatcher,
  decorateCollection,
  deriveDocId,
  openDB,
  runMigrations,
  validateWebhookConfig
};
