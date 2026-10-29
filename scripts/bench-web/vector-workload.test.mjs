import { test } from 'node:test'
import assert from 'node:assert/strict'
import { settings, dataset, percentiles, runVectorBenchmark, IncorrectResult } from './vector-workload.js'
import { aggregate, compare, incorrectBaseline } from '../vector-web-bench-compare.mjs'

test('settings validate device budgets and dataset probes are stable across collection sizes', () => {
  const config = settings('count=100&dims=8&queries=3&cache-bytes=1048576')
  assert.equal(config.cacheBytes, 1048576)
  const a = dataset(config), b = dataset({ ...config, count: 200 })
  assert.deepEqual(a.probes, b.probes)
  assert.deepEqual(a, dataset(config))
  assert.ok(a.documents.every(doc => Math.abs(doc.v.reduce((s, x) => s + x * x, 0) - 1) < 1e-10))
  for (const query of ['count=1', 'queries=0', 'dims=NaN', 'cache-bytes=-1', 'concurrency=0', 'concurrency=17', 'quantization=bad']) assert.throws(() => settings(query))
  assert.deepEqual(percentiles([4, 1, 3, 2]), { p50Ms: 3, p95Ms: 4 })
})

function fake(storage = 'opfs', fail = false, { fallback = false, noStats = false, leak = false, scalarArrays = false } = {}) {
  const calls = [], documents = []
  let opens = 0, closes = 0, terminated = 0
  class Client {
    worker = { terminate() { terminated++ } }
    async call(op, args) {
      calls.push({ op, args })
      if (op === 'init') {
        opens++
        const config = JSON.parse(args.configJson)
        this.baselineBytes = config.vector_cache_bytes ?? Math.max(1048576, Math.min(16777216, Math.floor((config.vector_cache_memory_bytes ?? 4 * 1024 ** 3) / 512)))
        this.cacheBytes = this.baselineBytes
        return
      }
      if (op === 'close') { closes++; return }
      if (op === 'capabilities') return { storage }
      if (op === 'insertMany') { documents.push(...JSON.parse(args.docsJson)); return }
      if (op === 'createIndex') return
      const request = JSON.parse(args.requestJson)
      if (request.op === 'memoryPressure') {
        this.cacheBytes = request.level === 'critical' ? 0 : request.level === 'moderate' ? Math.floor(this.baselineBytes / 4) : this.baselineBytes
      }
      if (['cacheStats', 'memoryPressure'].includes(request.op)) {
        if (noStats) throw new Error('unknown op')
        return { budgetBytes: this.cacheBytes, baselineBudgetBytes: this.baselineBytes, memoryBudgetBytes: Math.max(this.cacheBytes, 65536), retainedBytes: Math.min(this.cacheBytes, 2048), activeBytes: leak ? 1 : 0, peakBytes: Math.max(this.cacheBytes, 65536) }
      }
      if (request.op === 'create') return
      if (request.op === 'beginBuild') { this.processed = 0; return { id: 'build' } }
      if (request.op === 'stepBuild') {
        this.processed = Math.min(documents.length, this.processed + request.batchSize)
        return { processed: this.processed, total: documents.length, state: this.processed === documents.length ? 'ready' : 'building' }
      }
      if (fail) throw new Error('search failed')
      const matches = (doc, filter) => Object.entries(filter ?? {}).every(([field, value]) => {
        if (field === '$and') return value.every(child => matches(doc, child))
        if (value && typeof value === 'object') {
          const values = Array.isArray(doc[field]) ? doc[field] : [doc[field]]
          const test = ([op, bound]) => v => op === '$gte' ? v >= bound : v < bound
          // Engines before array-aware narrowing required one element to satisfy every bound.
          if (scalarArrays) return values.some(v => Object.entries(value).every(entry => test(entry)(v)))
          return Object.entries(value).every(entry => values.some(test(entry)))
        }
        return doc[field] === value
      })
      let matching = documents.filter(doc => matches(doc, request.filter))
      if (request.options.groupBy) {
        const counts = new Map()
        matching = matching.filter(doc => {
          const key = doc[request.options.groupBy], count = counts.get(key) ?? 0
          counts.set(key, count + 1)
          return count < request.options.groupSize
        })
      }
      matching = matching.slice(request.options.offset ?? 0)
      return { hits: matching.slice(0, request.topK).map(document => ({ document, score: 1 / (document.ordinal + 1) })),
        execution: { path: request.options.mode === 'ann' && !fallback ? 'hnsw' : 'exact', reason: request.options.mode === 'ann' && fallback ? 'memoryBudget' : 'indexReady', distanceComputations: matching.length } }
    }
  }
  return { Client, calls, stats: () => ({ opens, closes, terminated }) }
}

