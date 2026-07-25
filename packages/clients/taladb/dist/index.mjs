// src/config.ts
var ENDPOINT_FIELDS = [
  "endpoint",
  "insert_endpoint",
  "update_endpoint",
  "delete_endpoint"
];
var LOCALHOST_HOSTS = /* @__PURE__ */ new Set(["localhost", "127.0.0.1", "[::1]", "::1"]);
function isLocalhostUrl(url) {
  try {
    return LOCALHOST_HOSTS.has(new URL(url).hostname);
  } catch {
    return false;
  }
}
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
    if (url?.startsWith("http://") && !isLocalhostUrl(url)) {
      console.warn(
        `[TalaDB] sync endpoint "${url}" uses plaintext HTTP \u2014 use HTTPS in production to prevent changeset interception`
      );
    }
  }
}
async function loadConfig(configPath) {
  if (typeof process === "undefined" || typeof process.cwd !== "function") {
    return {};
  }
  const { join, extname } = await import(
    /* @vite-ignore */
    "path"
  );
  const { readFile, access } = await import(
    /* @vite-ignore */
    "fs/promises"
  );
  async function parseFile(filePath) {
    const content = await readFile(filePath, "utf8");
    const ext = extname(filePath).toLowerCase();
    let raw;
    if (ext === ".json") {
      raw = JSON.parse(content);
    } else if (ext === ".yml" || ext === ".yaml") {
      const yaml = await import(
        /* @vite-ignore */
        "js-yaml"
      );
      raw = yaml.load(content);
    } else {
      throw new Error(
        `TalaDB config: unsupported file extension "${ext}" \u2014 use .json, .yml, or .yaml`
      );
    }
    const config = raw !== null && typeof raw === "object" ? raw : {};
    validateConfig(config);
    return config;
  }
  if (configPath) {
    return parseFile(configPath);
  }
  const cwd = process.cwd();
  for (const name of ["taladb.config.yml", "taladb.config.yaml", "taladb.config.json"]) {
    const full = join(cwd, name);
    try {
      await access(full);
      return parseFile(full);
    } catch {
    }
  }
  return {};
}

// src/sync.ts
var CURSOR_COLLECTION = "__taladb_sync";
async function resolveCollections(handle, options) {
  const base = options.collections ?? await handle.listCollectionNames();
  const excluded = new Set(options.exclude ?? []);
  return base.filter((c) => !excluded.has(c) && !c.startsWith("_"));
}
function unsupportedSync(runtime) {
  const err = () => new Error(
    `TalaDB sync is not yet available on the ${runtime} runtime (Node.js is supported today; browser and React Native are in progress). Track it on the roadmap.`
  );
  return {
    sync: () => Promise.reject(err()),
    exportChanges: () => Promise.reject(err()),
    importChanges: () => Promise.reject(err())
  };
}
async function readCursor(cursorCol, target) {
  const doc = await cursorCol.findOne({ target });
  return {
    pushMs: doc?.pushMs ?? 0,
    pullMs: doc?.pullMs ?? 0,
    pullCursor: doc?.pullCursor
  };
}
async function writeCursor(cursorCol, target, cursor) {
  const updated = await cursorCol.updateOne({ target }, { $set: { ...cursor } });
  if (!updated) {
    await cursorCol.insert({ target, ...cursor });
  }
}
function isCursorAdapter(adapter) {
  return typeof adapter.pullWithCursor === "function";
}
var MAX_PULL_PAGES = 1e4;
async function runSync(handle, adapter, options, syncSchemas = {}) {
  const direction = options.direction ?? "both";
  const target = options.target ?? "default";
  const doPush = direction === "push" || direction === "both";
  const doPull = direction === "pull" || direction === "both";
  if (doPull && !adapter.pull && !isCursorAdapter(adapter)) {
    throw new Error(
      `sync direction '${direction}' requires adapter.pull() or adapter.pullWithCursor()`
    );
  }
  if (doPush && !adapter.push) {
    throw new Error(`sync direction '${direction}' requires adapter.push()`);
  }
  const collections = await resolveCollections(handle, options);
  const cursorCol = handle.collection(CURSOR_COLLECTION);
  const cursor = await readCursor(cursorCol, target);
  const local = doPush ? await handle.exportChanges(collections, 0) : "[]";
  const scopedSchemas = {};
  for (const c of collections) {
    if (syncSchemas[c]) scopedSchemas[c] = syncSchemas[c];
  }
  const useValidated = handle.importChangesValidated && Object.keys(scopedSchemas).length > 0;
  let pulled = 0;
  let skipped = 0;
  let quarantined = 0;
  let pullCursor = cursor.pullCursor;
  async function importOne(changeset) {
    if (!changeset || changeset === "[]") return;
    if (useValidated) {
      const report = await handle.importChangesValidated(changeset, JSON.stringify(scopedSchemas));
      pulled += report.applied;
      skipped += report.skipped;
      quarantined += report.quarantined;
    } else {
      pulled += await handle.importChanges(changeset);
    }
  }
  if (doPull) {
    if (isCursorAdapter(adapter)) {
      let pages = 0;
      for (; ; ) {
        const result = await adapter.pullWithCursor(pullCursor ?? null);
        await importOne(result.changeset);
        pullCursor = result.cursor;
        await writeCursor(cursorCol, target, { ...cursor, pullCursor });
        if (!result.hasMore) break;
        if (++pages >= MAX_PULL_PAGES) {
          throw new Error(
            `sync: origin returned hasMore after ${MAX_PULL_PAGES} pages for target '${target}' \u2014 it is probably not advancing its cursor.`
          );
        }
      }
    } else {
      await importOne(await adapter.pull(0));
    }
  }
  let pushed = 0;
  if (doPush && local !== "[]") {
    pushed = JSON.parse(local).length;
    await adapter.push(local);
  }
  await writeCursor(cursorCol, target, {
    pushMs: cursor.pushMs,
    pullMs: cursor.pullMs,
    ...pullCursor !== void 0 ? { pullCursor } : {}
  });
  return { pushed, pulled, skipped, quarantined, cursor: 0 };
}

