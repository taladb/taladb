// ============================================================
// TalaDB config loader
//
// Parses and validates `taladb.config.yml` / `taladb.config.json`.
//
// The engine (Rust) reads only the `durability` block from this same file and
// ignores everything else; the `webhook` block is read here. One file, two
// readers, neither rejecting the other's keys.
// ============================================================

import { validateWebhookConfig, type WebhookConfig } from './webhook';

export type { WebhookConfig };

/** Storage durability settings. */
export interface DurabilityConfig {
  /**
   * When `true` (default), every write commit is fsync'd immediately — a crash
   * never loses an acknowledged write. When `false`, commits are batched for
   * higher write throughput; call `db.flush()` to force a durable sync. Applies
   * to Node (file) and browser OPFS storage; in-memory ignores it.
   */
  flush_every_write?: boolean;
  /**
   * Browser IndexedDB-fallback snapshot debounce, in milliseconds (default
   * 500). Only affects the non-OPFS browser fallback path — the OPFS and Node
   * paths use `flush_every_write`.
   */
  flush_ms?: number;
}

/** Top-level TalaDB configuration. */
export interface TalaDbConfig {
  /** Outbound change-webhook configuration. Disabled by default. */
  webhook?: WebhookConfig;
  /** Storage durability configuration. */
  durability?: DurabilityConfig;
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/**
 * Validate a parsed `TalaDbConfig`. Throws on the first invalid value.
 *
 * Endpoint checks live in `webhook.ts` alongside the code that uses them, so a
 * config built inline and passed to `openDB({ webhook })` — which never goes
 * through this loader — is validated by exactly the same rules.
 */
export function validateConfig(config: TalaDbConfig): void {
  if (config.webhook) validateWebhookConfig(config.webhook);
}

// ---------------------------------------------------------------------------
// Loader
// ---------------------------------------------------------------------------

/**
 * Load and validate a TalaDB config file.
 *
 * - Supports `.json`, `.yml`, and `.yaml` extensions.
 * - YAML parsing requires `js-yaml` (already in `taladb`'s dependencies).
 * - Only runs in Node.js. Returns `{}` silently on browser / React Native.
 * - Returns `{}` (webhook disabled) when no config file is found — **not an error**.
 *
 * @param configPath  Explicit path to the config file. If omitted, auto-discovers
 *                    `taladb.config.yml`, `taladb.config.yaml`, or
 *                    `taladb.config.json` from `process.cwd()`.
 */
export async function loadConfig(configPath?: string): Promise<TalaDbConfig> {
  // Non-Node platforms: no config file to read; pass `config` to openDB instead.
  if (typeof process === 'undefined' || typeof process.cwd !== 'function') {
    return {};
  }

  const { join, extname } = await import(/* @vite-ignore */ 'node:path');
  const { readFile, access } = await import(/* @vite-ignore */ 'node:fs/promises');

  async function parseFile(filePath: string): Promise<TalaDbConfig> {
    const content = await readFile(filePath, 'utf8');
    const ext = extname(filePath).toLowerCase();

    let raw: unknown;
    if (ext === '.json') {
      raw = JSON.parse(content);
    } else if (ext === '.yml' || ext === '.yaml') {
      // Dynamic import so the js-yaml parse cost is only paid when needed.
      const yaml = await import(/* @vite-ignore */ 'js-yaml');
      raw = yaml.load(content);
    } else {
      throw new Error(
        `TalaDB config: unsupported file extension "${ext}" — use .json, .yml, or .yaml`,
      );
    }

    const config = ((raw !== null && typeof raw === 'object' ? raw : {}) as TalaDbConfig);
    validateConfig(config);
    return config;
  }

  if (configPath) {
    return parseFile(configPath);
  }

  // Auto-discover from cwd.
  const cwd = process.cwd();
  for (const name of ['taladb.config.yml', 'taladb.config.yaml', 'taladb.config.json']) {
    const full = join(cwd, name);
    try {
      await access(full);
      return parseFile(full);
    } catch {
      // File doesn't exist — try the next candidate.
    }
  }

  // No config file found — the webhook is disabled, which is the default.
  return {};
}
