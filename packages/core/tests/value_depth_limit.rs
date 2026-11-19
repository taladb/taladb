//! S2 (v0.13.2 M4): a deeply nested `Value` in stored bytes — a snapshot, a
//! legacy snapshot that R1 now migrates, or a tampered document body — must be
//! an ordinary error, never a stack overflow.
//!
//! A stack overflow aborts the process (wasm: an unrecoverable trap); it
//! cannot be caught. These tests therefore run the risky calls on a thread
//! with a deliberately small stack and with nesting far past the limit: if any
//! decode, skip or cleanup path recurses per level, the binary dies instead of
//! failing a single assertion.
//!
//! Wire encodings used to craft input (postcard, the storage format):
//! `Value` is an enum → variant index varint; `Null` = `00`, `Int` = `02`
//! + zigzag varint, `Array` = `06` + length varint + elements, `Object` = `07`
//! + length varint + `(String, Value)` pairs, strings = length varint + bytes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use taladb::aggregate::{Accumulator, GroupKey, Stage};
use taladb::document::{Document, Value};
use taladb::engine::{RedbBackend, StorageBackend};
use taladb::index::{META_VERSION_KEY, META_VERSION_TABLE};
use taladb::{Database, Filter, Ulid};

/// The nesting limit under test. Kept local so this file also builds against
/// the unfixed baseline; `limit_matches_the_published_constant` ties it to the
/// engine's constant once that exists.
const LIMIT: usize = 128;

/// Far past any limit: 1 M levels (~2–4 MB of input). The baseline overflowed
/// at ~2,500 on a 2 MiB test thread.
const ABSURD: usize = 1_000_000;

/// Small enough that per-level recursion over `ABSURD` levels cannot fit,
/// large enough for the engine's ordinary bounded work in a debug build.
const SMALL_STACK: usize = 512 * 1024;

fn on_small_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(f)
        .unwrap()
        .join()
        .expect("the guarded call panicked")
}

// ---------------------------------------------------------------------------
// Per-thread allocation accounting (peak live bytes on the measuring thread).
// ---------------------------------------------------------------------------

struct Counting;
thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}
fn track(delta: isize) {
    // `try_with`: allocation can happen while thread-locals are torn down.
    let _ = LIVE.try_with(|live| {
        let now = live.get() + delta;
        live.set(now);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
    });
}
// SAFETY: forwards every call to `System` unchanged; the bookkeeping only
// touches const-initialised thread-locals, which never allocate.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        track(layout.size() as isize);
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        track(-(layout.size() as isize));
        // SAFETY: `ptr` came from `System.alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOC: Counting = Counting;

/// Peak bytes allocated (net of frees) on this thread while `f` ran.
fn peak_alloc_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.with(Cell::get);
    PEAK.with(|p| p.set(base));
    let out = f();
    let peak = PEAK.with(Cell::get) - base;
    (out, usize::try_from(peak).unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Crafted encodings
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Kind {
    Arrays,
    Objects,
}
const KINDS: [Kind; 2] = [Kind::Arrays, Kind::Objects];

/// postcard bytes of one `Value` with `depth` nested containers around `Null`.
fn nested(kind: Kind, depth: usize) -> Vec<u8> {
    let level: &[u8] = match kind {
        Kind::Arrays => &[0x06, 0x01],
        Kind::Objects => &[0x07, 0x01, 0x01, b'k'],
    };
    let mut out = Vec::with_capacity(depth * level.len() + 1);
    for _ in 0..depth {
        out.extend_from_slice(level);
    }
    out.push(0x00);
    out
}

/// The same value, built as a `Value` (only for depths the test can afford).
fn nested_value(kind: Kind, depth: usize) -> Value {
    let mut v = Value::Null;
    for _ in 0..depth {
        v = match kind {
            Kind::Arrays => Value::Array(vec![v]),
            Kind::Objects => Value::Object(vec![("k".into(), v)]),
        };
    }
    v
}

/// postcard bytes of a document `{ other: 7, a: <raw value bytes> }`.
fn doc_bytes(id: Ulid, raw_a: &[u8]) -> Vec<u8> {
    let id = id.to_string();
    let mut out = vec![u8::try_from(id.len()).unwrap()];
    out.extend_from_slice(id.as_bytes());
    out.push(0x02); // two fields
    out.extend_from_slice(&[0x05, b'o', b't', b'h', b'e', b'r', 0x02, 0x0e]); // Int(7)
    out.extend_from_slice(&[0x01, b'a']);
    out.extend_from_slice(raw_a);
    out
}

type Tables = BTreeMap<String, Vec<(Vec<u8>, Vec<u8>)>>;

fn tables_of(backend: &dyn StorageBackend) -> Tables {
    let txn = backend.begin_read().unwrap();
    txn.list_tables()
        .unwrap()
        .into_iter()
        .map(|t| {
            let rows = txn.scan_all(&t).unwrap();
            (t, rows)
        })
        .collect()
}

/// Re-emit a snapshot in `export_snapshot`'s format.
fn snapshot_from(tables: &Tables) -> Vec<u8> {
    let mut out = b"TDBS".to_vec();
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(tables.len() as u32).to_le_bytes());
    for (name, rows) in tables {
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(rows.len() as u64).to_le_bytes());
        for (k, v) in rows {
            out.extend_from_slice(&(k.len() as u32).to_le_bytes());
            out.extend_from_slice(k);
            out.extend_from_slice(&(v.len() as u32).to_le_bytes());
            out.extend_from_slice(v);
        }
    }
    out
}

