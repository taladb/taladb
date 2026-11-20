// Drives __TalaDB__ exactly as the RN clients do (global HostObject methods).
var __failures = 0;
var __done = false;
var pending = [];
function ok(name, cond, extra) {
  if (cond) print('PASS', name);
  else { __failures++; print('FAIL', name, extra === undefined ? '' : String(extra)); }
}
function throws(name, fn, re) {
  try { var r = fn(); __failures++; print('FAIL', name, '(no throw, returned ' + JSON.stringify(r) + ')'); }
  catch (e) {
    var m = String(e && e.message !== undefined ? e.message : e);
    if (re && !re.test(m)) { __failures++; print('FAIL', name, 'wrong error: ' + m); }
    else print('PASS', name, '->', m.slice(0, 140));
  }
}
function observe(name, fn) {
  try { var r = fn(); print('OBSERVE', name, '-> ok', r === undefined ? '' : JSON.stringify(r)); return true; }
  catch (e) { print('OBSERVE', name, '-> threw:', String(e.message || e).slice(0, 160)); return false; }
}
var T = function () { return globalThis.__TalaDB__; };
var PW = JSON.stringify({ passphrase: 'correct horse battery staple' });
var BAD = JSON.stringify({ passphrase: 'wrong passphrase' });

if (__suite === 'smoke') {
  var p1 = __dir + '/plain.redb', p2 = __dir + '/enc.redb';
  // --- plain open / insert / query ---
  __open(p1, null);
  var id = T().insert('c', { n: 1, s: 'a' });
  ok('insert returns an id', typeof id === 'string' && id.length === 26, id);
  T().insertMany('c', [{ n: 2, s: 'b' }, { n: 3, s: 'c' }]);
  ok('find all', T().find('c', {}).length === 3);
  ok('find filter', T().find('c', { n: { $gte: 2 } }).length === 2);
  ok('findOne', T().findOne('c', { _id: id }).s === 'a');
  ok('count', T().count('c', {}) === 3);
  ok('updateOne', T().updateOne('c', { n: 1 }, { $set: { s: 'z' } }) === true);
  ok('deleteOne', T().deleteOne('c', { n: 3 }) === true);
  T().createIndex('c', 'n');
  ok('indexed find', T().find('c', { n: 2 }).length === 1);
  T().setUserVersion(7);
  ok('userVersion', T().userVersion() === 7);
  // async paths (worker thread + setTimeout polling)
  pending.push(T().callAsync('find', ['c', {}]).then(function (r) { ok('callAsync find', r.length === 2, JSON.stringify(r)); }));
  pending.push(T().findAsync('c', { n: 2 }).then(function (r) { ok('findAsync', r.length === 1); }));
  Promise.all(pending).then(function () {
    // --- host close, stale use, reopen ---
    var host = T();
    host.close();
    throws('method after close() errors instead of crashing', function () { return host.find('c', {}); }, /taladb_find failed/);
    __open(p1, null);
    ok('reopen after close() sees data', T().find('c', {}).length === 2);
    ok('reopen keeps index + userVersion', T().find('c', { n: 2 }).length === 1 && T().userVersion() === 7);

    // --- module close (TalaDBModule.close) then immediate reopen ---
    __moduleClose();
    var reopened = observe('TalaDBModule.close() then immediate reopen of the same file', function () { __open(p1, null); });
    if (__oldGlue) print('NOTE old glue: reopen ' + (reopened ? 'succeeded' : 'FAILED (file still locked until GC)'));
    else ok('module close releases the file (reopen succeeds)', reopened);
    if (!reopened) { __gc(); __gc(); observe('reopen after forced GC', function () { __open(p1, null); }); }
    __moduleClose();

    // --- encryption ---
    __open(p2, PW);
    T().insert('secret', { k: 'v' });
    ok('encrypted insert/find', T().find('secret', {}).length === 1);
    __moduleClose();
    throws('plain open of encrypted db is refused', function () { __open(p2, null); }, /encrypt/i);
    throws('wrong passphrase is refused', function () { __open(p2, BAD); }, /passphrase|encrypt/i);
    __open(p2, PW);
    ok('correct passphrase after refusals opens with data intact', T().find('secret', {}).length === 1 && T().find('secret', {})[0].k === 'v');
    __moduleClose();
    var saltBefore = __exists(p1 + '.taladb-salt');
    throws('passphrase open of plaintext db is refused', function () { __open(p1, PW); }, /not encrypted/i);
    ok('no salt sidecar invented for the plaintext db', !saltBefore && !__exists(p1 + '.taladb-salt'));
    __open(p1, null);
    ok('plaintext db still opens plainly after refused passphrase open', T().find('c', {}).length === 2);
    __moduleClose();
    __done = true;
  }).catch(function (e) { __failures++; print('FAIL async', String(e && e.message || e)); __done = true; });
}

