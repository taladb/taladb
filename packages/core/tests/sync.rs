use taladb_core::Database;
use taladb_core::document::{Document, Value};
use taladb_core::query::filter::Filter;
use taladb_core::sync::{Change, ChangeOp, Changeset, LastWriteWins, SyncAdapter, stamp};
use ulid::Ulid;

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}
fn i(n: i64) -> Value {
    Value::Int(n)
}

// ---------------------------------------------------------------------------
// stamp() helper
// ---------------------------------------------------------------------------

#[test]
fn stamp_adds_changed_at() {
    let mut fields = vec![("name".into(), s("Alice"))];
    stamp(&mut fields);
    let has_changed_at = fields.iter().any(|(k, _)| k == "_changed_at");
    assert!(has_changed_at, "stamp() must add _changed_at field");
}

#[test]
fn stamp_replaces_existing_changed_at() {
    let mut fields = vec![("name".into(), s("Alice")), ("_changed_at".into(), i(1000))];
    stamp(&mut fields);

    // Must have exactly one _changed_at
    let count = fields.iter().filter(|(k, _)| k == "_changed_at").count();
    assert_eq!(
        count, 1,
        "stamp() must replace existing _changed_at, not duplicate it"
    );

    // New value must be >= 1000
    let ts = fields
        .iter()
        .find(|(k, _)| k == "_changed_at")
        .and_then(|(_, v)| {
            if let Value::Int(n) = v {
                Some(*n)
            } else {
                None
            }
        })
        .unwrap();
    assert!(ts >= 1000, "_changed_at should be >= old value");
}

// ---------------------------------------------------------------------------
// export_changes
// ---------------------------------------------------------------------------

#[test]
fn export_changes_returns_docs_after_since_ms() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("tasks").unwrap();
    let adapter = LastWriteWins::new();

    // Insert with known timestamps using insert_with_id so auto-stamping
    // does not overwrite the controlled _changed_at values.
    let old_doc = Document::new(vec![
        ("title".into(), s("old task")),
        ("_changed_at".into(), i(1000)),
    ]);
    col.insert_with_id(old_doc).unwrap();

    let new_doc = Document::new(vec![
        ("title".into(), s("new task")),
        ("_changed_at".into(), i(9000)),
    ]);
    col.insert_with_id(new_doc).unwrap();

    let changes = adapter.export_changes(&db, &["tasks"], 5000).unwrap();
    assert_eq!(
        changes.len(),
        1,
        "only doc with _changed_at > since_ms should be exported"
    );
    assert_eq!(
        changes[0].op.upsert_title(),
        Some("new task"),
        "exported change should be the newer doc"
    );
}

#[test]
fn export_changes_empty_collection_returns_empty() {
    let db = Database::open_in_memory().unwrap();
    let adapter = LastWriteWins::new();
    let changes = adapter.export_changes(&db, &["empty"], 0).unwrap();
    assert!(changes.is_empty());
}

#[test]
fn export_changes_since_zero_returns_all() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("items").unwrap();
    let adapter = LastWriteWins::new();

    for n in 0..5 {
        let mut fields = vec![("n".into(), i(n))];
        stamp(&mut fields);
        col.insert(fields).unwrap();
    }

    let changes = adapter.export_changes(&db, &["items"], 0).unwrap();
    assert_eq!(changes.len(), 5);
}

// ---------------------------------------------------------------------------
// import_changes — basic upsert
// ---------------------------------------------------------------------------

#[test]
fn import_changes_inserts_new_document() {
    let db = Database::open_in_memory().unwrap();
    let adapter = LastWriteWins::new();

    let remote_doc = taladb_core::document::Document::new(vec![
        ("title".into(), s("remote doc")),
        ("_changed_at".into(), i(5000)),
    ]);
    let remote_id = remote_doc.id;

    let changeset: Changeset = vec![Change {
        collection: "tasks".into(),
        id: remote_id,
        op: ChangeOp::Upsert(remote_doc),
        changed_at: 5000,
    }];

    let applied = adapter.import_changes(&db, changeset).unwrap();
    assert_eq!(applied, 1);

    let col = db.collection("tasks").unwrap();
    let results = col.find(Filter::All).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].get("title"), Some(&s("remote doc")));
}

