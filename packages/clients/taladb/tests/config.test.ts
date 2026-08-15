import { describe, it, expect } from 'vitest';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { validateConfig, loadConfig } from '../src/config';

// ---------------------------------------------------------------------------
// validateConfig
// ---------------------------------------------------------------------------

describe('validateConfig', () => {
  it('accepts an empty config', () => {
    expect(() => validateConfig({})).not.toThrow();
  });

  it('accepts config with no webhook block', () => {
    expect(() => validateConfig({ webhook: undefined })).not.toThrow();
  });

  it('accepts a valid https endpoint', () => {
    expect(() =>
      validateConfig({ webhook: { endpoint: 'https://api.example.com/hook' } }),
    ).not.toThrow();
  });

  it('accepts a valid http endpoint (localhost)', () => {
    expect(() =>
      validateConfig({ webhook: { endpoint: 'http://localhost:4000/events' } }),
    ).not.toThrow();
  });

  it('rejects a non-http(s) endpoint', () => {
    expect(() =>
      validateConfig({ webhook: { endpoint: 'ftp://files.example.com' } }),
    ).toThrow(/invalid \w*_?endpoint/);
  });

  it('rejects a relative path as endpoint', () => {
    expect(() =>
      validateConfig({ webhook: { endpoint: '/relative/path' } }),
    ).toThrow(/invalid \w*_?endpoint/);
  });

  it('rejects a bare hostname with no scheme', () => {
    expect(() =>
      validateConfig({ webhook: { endpoint: 'api.example.com/hook' } }),
    ).toThrow(/invalid \w*_?endpoint/);
  });

  it('validates insert_endpoint', () => {
    expect(() =>
      validateConfig({ webhook: { insert_endpoint: 'not-a-url' } }),
    ).toThrow(/invalid \w*_?endpoint/);
  });

  it('validates update_endpoint', () => {
    expect(() =>
      validateConfig({ webhook: { update_endpoint: 'ws://wrong' } }),
    ).toThrow(/invalid \w*_?endpoint/);
  });

  it('validates delete_endpoint', () => {
    expect(() =>
      validateConfig({ webhook: { delete_endpoint: 'mailto:user@example.com' } }),
    ).toThrow(/invalid \w*_?endpoint/);
  });

  it('accepts valid per-event endpoints', () => {
    expect(() =>
      validateConfig({
        webhook: {
          insert_endpoint: 'https://api.example.com/insert',
          update_endpoint: 'https://api.example.com/update',
          delete_endpoint: 'http://localhost:3000/delete',
        },
      }),
    ).not.toThrow();
  });
});

// ---------------------------------------------------------------------------
// loadConfig
// ---------------------------------------------------------------------------

function tempDir(): string {
  return mkdtempSync(join(tmpdir(), 'taladb-config-test-'));
}

describe('loadConfig', () => {
  it('returns empty config when no file exists', async () => {
    const dir = tempDir();
    // Temporarily override process.cwd so auto-discovery finds nothing.
    const origCwd = process.cwd;
    process.cwd = () => dir;
    try {
      const cfg = await loadConfig();
      expect(cfg).toEqual({});
    } finally {
      process.cwd = origCwd;
    }
  });

  it('loads a valid JSON config by explicit path', async () => {
    const dir = tempDir();
    const filePath = join(dir, 'taladb.config.json');
    writeFileSync(
      filePath,
      JSON.stringify({
        webhook: {
          enabled: true,
          endpoint: 'https://api.example.com/events',
          headers: { Authorization: 'Bearer tok' },
        },
      }),
    );
    const cfg = await loadConfig(filePath);
    expect(cfg.webhook?.enabled).toBe(true);
    expect(cfg.webhook?.endpoint).toBe('https://api.example.com/events');
    expect(cfg.webhook?.headers?.['Authorization']).toBe('Bearer tok');
  });

  it('loads a valid YAML config by explicit path', async () => {
    const dir = tempDir();
    const filePath = join(dir, 'taladb.config.yml');
    writeFileSync(
      filePath,
      [
        'webhook:',
        '  enabled: true',
        '  endpoint: "https://hook.example.com"',
        '  headers:',
        '    X-Token: "secret"',
      ].join('\n'),
    );
    const cfg = await loadConfig(filePath);
    expect(cfg.webhook?.enabled).toBe(true);
    expect(cfg.webhook?.endpoint).toBe('https://hook.example.com');
    expect(cfg.webhook?.headers?.['X-Token']).toBe('secret');
  });

  it('auto-discovers taladb.config.yml from cwd', async () => {
    const dir = tempDir();
    writeFileSync(
      join(dir, 'taladb.config.yml'),
      'webhook:\n  enabled: false\n  endpoint: "https://auto.example.com"\n',
    );
    const origCwd = process.cwd;
    process.cwd = () => dir;
    try {
      const cfg = await loadConfig();
      expect(cfg.webhook?.endpoint).toBe('https://auto.example.com');
    } finally {
      process.cwd = origCwd;
    }
  });

  it('prefers .yml over .json when both exist', async () => {
    const dir = tempDir();
    writeFileSync(
      join(dir, 'taladb.config.yml'),
      'webhook:\n  endpoint: "https://yml.example.com"\n',
    );
    writeFileSync(
      join(dir, 'taladb.config.json'),
      JSON.stringify({ webhook: { endpoint: 'https://json.example.com' } }),
    );
    const origCwd = process.cwd;
    process.cwd = () => dir;
    try {
      const cfg = await loadConfig();
      expect(cfg.webhook?.endpoint).toBe('https://yml.example.com');
    } finally {
      process.cwd = origCwd;
    }
  });

  it('throws on invalid endpoint URL in config file', async () => {
    const dir = tempDir();
    const filePath = join(dir, 'taladb.config.json');
    writeFileSync(
      filePath,
      JSON.stringify({ webhook: { enabled: true, endpoint: 'not-a-url' } }),
    );
    await expect(loadConfig(filePath)).rejects.toThrow(/invalid \w*_?endpoint/);
  });

  it('throws on unsupported file extension', async () => {
    const dir = tempDir();
    const filePath = join(dir, 'taladb.config.toml');
    writeFileSync(filePath, '[sync]\nenabled = true\n');
    await expect(loadConfig(filePath)).rejects.toThrow('unsupported file extension');
  });

  it('ignores unknown keys in the config', async () => {
    const dir = tempDir();
    const filePath = join(dir, 'taladb.config.json');
    writeFileSync(
      filePath,
      JSON.stringify({ webhook: { enabled: false }, unknown_key: 'ignored' }),
    );
    await expect(loadConfig(filePath)).resolves.not.toThrow();
  });

  it('handles webhook disabled by default when key is absent', async () => {
    const dir = tempDir();
    const filePath = join(dir, 'taladb.config.json');
    writeFileSync(filePath, '{}');
    const cfg = await loadConfig(filePath);
    expect(cfg.webhook?.enabled).toBeFalsy();
  });
});
