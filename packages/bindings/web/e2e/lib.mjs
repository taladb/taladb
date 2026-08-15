// Shared harness plumbing: browser launch, page helpers, assertion runner.
import puppeteer from 'puppeteer-core';
import { spawn } from 'node:child_process';
import { rm, mkdtemp, readdir, access } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';

/**
 * Set once the server reports the port it actually bound. `let`, not `const`,
 * because the port is chosen by the OS: phase modules are imported *after*
 * `startServer` resolves, so they read the settled value.
 */
export let ORIGIN = '';
/**
 * Locate a Chrome to drive.
 *
 * `puppeteer-core`, not `puppeteer`: the full package downloads a browser from a
 * postinstall script, and this workspace allows install scripts only for the
 * packages named in `onlyBuiltDependencies`. Adding a network-fetching script to
 * that list to run a test suite is not a trade this repository makes, so the
 * browser is found rather than installed.
 *
 * Order: an explicit `CHROME_PATH`, then anything `@puppeteer/browsers` has
 * already put in the cache, then a system install.
 */
async function findChrome() {
  const exists = async (p) => {
    try {
      await access(p);
      return true;
    } catch {
      return false;
    }
  };

  if (process.env.CHROME_PATH) return process.env.CHROME_PATH;

  const cache = join(homedir(), '.cache', 'puppeteer');
  for (const flavour of ['chrome', 'chrome-headless-shell']) {
    let builds = [];
    try {
      builds = (await readdir(join(cache, flavour))).sort().reverse();
    } catch {
      continue;
    }
    for (const build of builds) {
      const dir = join(cache, flavour, build);
      const candidates =
        flavour === 'chrome'
          ? [
              join(dir, 'chrome-mac-x64', 'Google Chrome for Testing.app', 'Contents', 'MacOS', 'Google Chrome for Testing'),
              join(dir, 'chrome-mac-arm64', 'Google Chrome for Testing.app', 'Contents', 'MacOS', 'Google Chrome for Testing'),
              join(dir, 'chrome-linux64', 'chrome'),
              join(dir, 'chrome-win64', 'chrome.exe'),
            ]
          : [
              join(dir, 'chrome-headless-shell-mac-x64', 'chrome-headless-shell'),
              join(dir, 'chrome-headless-shell-mac-arm64', 'chrome-headless-shell'),
              join(dir, 'chrome-headless-shell-linux64', 'chrome-headless-shell'),
            ];
      for (const candidate of candidates) {
        if (await exists(candidate)) return candidate;
      }
    }
  }

  const system = [
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
    '/usr/bin/google-chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
  ];
  for (const candidate of system) {
    if (await exists(candidate)) return candidate;
  }

  throw new Error(
    'No Chrome found. Install one with `npx @puppeteer/browsers install chrome@stable`, ' +
      'or point CHROME_PATH at an existing binary.',
  );
}

export async function startServer() {
  const proc = spawn(process.execPath, [new URL('./server.mjs', import.meta.url).pathname], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  proc.stderr.on('data', (d) => process.stderr.write(`[server] ${d}`));
  await new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error('server did not start')), 5000);
    proc.stdout.on('data', (d) => {
      const match = /e2e server on (http:\/\/\S+)/.exec(String(d));
      if (match) {
        ORIGIN = match[1];
        clearTimeout(t);
        resolve();
      }
    });
  });
  return proc;
}

export async function launch({ headless = true } = {}) {
  const profile = await mkdtemp(join(tmpdir(), 'taladb-e2e-'));
  const browser = await puppeteer.launch({
    executablePath: await findChrome(),
    headless,
    userDataDir: profile,
    args: ['--no-sandbox', '--disable-dev-shm-usage', '--enable-features=SharedArrayBuffer'],
  });
  return {
    browser,
    async dispose() {
      await browser.close().catch(() => {});
      await rm(profile, { recursive: true, force: true }).catch(() => {});
    },
  };
}

export async function newTab(browser, { label = 'tab' } = {}) {
  const page = await browser.newPage();
  page.on('console', (m) => {
    const t = m.text();
    if (process.env.VERBOSE) console.log(`  [${label}:${m.type()}] ${t}`);
  });
  page.on('pageerror', (e) => console.log(`  [${label}:pageerror] ${e.message}`));
  await page.goto(`${ORIGIN}/`, { waitUntil: 'load' });
  await page.waitForFunction('window.__ready === true');
  return page;
}

// --- webhook receiver control -------------------------------------------------
export const hook = {
  async reset() {
    await fetch(`${ORIGIN}/ctl/reset`);
  },
  async log() {
    return (await fetch(`${ORIGIN}/ctl/log`)).json();
  },
  async mode(m) {
    await fetch(`${ORIGIN}/ctl/mode`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(m),
    });
  },
  /** Poll until `n` events have arrived or `timeout` elapses. */
  async waitFor(n, timeout = 5000) {
    const deadline = Date.now() + timeout;
    for (;;) {
      const log = await hook.log();
      if (log.length >= n || Date.now() > deadline) return log;
      await sleep(25);
    }
  },
};

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// --- assertion runner ---------------------------------------------------------
export function createRunner(phase) {
  const results = [];
  let current = null;
  const api = {
    results,
    async test(name, fn) {
      current = { name, phase, checks: [], failures: [], notes: [], ms: 0 };
      const t0 = Date.now();
      try {
        await fn(api);
      } catch (e) {
        current.failures.push(`threw: ${e.message}`);
      }
      current.ms = Date.now() - t0;
      results.push(current);
      const bad = current.failures.length;
      const icon = bad ? '✗' : '✓';
      console.log(
        `  ${icon} ${name}${current.notes.length ? ` — ${current.notes.join('; ')}` : ''}`,
      );
      for (const f of current.failures) console.log(`      ↳ ${f}`);
      current = null;
    },
    eq(actual, expected, label) {
      const a = JSON.stringify(actual);
      const e = JSON.stringify(expected);
      current.checks.push(label);
      if (a !== e) current.failures.push(`${label}: expected ${e}, got ${a}`);
      return a === e;
    },
    ok(cond, label) {
      current.checks.push(label);
      if (!cond) current.failures.push(`${label}: expected truthy`);
      return !!cond;
    },
    note(s) {
      current.notes.push(s);
    },
    fail(s) {
      current.failures.push(s);
    },
  };
  return api;
}

export function summarize(all) {
  const failed = all.filter((r) => r.failures.length);
  console.log(`\n${'='.repeat(70)}`);
  console.log(`${all.length - failed.length}/${all.length} passed`);
  if (failed.length) {
    console.log(`\nFAILURES:`);
    for (const f of failed) {
      console.log(`  [${f.phase}] ${f.name}`);
      for (const m of f.failures) console.log(`      ${m}`);
    }
  }
  return failed.length;
}