// src/http-adapter.ts
var HttpSyncAdapter = class {
  constructor(options) {
    this.endpoint = options.endpoint.replace(/\/$/, "");
    this.headers = options.headers ?? {};
    const f = options.fetch ?? globalThis.fetch?.bind(globalThis);
    if (!f) {
      throw new Error(
        "HttpSyncAdapter: no fetch available. Pass options.fetch on runtimes without a global fetch."
      );
    }
    this.fetchFn = f;
    this.pushPath = options.paths?.push ?? "/push";
    this.pullPath = options.paths?.pull ?? "/pull";
  }
  async push(changeset) {
    const res = await this.fetchFn(`${this.endpoint}${this.pushPath}`, {
      method: "POST",
      headers: { "content-type": "application/json", ...this.headers },
      body: changeset
    });
    if (!res.ok) {
      throw new Error(`HttpSyncAdapter push failed: ${res.status} ${res.statusText}`);
    }
  }
  async pull(sinceMs) {
    const url = `${this.endpoint}${this.pullPath}?since=${encodeURIComponent(String(sinceMs))}`;
    const res = await this.fetchFn(url, { method: "GET", headers: this.headers });
    if (!res.ok) {
      throw new Error(`HttpSyncAdapter pull failed: ${res.status} ${res.statusText}`);
    }
    const body = (await res.text()).trim();
    return body.length === 0 ? "[]" : body;
  }
};

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

// src/replication/coverage.ts
var COVERAGE_COLLECTION = "__taladb_replica";
function coverageKey(key) {
  return [
    key.origin,
    key.collection,
    key.scope,
    `p${key.projectionVersion}`,
    `s${key.schemaVersion}`
  ].map(encodeURIComponent).join("|");
}
var CoverageStore = class {
  constructor(db) {
    this.col = db.collection(COVERAGE_COLLECTION);
  }
  async read(key) {
    const doc = await this.col.findOne({ key: coverageKey(key) });
    if (!doc?.state) return { status: "empty" };
    try {
      return JSON.parse(doc.state);
    } catch {
      return { status: "empty" };
    }
  }
  async write(key, state) {
    const k = coverageKey(key);
    const state_json = JSON.stringify(state);
    await this.col.replaceManyWithIds(
      [{ _id: deriveDocId(COVERAGE_COLLECTION, k), key: k, state: state_json }],
      "local"
    );
  }
  /** Drop a scope's coverage, forcing a fresh bootstrap on next use. */
  async clear(key) {
    const k = coverageKey(key);
    await this.col.deleteManyWithIds([deriveDocId(COVERAGE_COLLECTION, k)], "local");
  }
};
function isAuthoritative(state) {
  return state.status === "complete";
}
function rowsApplied(state) {
  switch (state.status) {
    case "hydrating":
    case "complete":
    case "best-effort":
      return state.rowsApplied;
    default:
      return 0;
  }
}
function progress(state) {
  if (state.status === "complete") return 1;
  if (state.status !== "hydrating" || !state.total) return void 0;
  return Math.min(1, state.rowsApplied / state.total);
}