// ---------------------------------------------------------------------------
// LWW conflict: remote newer wins
// ---------------------------------------------------------------------------

#[test]
fn lww_remote_newer_wins() {
    let db = Database::open_in_memory().unwrap();
    let adapter = LastWriteWins::new();
    let col = db.collection("notes").unwrap();

    // Insert a local doc with old timestamp
    let local_fields = vec![
        ("content".into(), s("local version")),
        ("_changed_at".into(), i(1000)),
    ];
    let _local_id_str = col.insert(local_fields).unwrap().to_string();

    // Remote doc is newer
    let remote_doc = taladb_core::document::Document::new(vec![
        ("content".into(), s("remote version")),
        ("_changed_at".into(), i(9000)),
    ]);
    let _remote_id = remote_doc.id;

    // We need to match the _id that import_changes looks up
    // import_changes looks up by Filter::Eq("_id", Str(change.id.to_string()))
    // Since local doc doesn't have "_id" field, let's insert with the same id via a new doc
    // Instead, test with a fresh collection where the local _id is set explicitly
    let db2 = Database::open_in_memory().unwrap();
    let col2 = db2.collection("notes").unwrap();

    // Insert a doc that has _changed_at
    let fields = vec![
        ("content".into(), s("local version")),
        ("_changed_at".into(), i(1000)),
    ];
    col2.insert(fields).unwrap();

    // Remote has same remote_id but is newer
    let remote_doc2 = taladb_core::document::Document::new(vec![
        ("content".into(), s("remote wins")),
        ("_changed_at".into(), i(9999)),
    ]);
    let r2_id = remote_doc2.id;
    let changeset: Changeset = vec![Change {
        collection: "notes".into(),
        id: r2_id,
        op: ChangeOp::Upsert(remote_doc2),
        changed_at: 9999,
    }];
    let applied = adapter.import_changes(&db2, changeset).unwrap();
    assert_eq!(applied, 1);

    // Both local and remote doc should now be in the collection
    // (no conflict since they have different IDs)
    let all = col2.find(Filter::All).unwrap();
    assert_eq!(all.len(), 2);
}

// ---------------------------------------------------------------------------
// LWW conflict: local newer wins
// ---------------------------------------------------------------------------

#[test]
fn lww_local_newer_is_not_overwritten() {
    let db = Database::open_in_memory().unwrap();
    let adapter = LastWriteWins::new();
    let col = db.collection("notes").unwrap();

    // Insert local doc with a very high timestamp
    let local_doc_id = Ulid::new();
    let local_doc = taladb_core::document::Document::with_id(
        local_doc_id,
        vec![
            ("content".into(), s("local is newer")),
            ("_changed_at".into(), i(9000)),
            ("_id".into(), s(&local_doc_id.to_string())),
        ],
    );
    // Insert directly via changeset to control the ID
    let setup: Changeset = vec![Change {
        collection: "notes".into(),
        id: local_doc_id,
        op: ChangeOp::Upsert(local_doc),
        changed_at: 9000,
    }];
    adapter.import_changes(&db, setup).unwrap();

    // Remote has same ID but is older
    let remote_doc = taladb_core::document::Document::with_id(
        local_doc_id,
        vec![
            ("content".into(), s("remote is older")),
            ("_changed_at".into(), i(1000)),
            ("_id".into(), s(&local_doc_id.to_string())),
        ],
    );
    let remote_changeset: Changeset = vec![Change {
        collection: "notes".into(),
        id: local_doc_id,
        op: ChangeOp::Upsert(remote_doc),
        changed_at: 1000,
    }];
    let applied = adapter.import_changes(&db, remote_changeset).unwrap();
    assert_eq!(
        applied, 0,
        "older remote change should not overwrite newer local"
    );

    let results = col.find(Filter::All).unwrap();
    // The content should remain from the local (newer) version
    let found_local = results
        .iter()
        .any(|d| d.get("content") == Some(&s("local is newer")));
    assert!(found_local, "local newer content must not be overwritten");
}

