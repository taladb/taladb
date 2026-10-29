#!/usr/bin/env node
import { readFile, writeFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

const median = values => {
  const sorted = [...values].sort((a, b) => a - b), mid = Math.floor(sorted.length / 2)
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2
}

export function aggregate(samples) {
  if (!samples.length) throw new Error('no browser samples')
  const signature = report => JSON.stringify([report.ua, report.config, report.capabilities.storage,
    report.cases.map(row => [row.filter, row.mode, row.efSearch, row.recallAtK, row.fingerprint, row.distances, row.memoryFallbacks])])
  if (samples.some(report => signature(report) !== signature(samples[0]))) throw new Error('browser workload results changed between repeated runs')
  const report = structuredClone(samples[0])
  for (const key of ['insertMs', 'buildMs']) report[key] = median(samples.map(s => s[key]))
  for (const key of ['p50Ms', 'p95Ms']) report.buildStep[key] = median(samples.map(s => s.buildStep[key]))
  report.cases.forEach((row, i) => {
    if (row.incorrect) return
    for (const key of ['reopenMs', 'firstQueryMs', 'p50Ms', 'p95Ms']) row[key] = median(samples.map(s => s.cases[i][key]))
    if (row.burst) {
      for (const key of ['p50Ms', 'p95Ms', 'memoryFallbacks']) row.burst[key] = median(samples.map(s => s.cases[i].burst[key]))
    }
  })
  for (const key of ['originMemoryBeforeBytes', 'originMemoryAfterBytes']) {
    const measured = samples.map(s => s[key]).filter(value => value !== null)
    report[key] = measured.length ? median(measured) : null
  }
  report.samples = samples
  return report
}

// Cases the baseline answered wrongly: reported, not compared.
export function incorrectBaseline(before) {
  return [...new Set(before.cases.filter(row => row.incorrect).map(row => `${row.filter}: ${row.incorrect}`))]
}

// One bad run cannot fail a case: when both sides kept their interleaved runs,
// at least two thirds of the pairs must also exceed half the tolerance.
function consistent(before, after, value, tolerance) {
  const a = before.samples, b = after.samples
  if (!a || !b || a.length !== b.length || a.length < 2) return true
  const over = a.filter((sample, i) => value(b[i]) > value(sample) * (1 + tolerance / 2)).length
  return over >= Math.ceil(2 * a.length / 3)
}

export function compare(before, after) {
  const failures = []
  if (JSON.stringify(before.config) !== JSON.stringify(after.config)
      || before.capabilities.storage !== 'opfs' || after.capabilities.storage !== 'opfs'
      || before.ua !== after.ua) return ['browser, OPFS backend or workload configuration changed']
  if (after.buildMs > Math.max(before.buildMs * 1.3, before.buildMs + 500)
      && consistent(before, after, report => report.buildMs, 0.3)) failures.push('build time regressed by more than 30% and 500 ms')
  if (before.cases.length !== after.cases.length) return [...failures, 'workload cases changed']
  for (let i = 0; i < before.cases.length; i++) {
    const old = before.cases[i], row = after.cases[i]
    const label = `${row.filter} ${row.mode ?? "ann"} ef=${row.efSearch}`
    if (row.filter !== old.filter || row.mode !== old.mode || row.efSearch !== old.efSearch) { failures.push('workload cases changed'); continue }
    if (row.incorrect) { failures.push(`${label}: ${row.incorrect}`); continue }
    // The baseline predates a correctness fix; it has no timings to compare.
    if (old.incorrect) continue
    if (row.cacheStats && (row.cacheStats.activeBytes !== 0 || row.cacheStats.peakBytes > row.cacheStats.memoryBudgetBytes || row.cacheStats.retainedBytes > row.cacheStats.budgetBytes)) failures.push(`${label}: search memory exceeded allowance or leaked`)
    if (row.recallAtK + 0.03 < old.recallAtK) failures.push(`${label}: recall fell by more than 3 percentage points`)
    if (row.p50Ms > Math.max(old.p50Ms * 1.5, old.p50Ms + 0.5)
        && consistent(before, after, report => report.cases[i].p50Ms, 0.5)) failures.push(`${label}: warm median latency regressed by more than 50% and 0.5 ms`)
  }
  return failures
}

async function main() {
  const [baseline, candidate, output] = process.argv.slice(2)
  if (!baseline || !candidate || !output) throw new Error('usage: vector-web-bench-compare.mjs baseline.json candidate.json output.json')
  const load = async paths => aggregate(await Promise.all(paths.split(',').map(async path => JSON.parse(await readFile(path)))))
  const before = await load(baseline), after = await load(candidate)
  const failures = compare(before, after)
  await writeFile(output, JSON.stringify({ schema: 1, baseline: before, candidate: after, failures }, null, 2) + '\n')
  const lines = [`Browser worker + OPFS: ${after.config.count} vectors × ${after.config.dimensions} dimensions, ${after.config.cacheBytes} cache bytes, median of ${after.samples.length} runs`,
    '', '| Filter / mode / efSearch | Warm p50 ms before/after | Burst p95 ms before/after | Recall before/after | Peak reserved / limit bytes | Memory fallbacks |', '|---|---:|---:|---:|---:|---:|']
  for (let i = 0; i < Math.min(before.cases.length, after.cases.length); i++) {
    const a = before.cases[i], b = after.cases[i]
    if (a.incorrect || b.incorrect) {
      lines.push(`| ${b.filter} / ${b.mode ?? "ann"} / ${b.efSearch} | ${a.incorrect ? 'baseline incorrect' : a.p50Ms.toFixed(3)} / ${b.incorrect ? 'incorrect' : b.p50Ms.toFixed(3)} | | | | |`)
      continue
    }
    lines.push(`| ${b.filter} / ${b.mode ?? "ann"} / ${b.efSearch} | ${a.p50Ms.toFixed(3)} / ${b.p50Ms.toFixed(3)} | ${a.burst?.p95Ms.toFixed(3) ?? 'unavailable'} / ${b.burst?.p95Ms.toFixed(3) ?? 'unavailable'} | ${(a.recallAtK * 100).toFixed(1)}% / ${(b.recallAtK * 100).toFixed(1)}% | ${b.cacheStats ? `${b.cacheStats.peakBytes} / ${b.cacheStats.memoryBudgetBytes}` : 'unavailable'} | ${(b.memoryFallbacks ?? 0) + (b.burst?.memoryFallbacks ?? 0)} |`)
  }
  lines.push('', `Origin memory bytes before/after: ${before.originMemoryAfterBytes ?? 'unavailable'} / ${after.originMemoryAfterBytes ?? 'unavailable'}`,
    ...incorrectBaseline(before).map(message => `NOTE: baseline returned wrong results, not compared — ${message}`),
    ...failures.map(message => `FAIL: ${message}`))
  const summary = lines.join('\n') + '\n'
  console.log(summary)
  if (process.env.GITHUB_STEP_SUMMARY) await writeFile(process.env.GITHUB_STEP_SUMMARY, summary, { flag: 'a' })
  if (failures.length) process.exitCode = 1
}

if (process.argv[1] === fileURLToPath(import.meta.url)) main().catch(error => { console.error(error); process.exitCode = 1 })
