//! v0.13.2 M3 / R1: `restore_from_snapshot` must bring a restored database to
//! the current storage schema exactly as opening a file does.
//!
//! Before the fix, restore migrated the *empty* in-memory database first and
//! then copied the snapshot's tables (including `meta::db_version`) over it, so
//! no built-in migration ever ran against restored data:
//! - a v1 snapshot kept version 1, the retired `tomb::` table and stale index
//!   keys (an index-served `count` returned 0 where the file path returned 1);
//! - a v0 snapshot (no version key) was labelled v2 without being migrated.
//!
//! The IndexedDB fallback (`WorkerDB.openWithSnapshot`) and backup import both
//! go through `restore_from_snapshot`, so this is their regression suite too.
use std::ops::Bound;

use taladb::{Database, Filter, TalaDbError, Value};

const META_VERSION_TABLE: &str = "meta::db_version";
const META_VERSION_KEY: &[u8] = b"version";
const INDEX_TABLE: &str = "idx::c::k";

fn version_of(db: &Database) -> Option<Vec<u8>> {
    let r = db.backend().begin_read().unwrap();
    r.get(META_VERSION_TABLE, META_VERSION_KEY).unwrap()
}

fn has_tomb_table(db: &Database) -> bool {
    let r = db.backend().begin_read().unwrap();
    r.list_tables()
        .unwrap()
        .iter()
        .any(|t| t.starts_with("tomb::"))
}

fn index_entries(db: &Database) -> usize {
    let r = db.backend().begin_read().unwrap();
    r.range(INDEX_TABLE, Bound::Unbounded, Bound::Unbounded)
        .unwrap()
        .len()
}

fn indexed_count(db: &Database, k: i64) -> u64 {
    db.collection("c")
        .unwrap()
        .count(Filter::Eq("k".into(), Value::Int(k)))
        .unwrap()
}

/// Populate one indexed collection: two docs with `k = 7`, one with `k = 8`.
fn populate(db: &Database) {
    let col = db.collection("c").unwrap();
    col.create_index("k").unwrap();
    for k in [7, 7, 8] {
        col.insert(vec![("k".into(), Value::Int(k))]).unwrap();
    }
}

/// Rewind a current database to what an older release left on disk: the given
/// version key (`None` = never written, i.e. v0), a retired `tomb::` table
/// (dropped by v1 → v2), and an index whose keys are missing (both built-in
/// migrations re-derive every secondary index from the docs table).
fn rewind_to(db: &Database, version: Option<u32>) {
    let mut w = db.backend().begin_write().unwrap();
    match version {
        Some(v) => {
            let bytes = postcard_u32(v);
            w.put(META_VERSION_TABLE, META_VERSION_KEY, &bytes).unwrap();
        }
        None => {
            w.delete(META_VERSION_TABLE, META_VERSION_KEY).unwrap();
        }
    }
    w.put("tomb::c", b"x", b"y").unwrap();
    let keys = w
        .range(INDEX_TABLE, Bound::Unbounded, Bound::Unbounded)
        .unwrap();
    assert!(
        !keys.is_empty(),
        "fixture: the index must have entries to wipe"
    );
    for (k, _) in keys {
        w.delete(INDEX_TABLE, &k).unwrap();
    }
    w.commit().unwrap();
}

/// postcard encodes a `u32` as an LEB128 varint.
fn postcard_u32(mut v: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn legacy_snapshot(version: Option<u32>) -> Vec<u8> {
    let db = Database::open_in_memory().unwrap();
    populate(&db);
    rewind_to(&db, version);
    db.export_snapshot().unwrap()
}

/// The reference behaviour: the same legacy state reopened from a file.
fn legacy_file_reopen(version: Option<u32>) -> (tempfile::TempDir, Database) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    {
        let db = Database::open(&path).unwrap();
        populate(&db);
        rewind_to(&db, version);
    }
    let db = Database::open(&path).unwrap();
    (dir, db)
}

#[test]
fn v1_snapshot_is_migrated_like_a_v1_file() {
    let restored = Database::restore_from_snapshot(&legacy_snapshot(Some(1))).unwrap();
    let (_dir, file) = legacy_file_reopen(Some(1));

    for (what, db) in [("restore", &restored), ("file", &file)] {
        assert_eq!(version_of(db), Some(vec![2]), "{what}: schema version");
        assert!(!has_tomb_table(db), "{what}: v1 -> v2 drops tomb:: tables");
        assert_eq!(index_entries(db), 3, "{what}: index rebuilt from docs");
        assert_eq!(indexed_count(db, 7), 2, "{what}: index-served count");
        assert_eq!(indexed_count(db, 8), 1, "{what}: index-served count");
    }
}

#[test]
fn v0_snapshot_without_a_version_key_runs_the_whole_chain() {
    let snapshot = legacy_snapshot(None);
    let restored = Database::restore_from_snapshot(&snapshot).unwrap();
    let (_dir, file) = legacy_file_reopen(None);

    for (what, db) in [("restore", &restored), ("file", &file)] {
        assert_eq!(version_of(db), Some(vec![2]), "{what}: schema version");
        assert!(!has_tomb_table(db), "{what}");
        assert_eq!(index_entries(db), 3, "{what}");
        assert_eq!(indexed_count(db, 7), 2, "{what}");
    }
}

