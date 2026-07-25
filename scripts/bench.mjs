#!/usr/bin/env node
/**
 * TalaDB benchmark suite.
 *
 * Runs against the local @taladb/node native build (release) and prints
 * Markdown tables suitable for docs/benchmarks.md. Pass --json to also write
 * bench-results.json in the working directory.
 *
 *   pnpm --filter @taladb/node build   # build the native module first
 *   node scripts/bench.mjs [--json] [--skip-hnsw]
 *
 * The suite uses a file-backed database (the production configuration) and
 * the raw N-API binding. The `taladb` wrapper adds only a thin async
 * passthrough on Node.js, so these numbers reflect what applications see.
 */
import { createRequire } from 'node:module'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir, cpus, arch, platform, totalmem } from 'node:os'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dir = dirname(fileURLToPath(import.meta.url))
const require = createRequire(import.meta.url)
const { TalaDbNode } = require(join(__dir, '../packages/bindings/node/index.js'))
const pkg = require(join(__dir, '../packages/bindings/node/package.json'))
const skipHnsw = process.argv.includes('--skip-hnsw')

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Deterministic PRNG so runs are comparable. */
function mulberry32(seed) {
  let a = seed >>> 0
  return () => {
    a |= 0; a = (a + 0x6d2b79f5) | 0
    let t = Math.imul(a ^ (a >>> 15), 1 | a)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}
const rand = mulberry32(42)

const nowNs = () => process.hrtime.bigint()
const msSince = (t0) => Number(nowNs() - t0) / 1e6

/**
 * Measure `fn` repeatedly: warmup calls, then timed iterations until either
 * `maxIters` runs or ~`budgetMs` of measured time has accumulated.
 * Returns { median, p95, mean, iters } in milliseconds.
 */
function bench(fn, { warmup = 3, maxIters = 200, minIters = 5, budgetMs = 3000 } = {}) {
  for (let i = 0; i < warmup; i++) fn(i)
  const samples = []
  let spent = 0
  for (let i = 0; i < maxIters; i++) {
    const t0 = nowNs()
    fn(i)
    const ms = msSince(t0)
    samples.push(ms)
    spent += ms
    if (spent > budgetMs && samples.length >= minIters) break
  }
  samples.sort((a, b) => a - b)
  const q = (p) => samples[Math.min(samples.length - 1, Math.floor(p * samples.length))]
  return {
    median: q(0.5),
    p95: q(0.95),
    mean: samples.reduce((s, x) => s + x, 0) / samples.length,
    iters: samples.length,
  }
}

const fmtMs = (ms) => (ms < 1 ? `${(ms * 1000).toFixed(0)} µs` : ms < 100 ? `${ms.toFixed(2)} ms` : `${ms.toFixed(0)} ms`)
const fmtOps = (n) => (n >= 1e6 ? `${(n / 1e6).toFixed(2)}M` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}k` : n.toFixed(0))

// ---------------------------------------------------------------------------
// Data generation
// ---------------------------------------------------------------------------

const CATEGORIES = ['support', 'billing', 'onboarding', 'api', 'security', 'mobile', 'desktop', 'account']
const LOCALES = ['en', 'es', 'fr', 'de', 'ja', 'pt', 'tl', 'ko', 'zh', 'it']

/** A realistic small document (~200 bytes serialised). */
function makeDoc(i) {
  return {
    title: `Article ${i}: how to configure feature ${i % 97}`,
    category: CATEGORIES[i % CATEGORIES.length],
    locale: LOCALES[i % LOCALES.length],
    userId: `user-${i % 10_000}`,
    views: Math.floor(rand() * 100_000),
    publishedAt: 1_700_000_000_000 + i * 1000,
    draft: i % 5 === 0,
  }
}

/** Random unit vector of `dims` dimensions. */
function makeVector(dims) {
  const v = new Array(dims)
  let norm = 0
  for (let i = 0; i < dims; i++) {
    v[i] = rand() * 2 - 1
    norm += v[i] * v[i]
  }
  norm = Math.sqrt(norm) || 1
  for (let i = 0; i < dims; i++) v[i] /= norm
  return v
}

// ---------------------------------------------------------------------------
// Suite
// ---------------------------------------------------------------------------

const results = { meta: {}, sections: [] }
const dir = mkdtempSync(join(tmpdir(), 'taladb-bench-'))
const rows = []
const section = (title) => rows.push({ section: title })
const row = (name, detail, value, extra = {}) => rows.push({ name, detail, value, ...extra })

console.error(`TalaDB benchmark — v${pkg.version} — scratch dir ${dir}`)

try {
  // -------------------------------------------------------------------------
  // 1. Document writes
  // -------------------------------------------------------------------------
  {
    const db = TalaDbNode.open(join(dir, 'writes.db'))
    const col = db.collection('articles')

    // Single inserts — one transaction per call.
    {
      let i = 0
      const r = bench(() => col.insert(makeDoc(i++)), { warmup: 50, maxIters: 2000, budgetMs: 4000 })
      section('Document writes (file-backed)')
      row('insert (single doc)', 'one transaction per call', `${fmtOps(1000 / r.median)} ops/s`, { median: r.median, iters: r.iters })
    }

    // Batched inserts.
    for (const batch of [100, 1000]) {
      let i = 0
      const docs = () => Array.from({ length: batch }, () => makeDoc(i++))
      const r = bench((k) => col.insertMany(docs()), { warmup: 2, maxIters: 50, budgetMs: 4000 })
      row(`insertMany (batch ${batch})`, 'single transaction per batch', `${fmtOps(batch * (1000 / r.median))} docs/s`, { median: r.median, iters: r.iters })
    }

    // updateOne / deleteOne round-trips on a known id.
    const ids = col.insertMany(Array.from({ length: 1000 }, (_, i) => makeDoc(i)))
    {
      let i = 0
      const r = bench(() => col.updateOne({ _id: ids[i++ % ids.length] }, { $set: { views: i } }), { maxIters: 1000, budgetMs: 3000 })
      row('updateOne (by _id)', 'point update, $set one field', `${fmtOps(1000 / r.median)} ops/s`, { median: r.median, iters: r.iters })
    }
    {
      let i = 0
      const r = bench(() => col.deleteOne({ _id: ids[i++ % ids.length] }), { warmup: 0, maxIters: 500, budgetMs: 3000 })
      row('deleteOne (by _id)', 'point delete', `${fmtOps(1000 / r.median)} ops/s`, { median: r.median, iters: r.iters })
    }
  }

  // -------------------------------------------------------------------------
  // 2. Queries at 100k documents
  // -------------------------------------------------------------------------
  {
    const N = 100_000
    const db = TalaDbNode.open(join(dir, 'queries.db'))
    const col = db.collection('articles')

    console.error(`ingesting ${N} documents for query benchmarks…`)
    const allIds = []
    const t0 = nowNs()
    for (let i = 0; i < N; i += 5000) {
      allIds.push(...col.insertMany(Array.from({ length: 5000 }, (_, j) => makeDoc(i + j))))
    }
    const ingestMs = msSince(t0)

    col.createIndex('userId')
    col.createIndex('publishedAt')

    section(`Queries — 100,000 documents`)
    row('bulk ingest 100k docs', 'insertMany, batches of 5,000', `${fmtOps(N / (ingestMs / 1000))} docs/s`, { median: ingestMs })

    // Point get by _id.
    {
      let i = 0
      const r = bench(() => col.findOne({ _id: allIds[(i += 7919) % N] }), { maxIters: 500 })
      row('findOne by _id', 'primary-key point get', fmtMs(r.median), { median: r.median, iters: r.iters })
    }

    // Indexed equality — userId has 10k distinct values → ~10 matches.
    {
      let i = 0
      const r = bench(() => col.find({ userId: `user-${(i += 977) % 10_000}` }), { maxIters: 500 })
      row('find, indexed equality', '`userId` (secondary index, ~10 matches)', fmtMs(r.median), { median: r.median, iters: r.iters })
    }

    // Same query shape, no index — full collection scan.
    {
      let i = 0
      const r = bench(() => col.find({ title: `Article ${(i += 977) % N}: how to configure feature ${((i % N) % 97)}` }), { maxIters: 20, budgetMs: 4000 })
      row('find, unindexed field', 'full scan of 100k docs (~1 match)', fmtMs(r.median), { median: r.median, iters: r.iters })
    }

    // Indexed range — one-sided $gte over the newest ~100 docs. The planner
    // turns a single bound into a bounded index scan.
    {
      const start = 1_700_000_000_000 + (N - 100) * 1000
      const r = bench(() => col.find({ publishedAt: { $gte: start } }), { maxIters: 500 })
      row('find, indexed range ($gte)', '`publishedAt`, newest ~100 docs', fmtMs(r.median), { median: r.median, iters: r.iters })
    }

    // Two-sided range ($gte + $lt). Since v0.9.0 the planner emits a single
    // bounded index scan for both bounds, so only the ~100-doc window is read.
    {
      let i = 0
      const r = bench(() => {
        const start = 1_700_000_000_000 + ((i += 4409) % (N - 100)) * 1000
        return col.find({ publishedAt: { $gte: start, $lt: start + 100_000 } })
      }, { maxIters: 500 })
      row('find, indexed range ($gte + $lt)', '`publishedAt` window (~100 matches); bounded index scan', fmtMs(r.median), { median: r.median, iters: r.iters })
    }

    // Count with an unindexed equality (measures filtered scan without result materialisation).
    {
      const r = bench(() => col.count({ category: 'support' }), { maxIters: 20, budgetMs: 4000 })
      row('count, unindexed equality', '`category` scan, 12.5k matches', fmtMs(r.median), { median: r.median, iters: r.iters })
    }
  }

  // -------------------------------------------------------------------------
  // 3. Vector search — 384-dim (all-MiniLM-L6-v2 shape), cosine, flat index
  // -------------------------------------------------------------------------
  {
    const DIMS = 384
    const TOP_K = 10
    section(`Vector search — ${DIMS}-dim, cosine, top-${TOP_K} (flat index)`)

    for (const N of [1_000, 10_000, 50_000, 100_000]) {
      const db = TalaDbNode.open(join(dir, `vec-${N}.db`))
      const col = db.collection('chunks')
      col.createVectorIndex('embedding', DIMS)
      col.createIndex('locale') // hybrid queries pre-filter on locale; index it like a real app would

      console.error(`ingesting ${N} vectors…`)
      const t0 = nowNs()
      for (let i = 0; i < N; i += 500) {
        const batch = Math.min(500, N - i)
        col.insertMany(Array.from({ length: batch }, (_, j) => ({
          ...makeDoc(i + j),
          embedding: makeVector(DIMS),
        })))
      }
      const ingestMs = msSince(t0)

      const queries = Array.from({ length: 20 }, () => makeVector(DIMS))
      let i = 0
      const r = bench(() => col.findNearest('embedding', queries[i++ % queries.length], TOP_K), { maxIters: 60, budgetMs: 4000 })
      row(`findNearest, ${N.toLocaleString('en-US')} vectors`, 'exact k-NN over all vectors', fmtMs(r.median), { median: r.median, iters: r.iters, ingestDocsPerSec: N / (ingestMs / 1000) })

      if (N === 100_000) {
        // Hybrid: metadata pre-filter (locale = 'en' → 10% of docs) then rank.
        let j = 0
        const rh = bench(() => col.findNearest('embedding', queries[j++ % queries.length], TOP_K, { locale: 'en' }), { maxIters: 60, budgetMs: 4000 })
        row(`findNearest + filter, ${N.toLocaleString('en-US')} vectors`, 'pre-filter `locale: "en"` (10%), then rank', fmtMs(rh.median), { median: rh.median, iters: rh.iters })
        // Vector ingest rate at scale.
        row(`vector ingest, ${N.toLocaleString('en-US')} vectors`, 'insertMany with vector index live', `${fmtOps(N / (ingestMs / 1000))} docs/s`, { median: ingestMs })
      }

      if (N === 50_000 && !skipHnsw) {
        // HNSW — approximate index (needs a binary built with vector-hnsw,
        // shipped in @taladb/node since 0.8.3). Recall is measured against
        // the exact flat results for the same query vectors. Measured at 50k,
        // not 100k: graph construction is CPU-heavy (tens of minutes at 100k
        // on laptop hardware), which would make the suite impractical to run.
        const exact = queries.map((q) => new Set(col.findNearest('embedding', q, TOP_K).map((r) => r.document._id)))
        col.dropVectorIndex('embedding')
        const tBuild = nowNs()
        col.createVectorIndex('embedding', DIMS, 'cosine', 'hnsw')
        const buildMs = msSince(tBuild)
        let k = 0
        const ra = bench(() => col.findNearest('embedding', queries[k++ % queries.length], TOP_K), { maxIters: 200, budgetMs: 3000 })
        let hits = 0
        for (const [qi, q] of queries.entries()) {
          for (const r of col.findNearest('embedding', q, TOP_K)) {
            if (exact[qi].has(r.document._id)) hits++
          }
        }
        const recall = hits / (queries.length * TOP_K)
        if (recall < 0.5) {
          // Latency ≈ flat + recall 100% would also betray a flat fallback;
          // a low-recall or flat-identical result means the feature is absent.
          console.error('warning: HNSW recall suspiciously low — is the binary built with vector-hnsw?')
        }
        row(`findNearest (HNSW), ${N.toLocaleString('en-US')} vectors`, `approximate; recall@10 = ${(recall * 100).toFixed(0)}% vs exact`, fmtMs(ra.median), { median: ra.median, iters: ra.iters, recall })
        row(`HNSW graph build, ${N.toLocaleString('en-US')} vectors`, 'createVectorIndex backfill + graph build, one-off', fmtMs(buildMs), { median: buildMs })
      }
    }
  }
} finally {
  rmSync(dir, { recursive: true, force: true })
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

const cpu = cpus()[0]?.model ?? 'unknown CPU'
results.meta = {
  taladb: pkg.version,
  node: process.version,
  platform: `${platform()} ${arch()}`,
  cpu,
  memoryGB: Math.round(totalmem() / 2 ** 30),
  date: new Date().toISOString().slice(0, 10),
}

console.log(`\nTalaDB v${pkg.version} · ${cpu} · ${results.meta.platform} · Node ${process.version} · ${results.meta.date}\n`)
for (const r of rows) {
  if (r.section) {
    console.log(`\n### ${r.section}\n`)
    console.log('| Operation | Detail | Result |')
    console.log('|---|---|---|')
  } else {
    console.log(`| ${r.name} | ${r.detail} | **${r.value}** |`)
  }
}

if (process.argv.includes('--json')) {
  results.rows = rows
  writeFileSync('bench-results.json', JSON.stringify(results, null, 2))
  console.error('\nwrote bench-results.json')
}
