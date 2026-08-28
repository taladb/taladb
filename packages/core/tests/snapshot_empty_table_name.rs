//! A crafted snapshot with an empty table name must be refused, not panic.
//!
//! Found by `fuzz_snapshot` the first time it was ever able to run: the target
//! had existed for a while, but the fuzz crate was misconfigured as part of the
//! parent workspace, so every `cargo fuzz` invocation failed before reaching it.
//!
//! `restore_from_snapshot` read the table name straight out of the buffer and
//! handed it to redb, whose `TableDefinition::new` documents "name must not be
//! empty" and enforces it with an `assert!`. An empty name therefore aborted the
//! process instead of returning `Err` — in a function whose whole purpose is to
//! take untrusted bytes from disk or the network.

use taladb_core::{Database, TalaDbError};

/// The exact 48 bytes libFuzzer minimised to, kept verbatim so the test fails
/// again if the guard is removed.
const CRASHING_SNAPSHOT: &[u8] = &[
    // "TDBS" magic, version 1
    0x54, 0x44, 0x42, 0x53, 0x01, 0x00, 0x00, 0x00, //
    // table_count — reads as a huge number, which is fine: the stream runs out
    // long before it matters.
    0x52, 0x42, 0x44, 0x54, //
    // name_len = 0  <- the bug
    0x00, 0x00, 0x00, 0x00, //
    // entry_count = 17
    0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    // trailing bytes
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, //
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20,
];

#[test]
fn an_empty_table_name_is_an_error_not_a_panic() {
    match Database::restore_from_snapshot(CRASHING_SNAPSHOT) {
        Err(TalaDbError::InvalidSnapshot) => {}
        Err(other) => panic!("expected InvalidSnapshot, got {other:?}"),
        Ok(_) => panic!("a snapshot with an empty table name must not restore"),
    }
}

/// The same thing built deliberately rather than by a fuzzer, so the intent is
/// legible even if the bytes above ever stop being meaningful.
#[test]
fn a_hand_built_empty_name_is_refused_too() {
    let mut snap = Vec::new();
    snap.extend_from_slice(b"TDBS");
    snap.extend_from_slice(&1u32.to_le_bytes()); // version
    snap.extend_from_slice(&1u32.to_le_bytes()); // one table
    snap.extend_from_slice(&0u32.to_le_bytes()); // name_len = 0
    snap.extend_from_slice(&0u64.to_le_bytes()); // no entries

    assert!(
        matches!(
            Database::restore_from_snapshot(&snap),
            Err(TalaDbError::InvalidSnapshot)
        ),
        "an empty table name must be refused even with no entries behind it"
    );
}

/// The guard must not have made ordinary snapshots unrestorable. Internal table
/// names contain `::` and are not valid collection names, so anything stricter
/// than "non-empty" here would break this round trip.
#[test]
fn a_real_snapshot_still_round_trips() {
    let db = Database::open_in_memory().expect("open");
    let col = db.collection("books").expect("collection");
    col.insert(vec![
        ("title".to_string(), taladb_core::Value::Str("Dune".into())),
        ("year".to_string(), taladb_core::Value::Int(1965)),
    ])
    .expect("insert");
    col.create_index("year").expect("index");

    let snap = db.export_snapshot().expect("export");
    let restored = Database::restore_from_snapshot(&snap).expect("a real snapshot must restore");

    let found = restored
        .collection("books")
        .expect("collection")
        .find(taladb_core::Filter::All)
        .expect("find");
    assert_eq!(found.len(), 1, "the restored database should hold the row");
}