/// Decode a snapshot into its non-empty tables' entries. Restore has never
/// recreated a table with zero entries (and readers treat a missing table as
/// empty), so the comparison is over entries, not raw bytes.
fn entries(snapshot: &[u8]) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    fn take<'a>(buf: &'a [u8], at: &mut usize, n: usize) -> &'a [u8] {
        let out = &buf[*at..*at + n];
        *at += n;
        out
    }
    fn u32_at(buf: &[u8], at: &mut usize) -> usize {
        u32::from_le_bytes(take(buf, at, 4).try_into().unwrap()) as usize
    }
    let mut at = 8;
    let mut out = Vec::new();
    for _ in 0..u32_at(snapshot, &mut at) {
        let len = u32_at(snapshot, &mut at);
        let name = String::from_utf8(take(snapshot, &mut at, len).to_vec()).unwrap();
        let count = u64::from_le_bytes(take(snapshot, &mut at, 8).try_into().unwrap());
        for _ in 0..count {
            let klen = u32_at(snapshot, &mut at);
            let key = take(snapshot, &mut at, klen).to_vec();
            let vlen = u32_at(snapshot, &mut at);
            let val = take(snapshot, &mut at, vlen).to_vec();
            out.push((name.clone(), key, val));
        }
    }
    assert_eq!(at, snapshot.len());
    out
}

/// A current snapshot must restore unchanged: no migration runs, so no entry
/// may change, and a second cycle is byte-identical to the first. This pins
/// snapshot-format compatibility.
#[test]
fn current_snapshot_restores_unchanged() {
    let db = Database::open_in_memory().unwrap();
    populate(&db);
    db.set_user_version(5).unwrap();
    let col = db.collection("c").unwrap();
    let doomed = col.insert(vec![("k".into(), Value::Int(9))]).unwrap();
    col.delete_by_id(doomed).unwrap();
    let first = db.export_snapshot().unwrap();

    let restored = Database::restore_from_snapshot(&first).unwrap();
    assert_eq!(version_of(&restored), Some(vec![2]));
    assert_eq!(restored.user_version().unwrap(), 5, "app version untouched");
    assert_eq!(indexed_count(&restored, 7), 2);
    assert_eq!(indexed_count(&restored, 9), 0, "deleted doc stays deleted");
    let second = restored.export_snapshot().unwrap();
    assert_eq!(entries(&first), entries(&second), "no entry may change");
    let third = Database::restore_from_snapshot(&second)
        .unwrap()
        .export_snapshot()
        .unwrap();
    assert_eq!(second, third, "restore is a fixed point after one cycle");
}

/// `open_with_migrations` advances the same version key past the built-in
/// chain, so a version above `CURRENT_SCHEMA_VERSION` is legitimate (a Rust
/// caller's own migration) and indistinguishable from a newer engine. Restore
/// must treat it exactly as file open does: run no built-in migration and keep
/// the version as stored.
#[test]
fn version_beyond_the_builtin_chain_is_preserved_like_file_open() {
    let db = Database::open_in_memory().unwrap();
    populate(&db);
    {
        let mut w = db.backend().begin_write().unwrap();
        w.put(META_VERSION_TABLE, META_VERSION_KEY, &[3]).unwrap();
        w.put("tomb::c", b"x", b"y").unwrap();
        w.commit().unwrap();
    }
    let restored = Database::restore_from_snapshot(&db.export_snapshot().unwrap()).unwrap();
    assert_eq!(version_of(&restored), Some(vec![3]));
    assert!(
        has_tomb_table(&restored),
        "no built-in migration may run on a version past the chain"
    );
    assert_eq!(indexed_count(&restored, 7), 2);
}

/// An undecodable version key is an error, not a guess.
#[test]
fn undecodable_version_key_is_rejected() {
    let db = Database::open_in_memory().unwrap();
    populate(&db);
    {
        let mut w = db.backend().begin_write().unwrap();
        // An unterminated varint: postcard cannot decode it as a u32.
        w.put(META_VERSION_TABLE, META_VERSION_KEY, &[0xff])
            .unwrap();
        w.commit().unwrap();
    }
    let Err(err) = Database::restore_from_snapshot(&db.export_snapshot().unwrap()) else {
        panic!("an undecodable version key must not restore");
    };
    assert!(
        matches!(err, TalaDbError::Postcard(_)),
        "expected a decode error, got {err:?}"
    );
}

/// A migration that fails part-way must not yield a database: restore either
/// returns a fully migrated database or an error, never a half-migrated one.
#[test]
fn failed_migration_returns_an_error_not_a_partial_database() {
    let db = Database::open_in_memory().unwrap();
    populate(&db);
    rewind_to(&db, Some(1));
    {
        // A document body the index rebuild cannot decode.
        let mut w = db.backend().begin_write().unwrap();
        w.put("docs::c", &[0u8; 16], &[0xff, 0xff, 0xff]).unwrap();
        w.commit().unwrap();
    }
    let snapshot = db.export_snapshot().unwrap();
    assert!(Database::restore_from_snapshot(&snapshot).is_err());
}

/// The restored database must stay writable and keep its indexes coherent
/// after migration: new writes land in the rebuilt index and are found.
#[test]
fn migrated_restore_accepts_writes_and_keeps_indexes_coherent() {
    let restored = Database::restore_from_snapshot(&legacy_snapshot(Some(1))).unwrap();
    let col = restored.collection("c").unwrap();
    col.insert(vec![("k".into(), Value::Int(7))]).unwrap();
    col.delete_many(Filter::Eq("k".into(), Value::Int(8)))
        .unwrap();
    assert_eq!(indexed_count(&restored, 7), 3);
    assert_eq!(indexed_count(&restored, 8), 0);
    assert_eq!(index_entries(&restored), 3);
    // And the migrated state itself survives another export/restore cycle.
    let again = Database::restore_from_snapshot(&restored.export_snapshot().unwrap()).unwrap();
    assert_eq!(indexed_count(&again, 7), 3);
    assert_eq!(version_of(&again), Some(vec![2]));
}