test('worker workload uses configured cache, staged builds, persisted reopen and filtered ground truth', async () => {
  const f = fake()
  let time = 0
  const config = settings('count=100&dims=8&queries=3')
  const result = await runVectorBenchmark(config, { Client: f.Client, now: () => time++, memory: async () => 123 })
  assert.equal(result.cases.length, 36)
  assert.ok(result.cases.every(row => row.recallAtK === 1 && row.firstQueryMs > 0))
  assert.equal(result.cases.filter(row => row.mode === 'exact').length, 9)
  assert.ok(f.calls.some(c => c.op === 'vectorCommand' && JSON.parse(c.args.requestJson).options?.groupBy === 'tenant' && JSON.parse(c.args.requestJson).options?.offset === 2))
  assert.equal(result.buildStep.count, 4)
  assert.ok(result.cases.every(row => row.burst.requests === config.queries * config.concurrency && row.cacheStats.activeBytes === 0))
  assert.equal(result.originMemoryAfterBytes, 123)
  assert.deepEqual(f.stats(), { opens: 37, closes: 37, terminated: 37 })
  for (const name of ['and-skew-1pct', 'scalar-window-1pct', 'array-cross-1pct']) {
    assert.equal(result.cases.filter(row => row.filter === name).length, 4)
  }
  assert.ok(f.calls.filter(c => c.op === 'init').every(c => JSON.parse(c.args.configJson).vector_cache_bytes === config.cacheBytes))
  const builds = f.calls.filter(c => c.op === 'vectorCommand').map(c => JSON.parse(c.args.requestJson)).filter(c => c.op === 'stepBuild')
  assert.ok(builds.every(c => c.batchSize === 32))
})

test('adaptive browser profiles verify pressure, zero retention and recovery budgets', async () => {
  const f = fake()
  const config = settings('count=100&dims=8&queries=2&cache-bytes=auto&memory-hint-bytes=2147483648&pressure=1')
  const report = await runVectorBenchmark(config, { Client: f.Client, memory: async () => null })
  assert.equal(config.cacheBytes, null)
  assert.deepEqual(report.pressureCycle.map(row => row.level), ['normal', 'moderate', 'critical', 'normal'])
  assert.deepEqual(report.pressureCycle.map(row => row.cacheStats.budgetBytes), [4194304, 1048576, 0, 4194304])
  assert.equal(report.pressureCycle[2].cacheStats.retainedBytes, 0)
  assert.ok(report.pressureCycle.every(row => row.recallAtK === 1 && row.cacheStats.activeBytes === 0))
  const configJson = JSON.parse(f.calls.find(c => c.op === 'init').args.configJson)
  assert.equal(configJson.vector_cache_bytes, undefined)
  assert.equal(configJson.vector_cache_memory_bytes, 2147483648)
  for (const invalid of ['pressure=yes', 'memory-hint-bytes=0', 'memory-hint-bytes=Infinity']) assert.throws(() => settings(invalid))
})

test('unavailable OPFS and failed queries reject and close their workers', async () => {
  for (const [storage, fail, error] of [['indexeddb', false, /requires OPFS/], ['opfs', true, /search failed/]]) {
    const f = fake(storage, fail)
    await assert.rejects(runVectorBenchmark(settings('count=100&dims=8&queries=1'), { Client: f.Client, memory: async () => null }), error)
    assert.deepEqual(f.stats(), { opens: 1, closes: 1, terminated: 1 })
  }
})

