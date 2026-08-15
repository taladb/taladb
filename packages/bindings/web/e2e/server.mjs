// Static server for the TalaDB browser e2e harness + a webhook receiver.
//
// Path mapping mimics what a bundler does for the client's
// `new URL('@taladb/web/pkg/taladb_web.js', import.meta.url)` resolution:
// that yields /taladb/@taladb/web/pkg/... relative to the served client module,
// so any path containing `@taladb/web/{pkg,worker}/` is routed to the workspace.
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join } from 'node:path';

const HERE = new URL('.', import.meta.url).pathname;
/** Repository root — this file lives at packages/bindings/web/e2e/. */
const REPO = new URL('../../../../', import.meta.url).pathname;
/** 0 asks the OS for a free port — a leftover server must not break a run. */
const PORT = Number(process.env.PORT ?? 0);

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.wasm': 'application/wasm',
  '.json': 'application/json',
  '.map': 'application/json',
};

/** @type {{method:string,body:any,at:number,seq:number}[]} */
let events = [];
let seq = 0;
/** Webhook receiver behaviour, driven by /ctl/mode. */
let mode = { status: 200, failFirst: 0, delayMs: 0 };
let failCount = 0;

function json(res, code, obj) {
  const body = JSON.stringify(obj);
  res.writeHead(code, {
    'content-type': 'application/json',
    'content-length': Buffer.byteLength(body),
  });
  res.end(body);
}

async function readBody(req) {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  if (chunks.length === 0) return null;
  const text = Buffer.concat(chunks).toString('utf8');
  try {
    return JSON.parse(text);
  } catch {
    return { _raw: text };
  }
}

const server = createServer(async (req, res) => {
  const url = new URL(req.url, `http://${req.headers.host}`);
  const path = url.pathname;

  // --- webhook receiver -----------------------------------------------------
  if (path === '/hook' || path.startsWith('/hook/')) {
    const body = await readBody(req);
    if (mode.delayMs) await new Promise((r) => setTimeout(r, mode.delayMs));
    if (mode.failFirst > failCount) {
      failCount++;
      events.push({ method: req.method, body, at: Date.now(), seq: seq++, rejected: 500 });
      return json(res, 500, { error: 'injected' });
    }
    if (mode.status !== 200) {
      events.push({ method: req.method, body, at: Date.now(), seq: seq++, rejected: mode.status });
      return json(res, mode.status, { error: 'injected' });
    }
    events.push({ method: req.method, body, at: Date.now(), seq: seq++ });
    return json(res, 200, { ok: true });
  }

  // --- control --------------------------------------------------------------
  if (path === '/ctl/log') return json(res, 200, events);
  if (path === '/ctl/reset') {
    events = [];
    seq = 0;
    failCount = 0;
    mode = { status: 200, failFirst: 0, delayMs: 0 };
    return json(res, 200, { ok: true });
  }
  if (path === '/ctl/mode') {
    const body = (await readBody(req)) ?? {};
    mode = { status: 200, failFirst: 0, delayMs: 0, ...body };
    failCount = 0;
    return json(res, 200, mode);
  }

  // --- static ---------------------------------------------------------------
  let file = null;
  if (path === '/' || path === '/index.html') file = join(HERE, 'index.html');
  else if (path === '/harness.js') file = join(HERE, 'harness.js');
  else if (path.includes('@taladb/web/pkg/')) {
    file = join(REPO, 'packages/bindings/web/pkg', path.split('@taladb/web/pkg/')[1]);
  } else if (path.includes('@taladb/web/worker/')) {
    file = join(REPO, 'packages/bindings/web/worker', path.split('@taladb/web/worker/')[1]);
  } else if (path.startsWith('/taladb/')) {
    file = join(REPO, 'packages/clients/taladb/dist', path.slice('/taladb/'.length));
  }

  if (!file) {
    res.writeHead(404);
    return res.end('not found');
  }

  try {
    const buf = await readFile(file);
    res.writeHead(200, {
      'content-type': MIME[extname(file)] ?? 'application/octet-stream',
      // Cross-origin isolation, matching what the docs tell users to configure.
      'cross-origin-opener-policy': 'same-origin',
      'cross-origin-embedder-policy': 'require-corp',
      'cross-origin-resource-policy': 'same-origin',
      'cache-control': 'no-store',
    });
    res.end(buf);
  } catch (e) {
    res.writeHead(404);
    res.end(`not found: ${file} (${e.message})`);
  }
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`e2e server on http://127.0.0.1:${server.address().port}`);
});