// src/replication/coordinator.ts
var DEFAULT_PAGE_SIZE = 500;
var defaultYield = () => new Promise((resolve) => setTimeout(resolve, 0));
var MAX_BOOTSTRAP_PAGES = 1e5;
var inflightByDatabase = /* @__PURE__ */ new WeakMap();
var REPLICA_SCOPE_FIELD = "_replica_scope";
var REPLICA_REVISION_FIELD = "_remote_rev";
var ReplicationCoordinator = class {
  constructor(db, source, options = {}) {
    this.db = db;
    this.source = source;
    this.coverage = new CoverageStore(db);
    this.key = {
      origin: source.origin,
      collection: source.collection,
      scope: source.scope,
      projectionVersion: source.projectionVersion,
      schemaVersion: source.schemaVersion
    };
    this.pageSize = options.pageSize ?? DEFAULT_PAGE_SIZE;
    this.yieldFn = options.yieldFn ?? defaultYield;
    this.onProgress = options.onProgress;
    this.collectionOptions = options.collectionOptions;
    let shared = inflightByDatabase.get(db);
    if (!shared) {
      shared = /* @__PURE__ */ new Map();
      inflightByDatabase.set(db, shared);
    }
    this.inflight = shared;
  }
  get replicaScope() {
    return coverageKey(this.key);
  }
  get identityNamespace() {
    return `${this.source.origin}\0${this.source.scope}\0${this.source.collection}`;
  }
  getCoverage() {
    return this.coverage.read(this.key);
  }
  /** Whether a purely local read is authorized right now. */
  async isReady() {
    return isAuthoritative(await this.getCoverage());
  }
  /** Dedup by intent: identical concurrent work joins rather than duplicating. */
  dedup(key, run) {
    const existing = this.inflight.get(key);
    if (existing) return existing;
    const pass = run().finally(() => this.inflight.delete(key));
    this.inflight.set(key, pass);
    return pass;
  }
  /**
   * Write a batch of remote rows into the local collection.
   *
   * One commit for the whole batch, ids derived from the origin's primary key, and
   * `origin: 'remote'` so the rows can never replicate back out at the origin they
   * came from. This is the *only* write path in the coordinator — bootstrap, delta
   * and bridge all funnel through it, which is precisely why they converge instead
   * of conflicting.
   */
  async applyRows(rows) {
    if (rows.length === 0) return [];
    const col = this.db.collection(this.source.collection, this.collectionOptions);
    const docs = rows.map(
      (row) => {
        const revision = this.source.revisionOf(row);
        return {
          ...this.source.mapRow(row),
          _id: deriveDocId(this.identityNamespace, String(this.source.keyOf(row))),
          [REPLICA_SCOPE_FIELD]: this.replicaScope,
          [REPLICA_REVISION_FIELD]: revision
        };
      }
    );
    await col.replaceManyWithIds(docs, "remote");
    return docs.map((doc) => doc._id);
  }
  /**
   * Hydrate the scope: walk the origin page by page until the whole collection is
   * local, then mark it complete.
   *
   * Resumable and idempotent. If the walk is interrupted — a reload, a crash, a
   * dead network — the next call picks up from the last committed page, and
   * re-applying a page it already wrote is a no-op because the ids are derived.
   */
  hydrate() {
    return this.dedup(`${this.replicaScope}:hydrate`, () => this.runHydrate());
  }
  async runHydrate() {
    let state = await this.coverage.read(this.key);
    if (state.status === "complete") return state;
    let page = null;
    let snapshot = null;
    let rowsApplied2 = 0;
    let total;
    let deltaCursor;
    if (state.status === "hydrating") {
      page = state.nextPage;
      snapshot = state.snapshot;
      rowsApplied2 = state.rowsApplied;
      total = state.total;
      deltaCursor = state.deltaCursor;
    } else if (state.status === "error" && state.snapshot) {
      page = state.resumeFrom;
      snapshot = state.snapshot;
      rowsApplied2 = state.rowsApplied ?? 0;
      total = state.total;
      deltaCursor = state.deltaCursor;
    }
    let snapshotSupported = true;
    let pages = 0;
    try {
      for (; ; ) {
        const result = await this.source.bootstrap({ page, snapshot, limit: this.pageSize });
        if (snapshot !== null && result.snapshot !== void 0 && result.snapshot !== snapshot) {
          throw new Error(
            `replication: origin '${this.source.origin}' changed snapshot token mid-walk`
          );
        }
        if (snapshot === null && result.snapshot) snapshot = result.snapshot;
        if (result.snapshot === void 0 && page === null) snapshotSupported = false;
        if (result.deltaCursor && !deltaCursor) deltaCursor = result.deltaCursor;
        if (result.total !== void 0) total = result.total;
        rowsApplied2 += (await this.applyRows(result.rows)).length;
        page = result.nextPage;
        if (page !== null) {
          const next = {
            status: "hydrating",
            snapshot: snapshot ?? "",
            nextPage: page,
            rowsApplied: rowsApplied2,
            ...deltaCursor !== void 0 ? { deltaCursor } : {},
            ...total !== void 0 ? { total } : {}
          };
          await this.coverage.write(this.key, next);
          this.onProgress?.(next);
          if (++pages >= MAX_BOOTSTRAP_PAGES) {
            throw new Error(
              `replication: origin '${this.source.origin}' offered more than ${MAX_BOOTSTRAP_PAGES} bootstrap pages for '${this.source.collection}' \u2014 it is probably not advancing nextPage.`
            );
          }
          await this.yieldFn();
          continue;
        }
        if (snapshotSupported && this.source.delta && deltaCursor === void 0) {
          throw new Error(
            `replication: origin '${this.source.origin}' supports delta refresh but did not issue deltaCursor on the first bootstrap page`
          );
        }
        state = snapshotSupported ? {
          status: "complete",
          cursor: deltaCursor ?? "",
          completedAt: Date.now(),
          rowsApplied: rowsApplied2,
          ...total !== void 0 ? { total } : {}
        } : {
          // Every row the origin offered was applied — but without a snapshot
          // we cannot prove we saw a consistent view of it, so we must not
          // claim completeness. Reads keep going to the network.
          status: "best-effort",
          cursor: deltaCursor ?? "",
          reason: "the origin did not return a snapshot token, so a row that moved between pages during the walk may have been missed",
          rowsApplied: rowsApplied2,
          ...total !== void 0 ? { total } : {}
        };
        await this.coverage.write(this.key, state);
        this.onProgress?.(state);
        return state;
      }
    } catch (error) {
      const failed = {
        status: "error",
        resumeFrom: page ?? 0,
        ...snapshot ? { snapshot } : {},
        ...deltaCursor !== void 0 ? { deltaCursor } : {},
        rowsApplied: rowsApplied2,
        ...total !== void 0 ? { total } : {},
        error: error instanceof Error ? error.message : String(error)
      };
      await this.coverage.write(this.key, failed);
      this.onProgress?.(failed);
      throw error;
    }
  }
  /**
   * Apply incremental changes since the stored cursor.
   *
   * Deletions are applied by mapping the origin's primary keys through the same
   * `deriveDocId`, and are written with `origin: 'remote'` so they leave no
   * tombstone — the origin already knows it deleted these, and a tombstone would
   * push its own deletion back at it.
   */
  refresh() {
    return this.dedup(`${this.replicaScope}:refresh`, () => this.runRefresh());
  }
  async runRefresh() {
    const state = await this.coverage.read(this.key);
    if (state.status !== "complete") return state;
    if (!this.source.delta) return state;
    const col = this.db.collection(this.source.collection);
    let cursor = state.cursor;
    let rowsApplied2 = state.rowsApplied;
    for (; ; ) {
      const page = await this.source.delta(cursor);
      rowsApplied2 += (await this.applyRows(page.changed)).length;
      if (page.deleted.length > 0) {
        const ids = page.deleted.map(
          (k) => deriveDocId(this.identityNamespace, String(k))
        );
        await col.deleteManyWithIds(ids, "remote");
      }
      cursor = page.cursor;
      const next = { ...state, cursor, rowsApplied: rowsApplied2 };
      await this.coverage.write(this.key, next);
      if (!page.hasMore) {
        this.onProgress?.(next);
        return next;
      }
      await this.yieldFn();
    }
  }
  /**
   * Cold-start bridge: fetch exactly the rows one query needs, right now.
   *
   * Needed because a SPA or React Native app has no server render to paint behind
   * while the replica fills. The rows land in the same collection under the same
   * derived ids as the walk's, so this is not a cache — it is the replica, arriving
   * early.
   *
   * **Does not advance coverage.** These rows did not come from the bootstrap
   * snapshot and prove nothing about completeness; treating them as progress would
   * let a page-1 fetch masquerade as a hydrated catalog.
   */
  bridge(query) {
    if (!this.source.fetchQuery) return Promise.resolve({ count: 0, ids: [] });
    const key = `bridge:${coverageKey(this.key)}:${JSON.stringify(query)}`;
    return this.dedup(key, async () => {
      const rows = await this.source.fetchQuery(query);
      const ids = await this.applyRows(rows);
      return { count: ids.length, ids };
    });
  }
  /** Drop coverage and force a fresh bootstrap. Local rows are left alone. */
  async reset() {
    await this.coverage.clear(this.key);
  }
};

