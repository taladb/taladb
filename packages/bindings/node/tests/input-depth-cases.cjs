'use strict'
// Shared inputs for the input-depth tests.

/**
 * `depth` nested containers, built iteratively (a recursive builder would hit
 * V8's own stack limit at the depths under test). The innermost value is an
 * empty container, so `depth` is exactly the number of objects/arrays on the
 * deepest path.
 *   object: {k:{k:…{}}}   array: [[…[]]]   mixed: {k:[{k:[…]}]}
 */
function nest(shape, depth) {
  const objectAt = (i) => (shape === 'object' ? true : shape === 'array' ? false : i % 2 === 0)
  let v = objectAt(depth - 1) ? {} : []
  for (let i = depth - 2; i >= 0; i--) {
    v = objectAt(i) ? { k: v } : [v]
  }
  return v
}

/** Values that reference themselves: no finite depth at all. */
function cyclic(shape) {
  if (shape === 'cyclic-array') {
    const a = []
    a.push(a)
    return a
  }
  const o = { k: 1 }
  o.self = o
  return o
}

/**
 * One entry per JS-object argument position of every exported method, with
 * `value` placed in that position and every other argument valid.
 */
function methodCalls(value) {
  const f = {}
  const vec = new Float32Array([1, 0])
  return [
    { method: 'insert', arg: 'doc', call: (c) => c.insert(value) },
    { method: 'insertMany', arg: 'docs[0]', call: (c) => c.insertMany([value]) },
    { method: 'find', arg: 'filter', call: (c) => c.find(value) },
    { method: 'findOne', arg: 'filter', call: (c) => c.findOne(value) },
    { method: 'aggregate', arg: 'pipeline', call: (c) => c.aggregate(value) },
    { method: 'updateOne', arg: 'filter', call: (c) => c.updateOne(value, { $set: { a: 1 } }) },
    { method: 'updateOne', arg: 'update', call: (c) => c.updateOne(f, value) },
    { method: 'updateMany', arg: 'filter', call: (c) => c.updateMany(value, { $set: { a: 1 } }) },
    { method: 'updateMany', arg: 'update', call: (c) => c.updateMany(f, value) },
    { method: 'deleteOne', arg: 'filter', call: (c) => c.deleteOne(value) },
    { method: 'deleteMany', arg: 'filter', call: (c) => c.deleteMany(value) },
    { method: 'count', arg: 'filter', call: (c) => c.count(value) },
    { method: 'searchText', arg: 'filter', call: (c) => c.searchText('title', 'a', 5, value) },
    { method: 'searchText', arg: 'options', call: (c) => c.searchText('title', 'a', 5, null, value) },
    { method: 'hybridSearch', arg: 'filter', call: (c) => c.hybridSearch('title', 'a', 'embedding', [1, 0], 5, value) },
    { method: 'hybridSearch', arg: 'options', call: (c) => c.hybridSearch('title', 'a', 'embedding', [1, 0], 5, null, value) },
    { method: 'vectorCommand', arg: 'request', call: (c) => c.vectorCommand(value) },
    { method: 'vectorCommandAsync', arg: 'request', call: (c) => c.vectorCommandAsync(value) },
    { method: 'findNearest', arg: 'filter', call: (c) => c.findNearest('embedding', [1, 0], 5, value) },
    { method: 'findNearestF32', arg: 'filter', call: (c) => c.findNearestF32('embedding', vec, 5, value) },
    { method: 'findNearestAsync', arg: 'filter', call: (c) => c.findNearestAsync('embedding', vec, 5, value) },
    { method: 'findAsync', arg: 'filter', call: (c) => c.findAsync(value) },
    { method: 'insertAsync', arg: 'doc', call: (c) => c.insertAsync(value) },
    { method: 'insertManyAsync', arg: 'docs[0]', call: (c) => c.insertManyAsync([value]) },
    { method: 'updateOneAsync', arg: 'filter', call: (c) => c.updateOneAsync(value, { $set: { a: 1 } }) },
    { method: 'updateOneAsync', arg: 'update', call: (c) => c.updateOneAsync(f, value) },
    { method: 'updateManyAsync', arg: 'filter', call: (c) => c.updateManyAsync(value, { $set: { a: 1 } }) },
    { method: 'updateManyAsync', arg: 'update', call: (c) => c.updateManyAsync(f, value) },
    { method: 'deleteOneAsync', arg: 'filter', call: (c) => c.deleteOneAsync(value) },
    { method: 'deleteManyAsync', arg: 'filter', call: (c) => c.deleteManyAsync(value) },
  ]
}

module.exports = { nest, cyclic, methodCalls }
