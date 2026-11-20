'use strict'
// Child process for input-depth.test.mjs: passes pathological arguments to
// every exported method that takes a JS object. A regression in the depth
// limit crashes this process (SIGSEGV) instead of throwing, and the parent
// sees that as a signal rather than losing its own test runner.
//
// Usage: node input-depth-child.cjs <shape> <depth>
// Prints one JSON line: [{ method, arg, outcome, message }].

const { TalaDbNode } = require('..')
const { nest, cyclic, methodCalls } = require('./input-depth-cases.cjs')

async function main() {
  const [shape, depthArg] = process.argv.slice(2)
  const value = shape.startsWith('cyclic') ? cyclic(shape) : nest(shape, Number(depthArg))

  const db = TalaDbNode.openInMemory()
  const col = db.collection('deep')
  col.createFtsIndex('title')
  col.createVectorIndex('embedding', 2)
  col.insert({ title: 'a b', embedding: [1, 0] })

  const out = []
  for (const { method, arg, call } of methodCalls(value)) {
    try {
      await call(col)
      out.push({ method, arg, outcome: 'ok' })
    } catch (e) {
      out.push({ method, arg, outcome: 'error', message: String(e && e.message) })
    }
  }
  db.close()
  process.stdout.write(JSON.stringify(out) + '\n')
}

main().catch((e) => {
  process.stderr.write(String(e && e.stack) + '\n')
  process.exit(2)
})