// ---------------------------------------------------------------------------
// import_changes — delete operation
// ---------------------------------------------------------------------------

#[test]
fn import_delete_removes_existing_doc() {
    let db = Database::open_in_memory().unwrap();
    let adapter = LastWriteWins::new();
    let col = db.collection("items").unwrap();

    // Insert a local doc via changeset (so we have control over its ID)
    let doc_id = Ulid::new();
    let doc = taladb_core::document::Document::with_id(
        doc_id,
        vec![
            ("name".into(), s("to be deleted")),
            ("_id".into(), s(&doc_id.to_string())),
        ],
    );
    let setup: Changeset = vec![Change {
        collection: "items".into(),
        id: doc_id,
        op: ChangeOp::Upsert(doc),
        changed_at: 1000,
    }];
    adapter.import_changes(&db, setup).unwrap();
    assert_eq!(col.find(Filter::All).unwrap().len(), 1);

    // Now import a delete for the same ID
    let delete_changeset: Changeset = vec![Change {
        collection: "items".into(),
        id: doc_id,
        op: ChangeOp::Delete,
        changed_at: 2000,
    }];
    let applied = adapter.import_changes(&db, delete_changeset).unwrap();
    assert_eq!(applied, 1);
    assert_eq!(col.find(Filter::All).unwrap().len(), 0);
}

#[test]
fn imported_delete_preserves_original_timestamp_when_reexported() {
    let db = Database::open_in_memory().unwrap();
    let adapter = LastWriteWins::new();
    let doc_id = Ulid::new();
    let doc = Document::with_id(
        doc_id,
        vec![("_changed_at".into(), i(1000)), ("name".into(), s("old"))],
    );

    adapter
        .import_changes(
            &db,
            vec![Change {
                collection: "items".into(),
                id: doc_id,
                op: ChangeOp::Upsert(doc),
                changed_at: 1000,
            }],
        )
        .unwrap();
    adapter
        .import_changes(
            &db,
            vec![Change {
                collection: "items".into(),
                id: doc_id,
                op: ChangeOp::Delete,
                changed_at: 2000,
            }],
        )
        .unwrap();

    let exported = adapter.export_changes(&db, &["items"], 0).unwrap();
    assert_eq!(exported.len(), 1);
    assert!(matches!(exported[0].op, ChangeOp::Delete));
    assert_eq!(exported[0].changed_at, 2000);
}

#[test]
fn import_delete_nonexistent_returns_zero() {
    let db = Database::open_in_memory().unwrap();
    let adapter = LastWriteWins::new();

    let changeset: Changeset = vec![Change {
        collection: "items".into(),
        id: Ulid::new(),
        op: ChangeOp::Delete,
        changed_at: 1000,
    }];
    let applied = adapter.import_changes(&db, changeset).unwrap();
    assert_eq!(
        applied, 0,
        "deleting a non-existent doc should return 0 applied"
    );
}

// ---------------------------------------------------------------------------
// Round-trip: export then import
// ---------------------------------------------------------------------------

#[test]
fn export_import_round_trip() {
    let source_db = Database::open_in_memory().unwrap();
    let target_db = Database::open_in_memory().unwrap();
    let adapter = LastWriteWins::new();

    let source_col = source_db.collection("data").unwrap();
    for n in 0..5i64 {
        let mut fields = vec![("value".into(), i(n))];
        stamp(&mut fields);
        source_col.insert(fields).unwrap();
    }

    let changeset = adapter.export_changes(&source_db, &["data"], 0).unwrap();
    let applied = adapter.import_changes(&target_db, changeset).unwrap();
    assert_eq!(applied, 5);

    let target_col = target_db.collection("data").unwrap();
    let docs = target_col.find(Filter::All).unwrap();
    assert_eq!(docs.len(), 5);
}

// ---------------------------------------------------------------------------
// Helper trait for test readability
// ---------------------------------------------------------------------------

trait ChangeOpExt {
    fn upsert_title(&self) -> Option<&str>;
}