/// A database with two documents in `c`: `victim` and `bystander`. Returns
/// its tables with the victim's body replaced by `{other: 7, a: raw_a}`.
fn tables_with_victim(raw_a: &[u8], index_a: bool) -> (Tables, Ulid, Ulid) {
    let db = Database::open_in_memory().unwrap();
    let c = db.collection("c").unwrap();
    if index_a {
        c.create_index("a").unwrap();
    }
    let victim = c
        .insert(vec![
            ("other".into(), Value::Int(7)),
            ("a".into(), Value::Int(1)),
        ])
        .unwrap();
    let bystander = c
        .insert(vec![
            ("other".into(), Value::Int(8)),
            ("a".into(), Value::Int(2)),
        ])
        .unwrap();
    let mut tables = tables_of(db.backend());
    let docs = tables.get_mut("docs::c").unwrap();
    let row = docs
        .iter_mut()
        .find(|(k, _)| k.as_slice() == victim.to_bytes())
        .expect("victim row");
    row.1 = doc_bytes(victim, raw_a);
    (tables, victim, bystander)
}

/// Every read path that decodes documents of `c` reports an error.
fn assert_reads_fail_cleanly(db: &Database, victim: Ulid, bystander: Ulid) {
    let c = db.collection("c").unwrap();
    assert!(c.find(Filter::All).is_err(), "full decode");
    // `a` is not named by the filter: the projected decoder *skips* it.
    assert!(
        c.find(Filter::Eq("other".into(), Value::Int(7))).is_err(),
        "projected decode (skip path)"
    );
    assert!(c.count(Filter::Eq("other".into(), Value::Int(7))).is_err());
    assert!(c.find_by_id(victim).is_err(), "point read");
    // `$match → $group` takes the M2 projected-aggregate path.
    let pipeline = vec![
        Stage::Match(Filter::Eq("other".into(), Value::Int(7))),
        Stage::Group {
            key: GroupKey::Null,
            accumulators: vec![("n".into(), Accumulator::Count)],
        },
    ];
    assert!(c.aggregate(pipeline).is_err(), "projected aggregate");
    // The rest of the collection stays readable by id.
    let ok = c.find_by_id(bystander).unwrap().expect("bystander");
    assert_eq!(ok.get("other"), Some(&Value::Int(8)));
}

// ---------------------------------------------------------------------------
// Reproductions
// ---------------------------------------------------------------------------

#[test]
fn deep_value_in_current_snapshot_is_an_error_not_a_crash() {
    for kind in KINDS {
        on_small_stack(move || {
            let (tables, victim, bystander) = tables_with_victim(&nested(kind, ABSURD), false);
            // Restore copies bytes; with a current version record it decodes
            // nothing, so the value is first met by the reads below.
            let db = Database::restore_from_snapshot(&snapshot_from(&tables))
                .unwrap_or_else(|e| panic!("{kind:?}: {e}"));
            assert_reads_fail_cleanly(&db, victim, bystander);
        });
    }
}