// src/replication/rest.ts
function parseRows(body, endpoint) {
  if (Array.isArray(body)) return body;
  if (body && typeof body === "object") {
    const env = body;
    for (const field of ["data", "items", "rows"]) {
      const value = env[field];
      if (Array.isArray(value)) return value;
    }
    throw new Error(
      `taladb: could not find a row array in the response from ${endpoint}. Expected a bare array or a { data | items | rows } envelope, but got an object with keys: ${Object.keys(env).join(", ") || "(none)"}. Pass { parse } to extract them yourself.`
    );
  }
  throw new Error(
    `taladb: expected an array or object from ${endpoint}, got ${typeof body}.`
  );
}
function pick(body, names) {
  if (!body || typeof body !== "object") return void 0;
  const rec = body;
  for (const n of names) {
    if (rec[n] !== void 0) return rec[n];
    const meta = rec.meta;
    if (meta && meta[n] !== void 0) return meta[n];
  }
  return void 0;
}
function createRestSource(options) {
  const {
    endpoint,
    collection,
    origin = endpoint,
    scope = "global",
    projectionVersion = 1,
    schemaVersion = 1,
    key = "id",
    revision = "rev",
    mapRow,
    getAuth,
    paths,
    toParams,
    parse,
    pagination = "page"
  } = options;
  const doFetch = options.fetch ?? globalThis.fetch;
  async function get(path, params) {
    const url = new URL(path, globalThis.location?.origin ?? "http://localhost");
    for (const [k, v] of Object.entries(params)) url.searchParams.set(k, v);
    const headers = getAuth ? await getAuth() : void 0;
    const response = await doFetch(url.href, { headers });
    if (!response.ok) {
      throw new Error(
        `taladb: ${path} responded ${response.status} ${response.statusText}`
      );
    }
    return response.json();
  }
  const rowsFrom = (body) => parse ? parse(body) : parseRows(body, endpoint);
  return {
    origin,
    collection,
    scope,
    projectionVersion,
    schemaVersion,
    keyOf: (row) => {
      const value = row[key];
      if (value === void 0 || value === null) {
        throw new Error(
          `taladb: row from ${endpoint} has no '${key}' field to use as its primary key. Pass { key } to name the right one. Without a stable key, repeated fetches of the same row cannot be recognized as the same row.`
        );
      }
      return String(value);
    },
    revisionOf: (row) => {
      const value = typeof revision === "function" ? revision(row) : row[revision];
      if (value === void 0 || value === null) {
        throw new Error(
          `taladb: row from ${endpoint} has no authoritative revision. Pass { revision } to name the monotonic revision field.`
        );
      }
      const n = Number(value);
      if (!Number.isSafeInteger(n)) {
        throw new Error(`taladb: authoritative revision must be a safe integer, got ${String(value)}`);
      }
      return n;
    },
    mapRow: (row) => {
      if (mapRow) return mapRow(row);
      const { _id, ...rest } = row;
      void _id;
      return rest;
    },
    bootstrap: async (request) => {
      const params = { limit: String(request.limit) };
      if (request.page !== null) params.page = String(request.page);
      if (request.snapshot !== null) params.snapshot = request.snapshot;
      const body = await get(endpoint + (paths?.bootstrap ?? ""), params);
      const rows = rowsFrom(body);
      const nextPage = pick(body, ["nextPage", "next_page", "next"]);
      const snapshot = pick(body, ["snapshot"]);
      const deltaCursor = pick(body, ["deltaCursor", "delta_cursor", "cursor"]);
      const total = pick(body, ["total", "totalCount", "count"]);
      return {
        rows,
        // An origin that reports no explicit `nextPage` is treated as exhausted
        // once it returns a short page — the conventional REST behavior.
        nextPage: nextPage !== void 0 ? nextPage : rows.length < request.limit ? null : pagination === "offset" ? Number(request.page ?? 0) + request.limit : Number(request.page ?? 1) + 1,
        ...snapshot !== void 0 ? { snapshot } : {},
        ...deltaCursor !== void 0 ? { deltaCursor } : {},
        ...total !== void 0 ? { total } : {}
      };
    },
    ...options.delta === true || paths?.delta ? { delta: async (cursor) => {
      const body = await get(endpoint + (paths?.delta ?? ""), { since: cursor });
      const changed = rowsFrom(body);
      const deleted = pick(body, ["deleted", "removed"]) ?? [];
      const next = pick(body, ["cursor", "now", "nextCursor"]);
      return {
        changed,
        deleted: deleted.map(String),
        cursor: next ?? cursor,
        hasMore: Boolean(pick(body, ["hasMore", "has_more"]))
      };
    } } : {},
    fetchQuery: async (query) => {
      if (!toParams && Object.values(query.filter ?? {}).some((v) => typeof v === "object" && v !== null)) {
        throw new Error(
          "taladb: bridge filters with operators require RestSourceOptions.toParams; the default translator only supports scalar equality fields."
        );
      }
      const sortEntry = Object.entries(query.sort ?? {})[0];
      const params = toParams ? toParams(query) : {
        ...query.page !== void 0 ? { page: String(query.page) } : {},
        ...query.limit !== void 0 ? { limit: String(query.limit) } : {},
        ...Object.fromEntries(
          Object.entries(query.filter ?? {}).map(([k, v]) => [k, String(v)])
        ),
        ...sortEntry ? { sort: sortEntry[0], order: sortEntry[1] === -1 ? "desc" : "asc" } : {}
      };
      return rowsFrom(await get(endpoint, params));
    }
  };
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
    "_changed_at",
    "_remote",
    "_remote_rev",
    "_replica_scope"
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
  function stampDoc(doc) {
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
    // Rows arriving from a remote origin are validated like any other write. This
    // is the "parse, don't assert" boundary: the compile-time generic and the
    // runtime schema check have to be the same seam, or a malformed server
    // response walks straight into a typed collection.
    replaceManyWithIds: wrapWrites ? async (docs, origin) => {
      if (origin !== "remote") {
        docs.forEach((doc, i) => assertWritableDocument(doc, `replaceManyWithIds[${i}]`));
        if (targetVersion > 0) {
          throw new Error(
            "local replaceManyWithIds is disabled on versioned collections; use updateOne/updateMany so schema-version guards are atomic"
          );
        }
      }
      if (schema) docs.forEach((doc, i) => {
        const { _replica_scope, _remote_rev, ...schemaDoc } = doc;
        void _replica_scope;
        void _remote_rev;
        parseWrite(schemaDoc, `replaceManyWithIds[${i}]`);
      });
      return col.replaceManyWithIds(docs.map((d) => stampDoc(d)), origin);
    } : col.replaceManyWithIds.bind(col),
    deleteManyWithIds: stampVersion ? async (ids, origin) => {
      if (origin !== "remote") {
        throw new Error(
          "local deleteManyWithIds is disabled on versioned collections; use deleteOne/deleteMany so schema-version guards are atomic"
        );
      }
      return col.deleteManyWithIds(ids, origin);
    } : col.deleteManyWithIds.bind(col),
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
async function createBrowserDB(dbName, config, passphrase, migrations) {
  const workerUrl = new URL("@taladb/web/worker/taladb.worker.js", import.meta.url);
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
  const syncSchemas = {};
  function wrapCollection(name, opts) {
    if (opts?.syncSchema) syncSchemas[name] = opts.syncSchema;
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
      replaceManyWithIds: async (docs, origin = "local") => {
        const json = await proxy.send("replaceManyWithIds", {
          collection: name,
          docsJson: s(docs),
          origin
        });
        return JSON.parse(json);
      },
      deleteManyWithIds: (ids, origin = "local") => proxy.send("deleteManyWithIds", {
        collection: name,
        idsJson: s(ids),
        origin
      }),
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
        () => proxy.send("find", {
          collection: name,
          filterJson: filter ? s(filter) : "null"
        }),
        callback,
        onError
      ),
      subscribeAggregate: (pipeline, callback, onError) => nudgedPoller(
        () => proxy.send("aggregate", {
          collection: name,
          pipelineJson: s(pipeline)
        }),
        callback,
        onError
      )
    };
    return opts ? applySchema(wrapped, opts) : wrapped;
  }
  function nudgedPoller(fetchJson, callback, onError) {
    let active = true;
    let lastJson = "";
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
        const json = await fetchJson();
        if (!active) return;
        if (json !== lastJson) {
          lastJson = json;
          callback(JSON.parse(json));
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
    syncStatus: async () => JSON.parse(await proxy.send("syncStatus")),
    flushSync: (timeoutMs = 5e3) => proxy.send("flushSync", { timeoutMs }),
    close: async () => {
      channel?.close();
      try {
        await proxy.send("close");
      } finally {
        worker.terminate();
        proxy.abort(new Error("taladb worker closed"));
      }
    },
    // All engine work (export scan, LWW merge) runs inside the worker, off the
    // main thread — a sync pass never blocks rendering, whatever its size.
    exportChanges: (collections, sinceMs) => proxy.send("exportChangeset", { collectionsJson: JSON.stringify(collections), sinceMs }),
    importChanges: (changeset) => proxy.send("importChangeset", { changesetJson: changeset }),
    importChangesValidated: async (changeset, schemasJson) => JSON.parse(await proxy.send("importChangesetValidated", { changesetJson: changeset, schemasJson })),
    listCollectionNames: async () => JSON.parse(await proxy.send("listCollections")),
    quarantined: async (collection) => JSON.parse(await proxy.send("quarantined", { collection })),
    sync: (adapter, options) => runSync(handle, adapter, options, syncSchemas)
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
async function createNodeDB(dbName, config, passphrase, migrations) {
  const native = await import("@taladb/node");
  const TalaDBNode = native.TalaDbNode ?? native.TalaDBNode;
  if (!TalaDBNode) throw new Error("@taladb/node loaded but exports no TalaDbNode class \u2014 rebuild the native module");
  const configJson = config !== void 0 ? JSON.stringify(config) : null;
  const db = TalaDBNode.open(dbName, configJson, passphrase ?? null);
  const syncSchemas = {};
  function wrapCollection(name, opts) {
    if (opts?.syncSchema) syncSchemas[name] = opts.syncSchema;
    const col = db.collection(name);
    const wrapped = {
      insert: async (doc) => col.insertAsync ? col.insertAsync(doc) : col.insert(doc),
      insertMany: async (docs) => col.insertManyAsync ? col.insertManyAsync(docs) : col.insertMany(docs),
      replaceManyWithIds: async (docs, origin = "local") => col.replaceManyWithIdsAsync ? col.replaceManyWithIdsAsync(docs, origin) : col.replaceManyWithIds(docs, origin),
      deleteManyWithIds: async (ids, origin = "local") => col.deleteManyWithIdsAsync ? col.deleteManyWithIdsAsync(ids, origin) : col.deleteManyWithIds(ids, origin),
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
    return opts ? applySchema(wrapped, opts) : wrapped;
  }
  const handle = {
    collection: (name, opts) => wrapCollection(name, opts),
    compact: async () => db.compact(),
    // Releases the native file handle/lock (no-op on older .node binaries).
    close: async () => db.close?.(),
    flush: db.flush ? async () => {
      db.flush();
    } : void 0,
    exportChanges: async (collections, sinceMs) => db.exportChanges(sinceMs, collections),
    importChanges: async (changeset) => db.importChanges(changeset),
    // Feature-detected: only present when the loaded .node binary supports it,
    // so older prebuilt binaries fall back to plain importChanges.
    importChangesValidated: db.importChangesValidated ? async (changeset, schemasJson) => db.importChangesValidated(changeset, schemasJson) : void 0,
    listCollectionNames: async () => db.listCollectionNames(),
    quarantined: async (collection) => db.quarantined ? db.quarantined(collection) : [],
    sync: (adapter, options) => runSync(handle, adapter, options, syncSchemas)
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
async function createNativeDB(_dbName, migrations) {
  const maybeNative = globalThis.__TalaDB__;
  if (!maybeNative) {
    throw new Error(
      "@taladb/react-native JSI HostObject not found. Did you call TalaDBModule.initialize() in your app entry point?"
    );
  }
  const native = maybeNative;
  const syncSchemas = {};
  function wrapCollection(name, opts) {
    if (opts?.syncSchema) syncSchemas[name] = opts.syncSchema;
    const wrapped = {
      insert: async (doc) => native.insert(name, doc),
      insertMany: async (docs) => native.insertMany(name, docs),
      replaceManyWithIds: async (docs, origin = "local") => native.replaceManyWithIds(name, docs, origin),
      deleteManyWithIds: async (ids, origin = "local") => native.deleteManyWithIds(name, ids, origin),
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
    return opts ? applySchema(wrapped, opts) : wrapped;
  }
  const syncSurface = typeof native.exportChanges === "function" && typeof native.importChanges === "function" && typeof native.listCollectionNames === "function" ? (() => {
    const handle2 = {
      collection: (name, opts) => wrapCollection(name, opts),
      exportChanges: async (collections, sinceMs) => native.exportChanges(collections, sinceMs),
      importChanges: async (changeset) => native.importChanges(changeset),
      // Feature-detected: present on 0.9.2+ JSI HostObjects; when absent,
      // runSync falls back to unvalidated importChanges.
      importChangesValidated: native.importChangesValidated ? async (changeset, schemasJson) => native.importChangesValidated(changeset, schemasJson) : void 0,
      listCollectionNames: async () => native.listCollectionNames(),
      sync: (adapter, options) => runSync(handle2, adapter, options, syncSchemas)
    };
    return {
      exportChanges: handle2.exportChanges,
      importChanges: handle2.importChanges,
      sync: handle2.sync
    };
  })() : unsupportedSync("react-native");
  const handle = {
    collection: (name, opts) => wrapCollection(name, opts),
    compact: async () => native.compact(),
    close: async () => native.close(),
    flush: native.flush ? async () => {
      native.flush();
    } : void 0,
    quarantined: native.quarantined ? async (collection) => native.quarantined(collection) : void 0,
    ...syncSurface
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
  const platform = detectPlatform();
  const migrations = options?.migrations;
  switch (platform) {
    case "browser":
      return createBrowserDB(dbName, resolvedConfig, options?.passphrase, migrations);
    case "react-native":
      if (options?.passphrase !== void 0) {
        throw new Error("On React Native, pass the passphrase in the config JSON to TalaDBModule.initialize(); refusing to assume the already-open native database is encrypted");
      }
      return createNativeDB(dbName, migrations);
    case "node":
      return createNodeDB(dbName, resolvedConfig, options?.passphrase, migrations);
  }
}
export {
  COVERAGE_COLLECTION,
  CoverageStore,
  HttpSyncAdapter,
  REPLICA_REVISION_FIELD,
  REPLICA_SCOPE_FIELD,
  ReplicationCoordinator,
  TalaDbValidationError,
  applySchema,
  coverageKey,
  createRestSource,
  deriveDocId,
  isAuthoritative,
  openDB,
  progress,
  rowsApplied,
  runMigrations
};