impl ChangeOpExt for ChangeOp {
    fn upsert_title(&self) -> Option<&str> {
        if let Self::Upsert(doc) = self {
            doc.get("title").and_then(|v| v.as_str())
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Regression: update → export → import must not propagate as a deletion
// ---------------------------------------------------------------------------

/// Full bidirectional round trip after an update.
///
/// Previously, importing an upsert used delete_by_id + insert_with_id, which
/// left a delete tombstone newer than the document. The next export then
/// emitted a Delete change that destroyed the document on every peer.
#[test]
fn update_round_trip_does_not_delete_document() {
    let db_a = Database::open_in_memory().unwrap();
    let db_b = Database::open_in_memory().unwrap();
    let col_a = db_a.collection("notes").unwrap();
    let col_b = db_b.collection("notes").unwrap();
    let adapter = LastWriteWins::new();

    // A creates a doc and updates it.
    let id = col_a.insert(vec![("title".into(), s("v1"))]).unwrap();
    col_a
        .update_one(
            Filter::Eq("title".into(), s("v1")),
            taladb_core::Update::Set(vec![("title".into(), s("v2"))]),
        )
        .unwrap();

    // A → B
    let changes = adapter.export_changes(&db_a, &["notes"], 0).unwrap();
    adapter.import_changes(&db_b, changes).unwrap();
    let doc_b = col_b.find_by_id(id).unwrap().expect("doc must exist on B");
    assert_eq!(doc_b.get("title"), Some(&s("v2")));

    // B → A (B's import must not have produced a tombstone that deletes on A)
    let changes_back = adapter.export_changes(&db_b, &["notes"], 0).unwrap();
    assert!(
        !changes_back
            .iter()
            .any(|c| matches!(c.op, ChangeOp::Delete) && c.id == id),
        "import on B must not generate a Delete change for an upserted doc"
    );
    adapter.import_changes(&db_a, changes_back).unwrap();
    let doc_a = col_a.find_by_id(id).unwrap();
    assert!(
        doc_a.is_some(),
        "document must survive a full A→B→A sync round trip after an update"
    );
    assert_eq!(doc_a.unwrap().get("title"), Some(&s("v2")));
}

/// A stale remote Delete (older than the local document) must not destroy
/// the newer local write.
#[test]
fn stale_delete_does_not_remove_newer_document() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("notes").unwrap();
    let adapter = LastWriteWins::new();

    let doc = Document::new(vec![
        ("title".into(), s("fresh")),
        ("_changed_at".into(), i(10_000)),
    ]);
    let id = doc.id;
    col.insert_with_id(doc).unwrap();

    let stale_delete: Changeset = vec![Change {
        collection: "notes".into(),
        id,
        op: ChangeOp::Delete,
        changed_at: 5_000, // older than the local doc
    }];
    let applied = adapter.import_changes(&db, stale_delete).unwrap();
    assert_eq!(applied, 0, "stale delete must not be applied");
    assert!(
        col.find_by_id(id).unwrap().is_some(),
        "newer local document must survive a stale remote delete"
    );
}

/// A newer remote Delete still removes the document.
#[test]
fn newer_delete_removes_older_document() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("notes").unwrap();
    let adapter = LastWriteWins::new();

    let doc = Document::new(vec![
        ("title".into(), s("old")),
        ("_changed_at".into(), i(5_000)),
    ]);
    let id = doc.id;
    col.insert_with_id(doc).unwrap();

    let newer_delete: Changeset = vec![Change {
        collection: "notes".into(),
        id,
        op: ChangeOp::Delete,
        changed_at: 10_000,
    }];
    let applied = adapter.import_changes(&db, newer_delete).unwrap();
    assert_eq!(applied, 1);
    assert!(col.find_by_id(id).unwrap().is_none());
}

/// A stale upsert must not resurrect a document deleted more recently.
#[test]
fn stale_upsert_does_not_resurrect_newer_deletion() {
    let db = Database::open_in_memory().unwrap();
    let col = db.collection("notes").unwrap();
    let adapter = LastWriteWins::new();

    // Install a tombstone at t=10_000 via a remote delete.
    let id = Ulid::new();
    adapter
        .import_changes(
            &db,
            vec![Change {
                collection: "notes".into(),
                id,
                op: ChangeOp::Delete,
                changed_at: 10_000,
            }],
        )
        .unwrap();

    // A stale upsert (t=5_000) arrives afterwards.
    let stale_doc = Document::with_id(
        id,
        vec![
            ("title".into(), s("zombie")),
            ("_changed_at".into(), i(5_000)),
        ],
    );
    let applied = adapter
        .import_changes(
            &db,
            vec![Change {
                collection: "notes".into(),
                id,
                op: ChangeOp::Upsert(stale_doc),
                changed_at: 5_000,
            }],
        )
        .unwrap();
    assert_eq!(applied, 0, "stale upsert must lose to a newer tombstone");
    assert!(col.find_by_id(id).unwrap().is_none());
}

/// Equal-timestamp conflicting upserts must converge to the same winner on
/// both replicas (previously the tie-break compared a ULID with itself and
/// each replica kept its own version).
#[test]
fn equal_timestamp_conflict_converges_on_both_replicas() {
    let adapter = LastWriteWins::new();
    let id = Ulid::new();
    let ts = 7_777u64;

    let doc_x = Document::with_id(
        id,
        vec![("v".into(), s("xxx")), ("_changed_at".into(), i(ts as i64))],
    );
    let doc_y = Document::with_id(
        id,
        vec![("v".into(), s("yyy")), ("_changed_at".into(), i(ts as i64))],
    );

    // Replica A holds X and receives Y; replica B holds Y and receives X.
    let db_a = Database::open_in_memory().unwrap();
    let db_b = Database::open_in_memory().unwrap();
    db_a.collection("c")
        .unwrap()
        .insert_with_id(doc_x.clone())
        .unwrap();
    db_b.collection("c")
        .unwrap()
        .insert_with_id(doc_y.clone())
        .unwrap();

    adapter
        .import_changes(
            &db_a,
            vec![Change {
                collection: "c".into(),
                id,
                op: ChangeOp::Upsert(doc_y),
                changed_at: ts,
            }],
        )
        .unwrap();
    adapter
        .import_changes(
            &db_b,
            vec![Change {
                collection: "c".into(),
                id,
                op: ChangeOp::Upsert(doc_x),
                changed_at: ts,
            }],
        )
        .unwrap();

    let final_a = db_a
        .collection("c")
        .unwrap()
        .find_by_id(id)
        .unwrap()
        .unwrap();
    let final_b = db_b
        .collection("c")
        .unwrap()
        .find_by_id(id)
        .unwrap()
        .unwrap();
    assert_eq!(
        final_a.get("v"),
        final_b.get("v"),
        "both replicas must converge on the same winner for equal timestamps"
    );
}

// ---------------------------------------------------------------------------
// Database-level convenience API (backs the JS db.sync() orchestration)
// ---------------------------------------------------------------------------

#[test]
fn database_export_import_round_trip_between_two_dbs() {
    // Mirrors the JS bidirectional sync loop: db_a exports since a cursor,
    // db_b imports; then a later write on db_a syncs incrementally.
    let db_a = Database::open_in_memory().unwrap();
    let db_b = Database::open_in_memory().unwrap();

    let a = db_a.collection("notes").unwrap();
    a.insert_with_id(Document::new(vec![
        ("body".into(), s("first")),
        ("_changed_at".into(), i(1000)),
    ]))
    .unwrap();
    a.insert_with_id(Document::new(vec![
        ("body".into(), s("second")),
        ("_changed_at".into(), i(2000)),
    ]))
    .unwrap();

    // Full sync from cursor 0 — both docs cross over.
    let cs = db_a.export_changes(&["notes"], 0).unwrap();
    assert_eq!(cs.len(), 2);
    let applied = db_b.import_changes(cs).unwrap();
    assert_eq!(applied, 2);
    assert_eq!(
        db_b.collection("notes")
            .unwrap()
            .count(Filter::All)
            .unwrap(),
        2
    );

    // Incremental: only a change after the advanced cursor is exported.
    a.insert_with_id(Document::new(vec![
        ("body".into(), s("third")),
        ("_changed_at".into(), i(3000)),
    ]))
    .unwrap();
    let delta = db_a.export_changes(&["notes"], 2000).unwrap();
    assert_eq!(delta.len(), 1, "only the post-cursor change is exported");
    assert_eq!(db_b.import_changes(delta).unwrap(), 1);
    assert_eq!(
        db_b.collection("notes")
            .unwrap()
            .count(Filter::All)
            .unwrap(),
        3
    );
}

#[test]
fn database_import_is_idempotent_under_replay() {
    // Re-importing the same changeset (e.g. an at-least-once transport retry)
    // must not double-apply — LWW sees equal timestamps and keeps one copy.
    let db_a = Database::open_in_memory().unwrap();
    let db_b = Database::open_in_memory().unwrap();
    db_a.collection("c")
        .unwrap()
        .insert_with_id(Document::new(vec![
            ("v".into(), s("x")),
            ("_changed_at".into(), i(5000)),
        ]))
        .unwrap();

    let cs = db_a.export_changes(&["c"], 0).unwrap();
    db_b.import_changes(cs.clone()).unwrap();
    db_b.import_changes(cs).unwrap(); // replay
    assert_eq!(
        db_b.collection("c").unwrap().count(Filter::All).unwrap(),
        1,
        "replayed changeset must not create a duplicate"
    );
}

#[test]
fn sync_cursor_collection_is_addressable() {
    // Regression: `db.sync()` persists its cursor in `__taladb_sync` through
    // the ordinary collection API. The reserved-`_` validation must exempt it
    // (0.8.4 rejected it, breaking the first real sync pass), while other
    // `_`-prefixed names stay blocked and the cursor store stays hidden.
    let db = Database::open_in_memory().unwrap();

    let cursors = db.collection("__taladb_sync").unwrap();
    cursors
        .insert_with_id(Document::new(vec![
            ("target".into(), s("default")),
            ("sinceMs".into(), i(0)),
        ]))
        .unwrap();
    assert_eq!(cursors.count(Filter::All).unwrap(), 1);

    // Still reserved for everything else…
    assert!(db.collection("_audit").is_err());
    assert!(db.collection("__other").is_err());
    // …and still hidden from the public listing.
    assert!(
        !db.list_collection_names()
            .unwrap()
            .contains(&"__taladb_sync".to_string())
    );
}

// ---------------------------------------------------------------------------
// Tolerant import-time validation (ImportValidator)
// ---------------------------------------------------------------------------

use std::sync::Arc;
use taladb_core::sync::{ImportDecision, ImportValidator};

/// Test validator modelling the three tolerant outcomes plus coercion:
/// - collection "ignored"        -> Skip
/// - missing/blank "name" string -> Quarantine
/// - doc tagged `_v = 0`         -> Coerce (stamp it up to _v = 1)
/// - otherwise                   -> Accept
struct DemoValidator;

impl ImportValidator for DemoValidator {
    fn check(&self, collection: &str, doc: &Document) -> ImportDecision {
        if collection == "ignored" {
            return ImportDecision::Skip;
        }
        match doc.get("name").and_then(|v| v.as_str()) {
            Some(name) if !name.is_empty() => {}
            _ => return ImportDecision::Quarantine("missing required `name`".into()),
        }
        // Below-current shape: upgrade in place (RxDB-style migrateDocument).
        if matches!(doc.get("_v"), Some(Value::Int(0)) | None) {
            let mut upgraded = doc.clone();
            upgraded.set("_v", Value::Int(1));
            return ImportDecision::Coerce(upgraded);
        }
        ImportDecision::Accept
    }
}

fn upsert(collection: &str, fields: Vec<(String, Value)>, changed_at: u64) -> Change {
    let mut fields = fields;
    fields.push(("_changed_at".into(), i(changed_at as i64)));
    let doc = Document::new(fields);
    Change {
        collection: collection.into(),
        id: doc.id,
        op: ChangeOp::Upsert(doc),
        changed_at,
    }
}

#[test]
fn validated_import_accepts_valid_and_reports_applied() {
    let db = Database::open_in_memory().unwrap();
    let changeset = vec![upsert(
        "users",
        vec![("name".into(), s("Alice")), ("_v".into(), i(1))],
        1000,
    )];

    let report = db
        .import_changes_validated(changeset, Arc::new(DemoValidator))
        .unwrap();

    assert_eq!(report.applied, 1);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.quarantined, 0);
    assert_eq!(
        db.collection("users").unwrap().count(Filter::All).unwrap(),
        1
    );
}