if (__suite === 'k1') {
  // A method kept after the HostObject itself became unreachable. Setup runs
  // in its own frame so no register of the top-level code still holds the host.
  var path = __dir + '/k1.redb';
  function setup() {
    __open(path, null);
    var h = globalThis.__TalaDB__;
    h.insert('c', { n: 1 });
    var fns = [h.find, h.close];
    globalThis.__TalaDB__ = undefined;
    return fns;
  }
  var fns = setup();
  __gc(); __gc(); __gc();
  // If the HostObject was finalized, its destructor closed the file, so a
  // second open of the same path succeeds (old glue). While retained methods
  // keep it alive, the file stays locked (fixed glue).
  var finalized = observe('second open of the same file after GC', function () {
    var c = globalThis.__TalaDB__; globalThis.__TalaDB__ = undefined; __open(path, null); globalThis.__TalaDB__.close(); globalThis.__TalaDB__ = undefined;
  });
  print('NOTE host object ' + (finalized ? 'WAS finalized while a method was retained' : 'kept alive by the retained method'));
  if (!__oldGlue) ok('retained method keeps the HostObject (and its database) alive', !finalized);
  ok('retained find() still works after the HostObject is unreachable', fns[0]('c', {}).length === 1);
  fns[1]();
  throws('retained find() after close errors cleanly', function () { return fns[0]('c', {}); }, /failed/);
  fns = undefined;
  __gc(); __gc();
  __done = true;
}

if (__suite === 'k2') {
  __open(__dir + '/k2.redb', null);
  T().insert('c', { n: 1, v: [0.1, 0.2] });
  var bad = [NaN, -1, Infinity, -Infinity, 1e20, 2e19];
  bad.forEach(function (k) {
    throws('findNearest topK=' + k, function () { return T().findNearest('c', 'v', [0.1, 0.2], k); }, /topK/);
    throws('searchText topK=' + k, function () { return T().searchText('c', 's', 'x', k); }, /topK/);
  });
  throws('createVectorIndex dims=NaN', function () { return T().createVectorIndex('c', 'v', NaN); }, /dimensions/);
  throws('createVectorIndex dims=-5', function () { return T().createVectorIndex('c', 'v', -5); }, /dimensions/);
  [NaN, -1, 4294967296, 1e300, Infinity].forEach(function (v) {
    throws('setUserVersion(' + v + ')', function () { return T().setUserVersion(v); }, /version/);
  });
  throws('setUserVersion("3") (not a number)', function () { return T().setUserVersion('3'); }, /version must be a number/);
  throws('findNearest query value 1e39 (beyond float)', function () { return T().findNearest('c', 'v', [1e39, 0], 1); }, /32-bit float/);
  // In-range values keep working, fractions truncate as before.
  T().setUserVersion(4294967295);
  ok('setUserVersion(2^32-1) accepted', T().userVersion() === 4294967295);
  T().setUserVersion(3.9);
  ok('setUserVersion(3.9) truncates to 3 as before', T().userVersion() === 3);
  T().createVectorIndex('c', 'v', 2);
  ok('findNearest topK=1 works', T().findNearest('c', 'v', [0.1, 0.2], 1).length === 1);
  ok('findNearest topK=1.7 truncates to 1', T().findNearest('c', 'v', [0.1, 0.2], 1.7).length === 1);
  ok('findNearest topK=0 returns nothing', T().findNearest('c', 'v', [0.1, 0.2], 0).length === 0);
  __moduleClose();
  __done = true;
}

if (__suite === 'deep') {
  __open(__dir + '/deep.redb', null);
  [100, 10000, 100000].forEach(function (d) {
    var o = {}, root = o;
    for (var i = 0; i < d; i++) { o.a = {}; o = o.a; }
    throws('insert of a ' + d + '-level object is refused, not a crash', function () { return T().insert('c', root); });
  });
  ok('database still usable after deep inputs', T().count('c', {}) === 0);
  __moduleClose();
  __done = true;
}