test('CI uses medians, retains samples, rejects unstable results and gates regressions', async () => {
  const f = fake()
  const report = await runVectorBenchmark(settings('count=100&dims=8&queries=1'), { Client: f.Client, memory: async () => null })
  report.ua = 'test browser'
  const samples = [1, 2, 100].map(ms => {
    const sample = structuredClone(report)
    sample.cases.forEach(row => { row.p50Ms = ms; row.p95Ms = ms * 2 })
    return sample
  })
  const result = aggregate(samples)
  assert.equal(result.cases[0].p50Ms, 2)
  assert.equal(result.samples.length, 3)
  assert.equal(result.originMemoryAfterBytes, null)
  assert.deepEqual(compare(result, result), [])
  const legacy = structuredClone(report)
  legacy.cases.forEach(row => { delete row.burst; delete row.cacheStats; delete row.memoryFallbacks })
  const legacyAggregate = aggregate([legacy, legacy])
  assert.deepEqual(compare(legacyAggregate, legacyAggregate), [])
  const slower = structuredClone(result)
  slower.cases[0].p50Ms = 10
  // The aggregate alone is not enough: the interleaved runs must agree.
  assert.deepEqual(compare(result, slower), [])
  slower.samples.forEach(sample => { sample.cases[0].p50Ms *= 10 })
  assert.ok(compare(result, slower).length)
  slower.cases[0].p50Ms = 2; slower.cases[0].recallAtK = 0
  assert.ok(compare(result, slower).length)
  slower.cases[0].cacheStats.peakBytes = 9000000
  assert.ok(compare(result, slower).some(f => f.includes('memory')))
  samples[1].cases[0].fingerprint++
  assert.throws(() => aggregate(samples), /changed between repeated runs/)
})


test('browser reports exact memory fallbacks and supports baseline bindings without cache statistics', async () => {
  const f = fake('opfs', false, { fallback: true, noStats: true })
  const config = settings('count=100&dims=8&queries=2&concurrency=3')
  const result = await runVectorBenchmark(config, { Client: f.Client, memory: async () => null })
  assert.ok(result.cases.filter(row => row.mode === 'ann').every(row => row.firstMemoryFallback && row.memoryFallbacks === 2 && row.burst.memoryFallbacks === 6 && row.cacheStats === null))
})

test('browser rejects leaked search reservations and releases its worker', async () => {
  const f = fake('opfs', false, { leak: true })
  await assert.rejects(runVectorBenchmark(settings('count=100&dims=8&queries=1'), { Client: f.Client, memory: async () => null }), /exceeded budget or leaked/)
  assert.equal(f.stats().opens, f.stats().terminated)
})

test('baselines that predate a correctness fix are recorded and skipped, candidates are rejected', async () => {
  const config = settings('count=100&dims=8&queries=2')
  await assert.rejects(runVectorBenchmark(config, { Client: fake('opfs', false, { scalarArrays: true }).Client, memory: async () => null }), IncorrectResult)
  const f = fake('opfs', false, { scalarArrays: true })
  // Step clocks keep the latency gate out of this test; real timings vary by runner.
  const clock = () => { let time = 0; return () => time++ }
  const before = await runVectorBenchmark(config, { Client: f.Client, now: clock(), memory: async () => null, baseline: true })
  assert.equal(f.stats().opens, f.stats().terminated)
  const wrong = before.cases.filter(row => row.incorrect)
  assert.deepEqual(wrong.map(row => [row.filter, row.mode, row.efSearch]),
    [['array-cross-1pct', 'exact', 100], ['array-cross-1pct', 'ann', 64], ['array-cross-1pct', 'ann', 100], ['array-cross-1pct', 'ann', 200]])
  assert.match(wrong[0].incorrect, /exact filter eligibility mismatch/)
  assert.equal(before.cases.length, 36)
  const after = await runVectorBenchmark(config, { Client: fake().Client, now: clock(), memory: async () => null })
  for (const report of [before, after]) report.ua = 'test browser'
  const a = aggregate([before, before]), b = aggregate([after, after])
  assert.deepEqual(compare(a, b), [])
  assert.deepEqual(incorrectBaseline(a), ['array-cross-1pct: exact filter eligibility mismatch: array-cross-1pct'])
  assert.ok(compare(b, a).some(message => message.includes('array-cross-1pct exact ef=100')))
})