#[test]
fn validated_import_coerces_below_current_shape() {
    let db = Database::open_in_memory().unwrap();
    // No _v => treated as v0 and upgraded to v1 on the way in.
    let changeset = vec![upsert("users", vec![("name".into(), s("Bob"))], 1000)];

    let report = db
        .import_changes_validated(changeset, Arc::new(DemoValidator))
        .unwrap();

    assert_eq!(report.applied, 1);
    let stored = db.collection("users").unwrap().find(Filter::All).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].get("_v"),
        Some(&Value::Int(1)),
        "coerced doc must be stamped to v1"
    );
}

#[test]
fn validated_import_quarantines_invalid_without_aborting_batch() {
    let db = Database::open_in_memory().unwrap();
    // A bad doc sandwiched between two good ones: the batch must still apply both good docs.
    let changeset = vec![
        upsert(
            "users",
            vec![("name".into(), s("Ana")), ("_v".into(), i(1))],
            1000,
        ),
        upsert(
            "users",
            vec![("age".into(), i(30)), ("_v".into(), i(1))],
            1000,
        ), // no name -> quarantine
        upsert(
            "users",
            vec![("name".into(), s("Cid")), ("_v".into(), i(1))],
            1000,
        ),
    ];

    let report = db
        .import_changes_validated(changeset, Arc::new(DemoValidator))
        .unwrap();

    assert_eq!(
        report.applied, 2,
        "both valid docs must land despite the bad one"
    );
    assert_eq!(report.quarantined, 1);
    assert_eq!(
        db.collection("users").unwrap().count(Filter::All).unwrap(),
        2
    );

    // The rejected doc is retained, recoverable, with its reason.
    let held = db.quarantined("users").unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].reason, "missing required `name`");
    assert_eq!(held[0].changed_at, 1000);
    assert_eq!(held[0].document.get("age"), Some(&Value::Int(30)));
}

