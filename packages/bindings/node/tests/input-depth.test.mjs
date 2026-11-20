// Node binding: deeply nested arguments must be refused with an error, never
// crash the process. Run with `node --test tests/` after `pnpm build`.
//
// napi-rs converts a JS argument into `serde_json::Value` recursively with no
// depth limit; a 10,000-level object overflowed the main-thread stack
// (SIGSEGV) before TalaDB's 64-level check ran. The binding now converts
// through a depth-bounded `JsonValue` (src/napi_json.rs) that stops at 256.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'
import { Worker } from 'node:worker_threads'

const require = createRequire(import.meta.url)
const { TalaDbNode } = require('..')
const { nest, methodCalls } = require('./input-depth-cases.cjs')

const CHILD = fileURLToPath(new URL('./input-depth-child.cjs', import.meta.url))
const CONVERSION_LIMIT = 256
const TOO_DEEP = /value nested deeper than 256 levels/
const ENGINE_TOO_DEEP = /JSON nested deeper than 64 levels/
const SHAPES = ['object', 'array', 'mixed']

function runChild(shape, depth = 0) {
  const r = spawnSync(process.execPath, [CHILD, shape, String(depth)], {
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
  })
  assert.equal(r.signal, null, `child killed by ${r.signal} (${shape}, ${depth})\n${r.stderr}`)
  assert.equal(r.status, 0, `child exit ${r.status} (${shape}, ${depth})\n${r.stderr}`)
  return JSON.parse(r.stdout)
}

function openCollection() {
  const db = TalaDbNode.openInMemory()
  return { db, col: db.collection('deep') }
}

// Every argument position, every shape, far past the limit and at the depth
// that used to crash. Each run is a separate process.
for (const shape of SHAPES) {
  for (const depth of [CONVERSION_LIMIT + 1, 10_000, 100_000]) {
    test(`${shape} nested ${depth} levels: every method refuses, no crash`, () => {
      const results = runChild(shape, depth)
      assert.equal(results.length, methodCalls(null).length)
      for (const r of results) {
        assert.equal(r.outcome, 'error', `${r.method}(${r.arg}) accepted a ${depth}-level value`)
        assert.match(r.message, TOO_DEEP, `${r.method}(${r.arg}): ${r.message}`)
      }
    })
  }
}

for (const shape of ['cyclic-object', 'cyclic-array']) {
  test(`${shape}: every method refuses, no crash`, () => {
    for (const r of runChild(shape)) {
      assert.equal(r.outcome, 'error', `${r.method}(${r.arg}) accepted a cycle`)
      assert.match(r.message, TOO_DEEP, `${r.method}(${r.arg}): ${r.message}`)
    }
  })
}

// At the conversion limit the argument converts, and the engine's own rule
// decides, with the error it always gave. One level more is refused by the
// conversion instead.
test('boundary: 256 levels reach the engine check, 257 are refused at conversion', () => {
  const { db, col } = openCollection()
  for (const shape of SHAPES) {
    assert.throws(() => col.insert(nest(shape, CONVERSION_LIMIT)), ENGINE_TOO_DEEP, shape)
    assert.throws(() => col.find(nest(shape, CONVERSION_LIMIT)), ENGINE_TOO_DEEP, shape)
    assert.throws(() => col.insert(nest(shape, CONVERSION_LIMIT + 1)), TOO_DEEP, shape)
    assert.throws(() => col.find(nest(shape, CONVERSION_LIMIT + 1)), TOO_DEEP, shape)
  }
  db.close()
})

// Unchanged behaviour: the engine's 64-level document rule.
test('documents: 64 levels insert and read back, 65 are refused as before', () => {
  const { db, col } = openCollection()
  for (const shape of ['object', 'mixed']) {
    const doc = { tag: shape, body: nest(shape, 63) } // body at depth 2..64
    const id = col.insert(doc)
    const back = col.findOne({ _id: id })
    assert.deepEqual(back.body, doc.body)
    assert.throws(() => col.insert({ tag: shape, body: nest(shape, 64) }), ENGINE_TOO_DEEP)
  }
  db.close()
})