/// M3 R1 made restore run migrations, so a v0/v1 snapshot with an indexed
/// collection decodes every document *during restore*. That must fail as an
/// error with no database returned.
#[test]
fn deep_value_in_legacy_snapshot_fails_restore_cleanly() {
    for kind in KINDS {
        for legacy_version in [None, Some(1u32)] {
            on_small_stack(move || {
                let (mut tables, _, _) = tables_with_victim(&nested(kind, ABSURD), true);
                match legacy_version {
                    None => {
                        tables.remove(META_VERSION_TABLE);
                    }
                    Some(v) => {
                        tables.insert(
                            META_VERSION_TABLE.into(),
                            vec![(
                                META_VERSION_KEY.to_vec(),
                                postcard::to_allocvec(&v).unwrap(),
                            )],
                        );
                    }
                }
                assert!(
                    Database::restore_from_snapshot(&snapshot_from(&tables)).is_err(),
                    "{kind:?} at {legacy_version:?}"
                );
            });
        }
    }
}

/// A tampered (or corrupted) document body in a database file.
#[test]
fn deep_value_in_stored_document_body_is_an_error_and_writes_nothing() {
    for kind in KINDS {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tampered.redb");
        let (victim, bystander) = {
            let db = Database::open(&path).unwrap();
            let c = db.collection("c").unwrap();
            let v = c
                .insert(vec![
                    ("other".into(), Value::Int(7)),
                    ("a".into(), Value::Int(1)),
                ])
                .unwrap();
            let b = c
                .insert(vec![
                    ("other".into(), Value::Int(8)),
                    ("a".into(), Value::Int(2)),
                ])
                .unwrap();
            db.close().unwrap();
            (v, b)
        };
        {
            let raw = RedbBackend::open(&path).unwrap();
            let mut w = raw.begin_write().unwrap();
            w.put(
                "docs::c",
                &victim.to_bytes(),
                &doc_bytes(victim, &nested(kind, ABSURD)),
            )
            .unwrap();
            w.commit().unwrap();
        }
        let before = tables_of(&RedbBackend::open(&path).unwrap());
        let path2 = path.clone();
        on_small_stack(move || {
            let db = Database::open(&path2).unwrap();
            assert_reads_fail_cleanly(&db, victim, bystander);
            db.close().unwrap();
        });
        assert_eq!(
            tables_of(&RedbBackend::open(&path).unwrap()),
            before,
            "{kind:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The limit itself
// ---------------------------------------------------------------------------

#[test]
fn crafted_nesting_at_the_limit_reads_and_one_past_is_refused() {
    for kind in KINDS {
        for (depth, ok) in [(LIMIT, true), (LIMIT + 1, false)] {
            let (tables, victim, _) = tables_with_victim(&nested(kind, depth), false);
            let db = Database::restore_from_snapshot(&snapshot_from(&tables)).unwrap();
            let got = db.collection("c").unwrap().find_by_id(victim);
            assert_eq!(got.is_ok(), ok, "{kind:?} depth {depth}: {got:?}");
            if ok {
                let doc = got.unwrap().unwrap();
                assert_eq!(doc.get("a"), Some(&nested_value(kind, depth)));
            }
        }
    }
}

/// Nothing may be stored that the reader would then refuse.
#[test]
fn writes_at_the_limit_round_trip_and_one_past_is_refused() {
    for kind in KINDS {
        let db = Database::open_in_memory().unwrap();
        let c = db.collection("c").unwrap();
        let id = c
            .insert(vec![("a".into(), nested_value(kind, LIMIT))])
            .unwrap();
        assert_eq!(
            c.find_by_id(id).unwrap().unwrap().get("a"),
            Some(&nested_value(kind, LIMIT))
        );

        let err = c
            .insert(vec![("a".into(), nested_value(kind, LIMIT + 1))])
            .expect_err("a value the reader would refuse must not be stored");
        assert!(err.to_string().contains("nest"), "{kind:?}: {err}");
        assert_eq!(
            c.count(Filter::All).unwrap(),
            1,
            "the failed insert wrote nothing"
        );

        let update = taladb::Update::Set(vec![("a".into(), nested_value(kind, LIMIT + 1))]);
        assert!(c.update_one(Filter::All, update).is_err());
        assert_eq!(
            c.find_by_id(id).unwrap().unwrap().get("a"),
            Some(&nested_value(kind, LIMIT)),
            "the failed update changed nothing"
        );
    }
}

// ---------------------------------------------------------------------------
// Compatibility: the wire format is unchanged for every value
// ---------------------------------------------------------------------------

/// A copy of `Value` with the derived impls the engine used up to 0.13.1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Value")]
enum Shadow {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

fn shadow(v: &Value) -> Shadow {
    match v {
        Value::Null => Shadow::Null,
        Value::Bool(b) => Shadow::Bool(*b),
        Value::Int(n) => Shadow::Int(*n),
        Value::Float(f) => Shadow::Float(*f),
        Value::Str(s) => Shadow::Str(s.clone()),
        Value::Bytes(b) => Shadow::Bytes(b.clone()),
        Value::Array(items) => Shadow::Array(items.iter().map(shadow).collect()),
        Value::Object(fields) => {
            Shadow::Object(fields.iter().map(|(k, v)| (k.clone(), shadow(v))).collect())
        }
    }
}

fn corpus() -> Vec<Value> {
    let ordinary = Value::Object(vec![
        ("name".into(), Value::Str("Noli Me Tángere".into())),
        ("year".into(), Value::Int(1887)),
        ("rating".into(), Value::Float(4.5)),
        (
            "tags".into(),
            Value::Array(vec![Value::Str("classic".into()), Value::Null]),
        ),
        (
            "author".into(),
            Value::Object(vec![
                ("name".into(), Value::Str("Rizal".into())),
                (
                    "born".into(),
                    Value::Object(vec![("year".into(), Value::Int(1861))]),
                ),
            ]),
        ),
        ("cover".into(), Value::Bytes(vec![0, 1, 2, 255])),
        ("ok".into(), Value::Bool(true)),
        ("empty_a".into(), Value::Array(vec![])),
        ("empty_o".into(), Value::Object(vec![])),
    ]);
    let mut out = vec![
        Value::Null,
        Value::Bool(false),
        Value::Int(i64::MIN),
        Value::Int(-1),
        Value::Int(i64::MAX),
        Value::Float(f64::NAN),
        Value::Float(-0.0),
        Value::Str(String::new()),
        Value::Str("ü€😀".into()),
        Value::Bytes(vec![]),
        Value::Array((0..300).map(Value::Int).collect()), // multi-byte length
        ordinary,
    ];
    for kind in KINDS {
        out.push(nested_value(kind, 10));
        out.push(nested_value(kind, LIMIT));
    }
    out
}

fn depth(v: &Value) -> usize {
    match v {
        Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        Value::Object(fields) => 1 + fields.iter().map(|(_, v)| depth(v)).max().unwrap_or(0),
        _ => 0,
    }
}

fn same(a: &Value, b: &Value) -> bool {
    // NaN != NaN; compare encodings instead of values.
    postcard::to_allocvec(a).unwrap() == postcard::to_allocvec(b).unwrap()
}

#[test]
fn ordinary_values_encode_and_decode_exactly_as_before() {
    for v in corpus() {
        let old = shadow(&v);
        // postcard (storage) — both directions.
        let new_bytes = postcard::to_allocvec(&v).unwrap();
        assert_eq!(new_bytes, postcard::to_allocvec(&old).unwrap(), "{v:?}");
        let back: Value = postcard::from_bytes(&new_bytes).unwrap();
        assert!(same(&back, &v), "{v:?}");
        // A self-describing format, to cover the identifier-by-name path.
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, serde_json::to_string(&old).unwrap());
        // Read back only what JSON itself can carry, with either impl: it has
        // no NaN (serde_json writes `null`), and serde_json's parser stops at
        // 128 levels — two per `Value` level in this externally tagged form.
        if !matches!(v, Value::Float(f) if !f.is_finite()) && depth(&v) < 64 {
            let back: Value = serde_json::from_str(&json).unwrap();
            assert!(same(&back, &v), "{json}");
        }
    }
    // Documents built from the corpus round-trip through storage.
    let db = Database::open_in_memory().unwrap();
    let c = db.collection("c").unwrap();
    for v in corpus() {
        let id = c.insert(vec![("v".into(), v.clone())]).unwrap();
        let got = c.find_by_id(id).unwrap().unwrap();
        assert!(same(got.get("v").unwrap(), &v));
    }
}

#[test]
fn stored_documents_from_the_previous_encoder_still_decode() {
    // Bytes produced by the derived impl (via the shadow) decode as `Document`.
    #[derive(Serialize)]
    struct OldDoc {
        id: Ulid,
        fields: Vec<(String, Shadow)>,
    }
    for v in corpus() {
        let old = OldDoc {
            id: Ulid::new(),
            fields: vec![("v".into(), shadow(&v))],
        };
        let bytes = postcard::to_allocvec(&old).unwrap();
        let doc: Document = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(doc.id, old.id);
        assert!(same(doc.get("v").unwrap(), &v));
    }
}

// ---------------------------------------------------------------------------
// Memory and runtime on malformed input
// ---------------------------------------------------------------------------

/// An array that claims 2^32 − 1 elements in 7 bytes must not reserve memory
/// for them.
#[test]
fn huge_declared_length_is_refused_without_a_huge_allocation() {
    for kind_tag in [0x06u8, 0x07] {
        let bytes = [kind_tag, 0xff, 0xff, 0xff, 0xff, 0x0f, 0x00];
        let (res, peak) = peak_alloc_during(|| postcard::from_bytes::<Value>(&bytes));
        assert!(res.is_err());
        assert!(
            peak < 2 * 1024 * 1024,
            "peak {peak} bytes for tag {kind_tag:#x}"
        );
    }
}

/// Rejecting 1 M levels costs bounded time and memory: decoding stops at the
/// limit instead of walking (or allocating for) the rest of the input.
#[test]
fn rejecting_absurd_nesting_is_fast_and_small() {
    for kind in KINDS {
        let raw = nested(kind, ABSURD);
        let doc = doc_bytes(Ulid::new(), &raw);
        on_small_stack(move || {
            let start = Instant::now();
            let (res, peak) = peak_alloc_during(|| postcard::from_bytes::<Document>(&doc));
            let took = start.elapsed();
            assert!(res.is_err(), "{kind:?}");
            assert!(took < Duration::from_millis(500), "{kind:?} took {took:?}");
            assert!(peak < 1024 * 1024, "{kind:?} peak {peak} bytes");
        });
    }
}

/// Headroom for the limit: the deepest value accepted survives every engine
/// path that walks it — write, index maintenance, full and projected decode,
/// equality on the nested value itself, snapshot export/restore and JSON
/// conversion for the bindings — on the same small stack.
#[test]
fn deepest_allowed_value_works_end_to_end_on_a_small_stack() {
    for kind in KINDS {
        on_small_stack(move || {
            let db = Database::open_in_memory().unwrap();
            let c = db.collection("c").unwrap();
            c.create_index("a").unwrap();
            let v = nested_value(kind, LIMIT);
            let id = c
                .insert(vec![
                    ("a".into(), v.clone()),
                    ("other".into(), Value::Int(7)),
                ])
                .unwrap();
            assert_eq!(c.find(Filter::All).unwrap().len(), 1);
            assert_eq!(c.find(Filter::Eq("a".into(), v.clone())).unwrap().len(), 1);
            assert_eq!(
                c.find(Filter::Eq("other".into(), Value::Int(7)))
                    .unwrap()
                    .len(),
                1
            );
            let doc = c.find_by_id(id).unwrap().unwrap();
            let json = taladb::json::document_to_json(&doc);
            assert_eq!(taladb::json::to_value(&json["a"]), v);
            let restored = Database::restore_from_snapshot(&db.export_snapshot().unwrap()).unwrap();
            let back = restored
                .collection("c")
                .unwrap()
                .find_by_id(id)
                .unwrap()
                .unwrap();
            assert_eq!(back.get("a"), Some(&v));
        });
    }
}

#[test]
fn limit_matches_the_published_constant() {
    assert_eq!(LIMIT, taladb::document::MAX_VALUE_DEPTH);
}