#[test]
fn validated_import_skips_unmodelled_collection() {
    let db = Database::open_in_memory().unwrap();
    let changeset = vec![upsert("ignored", vec![("name".into(), s("noise"))], 1000)];

    let report = db
        .import_changes_validated(changeset, Arc::new(DemoValidator))
        .unwrap();

    assert_eq!(report.skipped, 1);
    assert_eq!(report.applied, 0);
    assert_eq!(
        db.collection("ignored")
            .unwrap()
            .count(Filter::All)
            .unwrap(),
        0
    );
    assert!(db.quarantined("ignored").unwrap().is_empty());
}

#[test]
fn unvalidated_import_still_applies_everything() {
    // Back-compat: import_changes (no validator) must be unchanged.
    let db = Database::open_in_memory().unwrap();
    let changeset = vec![
        upsert("users", vec![("age".into(), i(30))], 1000), // would be quarantined *with* a validator
    ];
    let applied = db.import_changes(changeset).unwrap();
    assert_eq!(applied, 1);
    assert_eq!(
        db.collection("users").unwrap().count(Filter::All).unwrap(),
        1
    );
}

// ---------------------------------------------------------------------------
// Built-in SchemaValidator / StructuralSchema
// ---------------------------------------------------------------------------

use std::collections::HashMap;
use taladb_core::{FieldType, SchemaValidator, StructuralSchema};