// A call whose arguments nest past 64 in total is valid when the engine checks
// a sub-value on its own: an aggregate pipeline holds each `$match` filter two
// levels down. This is why the conversion limit cannot simply be 64.
test('aggregate: a 63-level $match inside the pipeline (65 levels in total) still works', () => {
  const { db, col } = openCollection()
  col.insert({ a: 1 })
  let filter = { a: 1 }
  for (let i = 0; i < 31; i++) filter = { $and: [filter] } // 1 + 31*2 = 63 levels
  const out = col.aggregate([{ $match: filter }])
  assert.equal(out.length, 1)
  db.close()
})

// Ordinary inputs convert exactly as napi-rs's own conversion did.
test('ordinary values: types, key order, undefined and holes behave as before', () => {
  const { db, col } = openCollection()
  const id = col.insert({
    s: 'x',
    i: 42,
    neg: -7,
    f: 1.5,
    big: 2 ** 40,
    t: true,
    n: null,
    arr: [1, 'two', [3], { four: 4 }],
    nested: { z: 1, a: 2 },
    skipped: undefined,
  })
  const doc = col.findOne({ _id: id })
  assert.equal(doc.s, 'x')
  assert.equal(doc.i, 42)
  assert.equal(doc.neg, -7)
  assert.equal(doc.f, 1.5)
  assert.equal(doc.big, 2 ** 40)
  assert.equal(doc.t, true)
  assert.equal(doc.n, null)
  assert.deepEqual(doc.arr, [1, 'two', [3], { four: 4 }])
  assert.deepEqual(Object.keys(doc.nested), ['z', 'a'])
  assert.equal('skipped' in doc, false)

  assert.equal(col.count({ i: { $gte: 40 } }), 1)
  assert.equal(col.updateOne({ s: 'x' }, { $set: { s: 'y' } }), true)
  assert.equal(col.find({ s: 'y' }).length, 1)

  // Errors that napi-rs's conversion gave keep their text.
  assert.throws(() => col.insert({ a: [1, undefined] }), /undefined cannot be represented/)
  assert.throws(() => col.insert({ a: () => 1 }), /JS functions cannot be represented/)
  assert.throws(() => col.insert({ a: Symbol('s') }), /JS symbols cannot be represented/)
  // eslint-disable-next-line no-sparse-arrays
  assert.throws(() => col.insert({ a: [1, , 3] }), /undefined cannot be represented/)
  db.close()
})

test('async variants convert on the calling thread and still work', async () => {
  const { db, col } = openCollection()
  const id = await col.insertAsync({ a: { b: [1, 2] } })
  const found = await col.findAsync({ _id: id })
  assert.deepEqual(found[0].a, { b: [1, 2] })
  assert.equal(await col.updateOneAsync({ _id: id }, { $set: { c: 1 } }), true)
  assert.equal(await col.deleteManyAsync({ c: 1 }), 1)
  db.close()
})

// Node worker threads default to a 4 MiB stack, half the main thread's.
test('worker thread: over-limit input is refused, not a crash', async () => {
  const src = `
    const { parentPort, workerData } = require('node:worker_threads')
    const { TalaDbNode } = require(workerData.binding)
    const { nest } = require(workerData.cases)
    const col = TalaDbNode.openInMemory().collection('w')
    const res = []
    for (const d of [256, 257, 100000]) {
      try { col.insert(nest('mixed', d)); res.push('ok') } catch (e) { res.push(e.message) }
    }
    parentPort.postMessage(res)`
  const worker = new Worker(src, {
    eval: true,
    workerData: {
      binding: fileURLToPath(new URL('..', import.meta.url)),
      cases: fileURLToPath(new URL('./input-depth-cases.cjs', import.meta.url)),
    },
  })
  const [res, code] = await new Promise((resolve, reject) => {
    let msg
    worker.on('message', (m) => (msg = m))
    worker.on('error', reject)
    worker.on('exit', (c) => resolve([msg, c]))
  })
  assert.equal(code, 0)
  assert.match(res[0], ENGINE_TOO_DEEP)
  assert.match(res[1], TOO_DEEP)
  assert.match(res[2], TOO_DEEP)
})