fn schema_validator(col: &str, schema: StructuralSchema) -> Arc<SchemaValidator> {
    let mut m = HashMap::new();
    m.insert(col.to_string(), schema);
    Arc::new(SchemaValidator::new(m))
}

#[test]
fn structural_schema_accepts_valid_and_quarantines_bad_type() {
    let db = Database::open_in_memory().unwrap();
    let mut types = HashMap::new();
    types.insert("name".to_string(), FieldType::Str);
    types.insert("age".to_string(), FieldType::Int);
    let schema = StructuralSchema {
        version: 1,
        required: vec!["name".into()],
        types,
        defaults: vec![],
        renames: vec![],
    };
    let v = schema_validator("users", schema);

    let changeset = vec![
        upsert(
            "users",
            vec![
                ("name".into(), s("Ana")),
                ("age".into(), i(30)),
                ("_v".into(), i(1)),
            ],
            1000,
        ),
        upsert(
            "users",
            vec![("name".into(), i(42)), ("_v".into(), i(1))],
            1000,
        ), // name wrong type
        upsert(
            "users",
            vec![("age".into(), i(9)), ("_v".into(), i(1))],
            1000,
        ), // missing required name
    ];
    let report = db.import_changes_validated(changeset, v).unwrap();

    assert_eq!(report.applied, 1);
    assert_eq!(report.quarantined, 2);
    let held = db.quarantined("users").unwrap();
    assert_eq!(held.len(), 2);
}

#[test]
fn structural_schema_upgrades_below_version_with_defaults() {
    let db = Database::open_in_memory().unwrap();
    let schema = StructuralSchema {
        version: 2,
        required: vec![],
        types: HashMap::new(),
        defaults: vec![("locale".into(), s("en"))],
        renames: vec![],
    };
    let v = schema_validator("posts", schema);

    // Doc has no _v (=> v0) and no locale — should be upgraded to v2 with locale.
    let changeset = vec![upsert("posts", vec![("title".into(), s("hi"))], 1000)];
    let report = db.import_changes_validated(changeset, v).unwrap();

    assert_eq!(report.applied, 1);
    let stored = db.collection("posts").unwrap().find(Filter::All).unwrap();
    assert_eq!(stored[0].get("_v"), Some(&Value::Int(2)));
    assert_eq!(stored[0].get("locale"), Some(&Value::Str("en".into())));
}

#[test]
fn structural_schema_accepts_newer_peer_shape_untouched() {
    let db = Database::open_in_memory().unwrap();
    let schema = StructuralSchema {
        version: 1,
        required: vec!["name".into()],
        types: HashMap::new(),
        defaults: vec![],
        renames: vec![],
    };
    let v = schema_validator("users", schema);

    // _v = 5 is ahead of our v1: accept as-is even though it lacks `name`.
    let changeset = vec![upsert(
        "users",
        vec![("future".into(), s("x")), ("_v".into(), i(5))],
        1000,
    )];
    let report = db.import_changes_validated(changeset, v).unwrap();
    assert_eq!(report.applied, 1);
    assert_eq!(report.quarantined, 0);
}

#[test]
fn structural_schema_unregistered_collection_passes_through() {
    let db = Database::open_in_memory().unwrap();
    let v = schema_validator("users", StructuralSchema::default());
    // "other" has no registered schema -> accepted regardless of shape.
    let changeset = vec![upsert("other", vec![("anything".into(), i(1))], 1000)];
    let report = db.import_changes_validated(changeset, v).unwrap();
    assert_eq!(report.applied, 1);
    assert_eq!(report.skipped, 0);
}

#[test]
fn structural_schema_renames_field_on_upgrade() {
    let db = Database::open_in_memory().unwrap();
    let schema = StructuralSchema {
        version: 2,
        required: vec!["email".into()],
        types: HashMap::new(),
        defaults: vec![],
        renames: vec![("mail".into(), "email".into())],
    };
    let v = schema_validator("users", schema);

    // A v0 doc using the old field name `mail` — renamed to `email` on import,
    // so the required-check on `email` passes.
    let changeset = vec![upsert("users", vec![("mail".into(), s("a@b.c"))], 1000)];
    let report = db.import_changes_validated(changeset, v).unwrap();

    assert_eq!(report.applied, 1);
    assert_eq!(report.quarantined, 0);
    let stored = db.collection("users").unwrap().find(Filter::All).unwrap();
    assert_eq!(stored[0].get("email"), Some(&Value::Str("a@b.c".into())));
    assert_eq!(
        stored[0].get("mail"),
        None,
        "old field removed after rename"
    );
    assert_eq!(stored[0].get("_v"), Some(&Value::Int(2)));
}
