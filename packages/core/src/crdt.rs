//! CRDT sync adapter for TalaDB — per-field LWW-register conflict resolution.
//!
//! Each field in a document carries a logical clock stored under the hidden
//! `_crdt_clocks` field.  When two replicas independently write the *same*
//! document, `import_crdt_changes` merges at field granularity rather than
//! picking a whole-document winner:
//!
//! ```text
//! Device A writes title = "Hello"  at t=100, node="A"
//! Device B writes price = 99       at t=100, node="B"
//! After sync → doc has title="Hello" AND price=99 (no data lost)
//! ```
//!
//! If two replicas write the **same** field concurrently, the one with the
//! higher `ts_ms` wins; ties are broken by `node_id` lexicographic order,
//! ensuring a deterministic total order without coordination.
//!
//! Array fields can optionally be configured as **grow-only sets** (G-Set)
//! via [`CrdtSyncAdapter::with_g_set_fields`].  G-Set fields are always
//! merged by union — elements can never be removed — which avoids the
//! lost-add anomaly when two devices concurrently push to the same array.
//!
//! # Usage
//!
//! ```rust,ignore
//! let adapter = CrdtSyncAdapter::new("device-alice");
//!
//! // Write path — stamp fields before inserting
//! let fields = adapter.stamp_insert(vec![
//!     ("title".into(), Value::Str("Hello".into())),
//! ]);
//! col.insert(fields)?;
//!
//! // Update path — stamp only the changed fields
//! adapter.update_fields(&col, doc_id, vec![
//!     ("title".into(), Value::Str("Updated".into())),
//! ])?;
//!
//! // Sync path
//! let outgoing = adapter.export_crdt_changes(&db, &["docs"], since_ms)?;
//! // ... exchange with peer ...
//! let applied = adapter.import_crdt_changes(&db, peer_changeset)?;
//! ```

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::document::{Document, Value};
#[allow(unused_imports)]
use crate::engine::StorageBackend;
use crate::error::TalaDbError;
use crate::index::tomb_table_name;
use crate::query::filter::Filter;
use crate::sync::now_ms;

/// Hidden document field that stores per-field logical clocks.
pub const CRDT_CLOCKS_FIELD: &str = "_crdt_clocks";

/// Default maximum number of entries accepted in a single [`CrdtChangeset`].
///
/// Override per-adapter via [`CrdtSyncAdapter::with_max_changeset_entries`].
pub const DEFAULT_MAX_CHANGESET_ENTRIES: usize = 10_000;

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

/// Per-field logical clock: (wall-clock ms, replica id).
///
/// Comparison uses `ts_ms` first; `node_id` lexicographic order breaks ties.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FieldClock {
    /// Milliseconds since Unix epoch at the time of the write.
    pub ts_ms: u64,
    /// Opaque replica identifier — any stable unique string per device.
    pub node_id: String,
}

impl FieldClock {
    pub fn new(ts_ms: u64, node_id: impl Into<String>) -> Self {
        Self {
            ts_ms,
            node_id: node_id.into(),
        }
    }

    /// Returns `true` if this clock causally dominates `other` (wins the merge).
    pub fn dominates(&self, other: &Self) -> bool {
        self.ts_ms > other.ts_ms || (self.ts_ms == other.ts_ms && self.node_id > other.node_id)
    }
}

// ---------------------------------------------------------------------------
// Changeset types
// ---------------------------------------------------------------------------

/// One field-level mutation exported from a replica.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldMutation {
    /// Field name (user-visible, no leading `_`).
    pub field: String,
    /// New value, or `None` to remove the field.
    pub value: Option<Value>,
    /// Logical clock at the time this field was written.
    pub clock: FieldClock,
}

/// A document-level CRDT change record.
///
/// Either a set of field mutations (`mutations` non-empty, `delete_clock` is
/// `None`) or a delete (`mutations` empty, `delete_clock` is `Some`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrdtChange {
    pub collection: String,
    pub id: Ulid,
    pub mutations: Vec<FieldMutation>,
    /// If `Some`, this record requests deletion of the document.
    pub delete_clock: Option<FieldClock>,
}

/// An ordered list of CRDT changes to exchange between replicas.
pub type CrdtChangeset = Vec<CrdtChange>;

// ---------------------------------------------------------------------------
// CrdtAdapter trait
// ---------------------------------------------------------------------------

/// Interface for CRDT-based peer sync.
pub trait CrdtAdapter: Send + Sync {
    /// Export all field mutations that occurred after `since_ms` (exclusive).
    fn export_crdt_changes(
        &self,
        db: &crate::Database,
        collections: &[&str],
        since_ms: u64,
    ) -> Result<CrdtChangeset, TalaDbError>;

    /// Merge a remote [`CrdtChangeset`] into the local database at field
    /// granularity.  Returns the number of documents actually modified.
    fn import_crdt_changes(
        &self,
        db: &crate::Database,
        changeset: CrdtChangeset,
    ) -> Result<u64, TalaDbError>;
}

// ---------------------------------------------------------------------------
// CrdtSyncAdapter
// ---------------------------------------------------------------------------

/// Conflict-free sync adapter using per-field LWW-registers.
///
/// Fields listed via [`with_g_set_fields`] use grow-only set (union) semantics
/// instead of LWW for array values.
///
/// [`with_g_set_fields`]: CrdtSyncAdapter::with_g_set_fields
pub struct CrdtSyncAdapter {
    node_id: String,
    g_set_fields: HashSet<String>,
    /// When `Some`, only changes for listed collections are imported.
    allowed_collections: Option<HashSet<String>>,
    /// Maximum number of entries accepted in a single `import_crdt_changes` call.
    max_changeset_entries: usize,
}

impl CrdtSyncAdapter {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            g_set_fields: HashSet::new(),
            allowed_collections: None,
            max_changeset_entries: DEFAULT_MAX_CHANGESET_ENTRIES,
        }
    }

    /// Override the per-import changeset entry limit (default: 10 000).
    ///
    /// Useful for trusted peer environments where larger batch syncs are expected.
    pub fn with_max_changeset_entries(mut self, n: usize) -> Self {
        self.max_changeset_entries = n;
        self
    }

    /// Restrict `import_crdt_changes` to a known set of collection names.
    ///
    /// Any incoming [`CrdtChange`] whose collection is not in `collections` is
    /// silently skipped. When not configured (the default), all collection names
    /// are accepted for backwards compatibility.
    pub fn with_allowed_collections(
        mut self,
        collections: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allowed_collections = Some(collections.into_iter().map(Into::into).collect());
        self
    }

    /// Configure specific array fields to use grow-only set semantics.
    ///
    /// G-Set fields are merged by union rather than LWW — every element ever
    /// added to the array from any replica is preserved.
    pub fn with_g_set_fields(
        mut self,
        fields: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.g_set_fields = fields.into_iter().map(Into::into).collect();
        self
    }

    /// Prepare fields for a CRDT-tracked insert.
    ///
    /// Stamps every non-system field with the current clock under
    /// `_crdt_clocks` and adds `_changed_at`.  Pass the result directly to
    /// [`Collection::insert`].
    pub fn stamp_insert(&self, fields: Vec<(String, Value)>) -> Vec<(String, Value)> {
        self.stamp_insert_at(fields, now_ms())
    }

    /// Like [`stamp_insert`] but uses a caller-supplied timestamp.
    ///
    /// [`stamp_insert`]: CrdtSyncAdapter::stamp_insert
    pub fn stamp_insert_at(
        &self,
        mut fields: Vec<(String, Value)>,
        ts_ms: u64,
    ) -> Vec<(String, Value)> {
        fields.retain(|(k, _)| k != CRDT_CLOCKS_FIELD && k != "_changed_at");
        let clock_val = make_clock_value(ts_ms, &self.node_id);
        let clock_entries: Vec<(String, Value)> = fields
            .iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .map(|(k, _)| (k.clone(), clock_val.clone()))
            .collect();
        fields.push((CRDT_CLOCKS_FIELD.into(), Value::Object(clock_entries)));
        fields.push(("_changed_at".into(), Value::Int(ts_ms as i64)));
        fields
    }

    /// Update specific fields on an existing document with CRDT clock tracking.
    ///
    /// Loads the document, advances clocks for `changes`, and writes the
    /// merged document back.  Returns `true` if the document was found and
    /// updated, `false` if not found.
    pub fn update_fields(
        &self,
        col: &crate::collection::Collection,
        id: Ulid,
        changes: Vec<(String, Value)>,
    ) -> Result<bool, TalaDbError> {
        self.update_fields_at(col, id, changes, now_ms())
    }

    /// Like [`update_fields`] but uses a caller-supplied timestamp.
    ///
    /// [`update_fields`]: CrdtSyncAdapter::update_fields
    pub fn update_fields_at(
        &self,
        col: &crate::collection::Collection,
        id: Ulid,
        changes: Vec<(String, Value)>,
        ts_ms: u64,
    ) -> Result<bool, TalaDbError> {
        let mut doc = match col.find_by_id(id)? {
            Some(d) => d,
            None => return Ok(false),
        };
        let mut clock_map = read_clock_map(&doc);
        let new_clock = FieldClock::new(ts_ms, &self.node_id);
        for (field, val) in changes {
            if !field.starts_with('_') {
                doc.set(field.clone(), val);
                clock_map.insert(field, new_clock.clone());
            }
        }
        let highest_ts = clock_map.values().map(|c| c.ts_ms).max().unwrap_or(ts_ms);
        doc.set(CRDT_CLOCKS_FIELD, clock_map_to_value(&clock_map));
        doc.set("_changed_at", Value::Int(highest_ts as i64));
        // Atomic in-place replace: delete_by_id + insert_with_id would write a
        // delete tombstone newer than the document, which the next export
        // would propagate to peers as a deletion of the updated document.
        col.replace_with_id(doc)?;
        Ok(true)
    }
}

impl CrdtAdapter for CrdtSyncAdapter {
    fn export_crdt_changes(
        &self,
        db: &crate::Database,
        collections: &[&str],
        since_ms: u64,
    ) -> Result<CrdtChangeset, TalaDbError> {
        let mut changes = Vec::new();

        for &col_name in collections {
            let col = db.collection(col_name)?;

            let docs = if since_ms == 0 {
                col.find(Filter::All)?
            } else {
                col.find(Filter::Gt(
                    "_changed_at".into(),
                    Value::Int(since_ms as i64),
                ))?
            };

            for doc in docs {
                let clock_map = read_clock_map(&doc);
                let mutations: Vec<FieldMutation> = doc
                    .fields
                    .iter()
                    .filter(|(k, _)| !k.starts_with('_'))
                    .filter_map(|(field, val)| {
                        let clock = clock_map.get(field).cloned().unwrap_or_default();
                        if since_ms == 0 || clock.ts_ms > since_ms {
                            Some(FieldMutation {
                                field: field.clone(),
                                value: Some(val.clone()),
                                clock,
                            })
                        } else {
                            None
                        }
                    })
                    .collect();

                if !mutations.is_empty() {
                    changes.push(CrdtChange {
                        collection: col_name.to_string(),
                        id: doc.id,
                        mutations,
                        delete_clock: None,
                    });
                }
            }

            // Export tombstones as delete changes.
            let tomb_table = tomb_table_name(col_name);
            let rtxn = db.backend().begin_read()?;
            let all_tombs = rtxn.scan_all(&tomb_table)?;
            for (key_bytes, val_bytes) in all_tombs {
                if key_bytes.len() != 16 {
                    continue;
                }
                let ts: i64 = postcard::from_bytes(&val_bytes).unwrap_or_else(|e| {
                    tracing::warn!(
                        collection = col_name,
                        error = %e,
                        "crdt: corrupt tombstone timestamp, treating as max so deletion still propagates"
                    );
                    // Default to i64::MAX so the tombstone is treated as "newer than
                    // everything" — ensures the delete propagates rather than being
                    // silently ignored because ts=0 loses every dominates() comparison.
                    i64::MAX
                });
                let ts_u64 = ts as u64;
                if ts_u64 > since_ms {
                    let mut id_arr = [0u8; 16];
                    id_arr.copy_from_slice(&key_bytes);
                    let id = Ulid::from_bytes(id_arr);
                    changes.push(CrdtChange {
                        collection: col_name.to_string(),
                        id,
                        mutations: Vec::new(),
                        delete_clock: Some(FieldClock::new(ts_u64, self.node_id.clone())),
                    });
                }
            }
        }

        Ok(changes)
    }

    fn import_crdt_changes(
        &self,
        db: &crate::Database,
        changeset: CrdtChangeset,
    ) -> Result<u64, TalaDbError> {
        if changeset.len() > self.max_changeset_entries {
            return Err(TalaDbError::ChangesetTooLarge);
        }

        let mut applied = 0u64;

        for change in changeset {
            // Skip collections not in the allowlist (if one is configured).
            if let Some(ref allowed) = self.allowed_collections
                && !allowed.contains(&change.collection)
            {
                continue;
            }

            let col = db.collection(&change.collection)?;

            if let Some(ref delete_clock) = change.delete_clock {
                let local = col.find_by_id(change.id)?;
                let should_delete = match local {
                    None => false,
                    Some(ref local_doc) => {
                        let local_ts = local_doc
                            .get("_changed_at")
                            .and_then(|v| {
                                if let Value::Int(n) = v {
                                    Some(*n as u64)
                                } else {
                                    None
                                }
                            })
                            .unwrap_or(0);
                        delete_clock.dominates(&FieldClock::new(local_ts, ""))
                    }
                };

                // Always upsert the tombstone — even if we don't delete locally —
                // so this replica can forward the deletion to downstream peers.
                let tomb_table = tomb_table_name(&change.collection);
                let mut wtxn = db.backend().begin_write()?;
                let ts_bytes = postcard::to_allocvec(&(delete_clock.ts_ms as i64))?;
                let existing_ts: i64 = wtxn
                    .get(&tomb_table, &change.id.to_bytes())?
                    .and_then(|b| postcard::from_bytes::<i64>(&b).ok())
                    .unwrap_or(0);
                if delete_clock.ts_ms as i64 > existing_ts {
                    wtxn.put(&tomb_table, &change.id.to_bytes(), &ts_bytes)?;
                }
                wtxn.commit()?;

                if should_delete {
                    col.delete_by_id(change.id)?;
                    applied += 1;
                }
                continue;
            }

            if change.mutations.is_empty() {
                continue;
            }

            // Field-level merge.
            let local_opt = col.find_by_id(change.id)?;

            // If the document was deleted locally, only resurrect it when at
            // least one incoming mutation is newer than the deletion. A
            // tombstone with ts >= every mutation clock means the delete wins.
            if local_opt.is_none() {
                let tomb_table = tomb_table_name(&change.collection);
                let rtxn = db.backend().begin_read()?;
                let tomb_ts: Option<i64> = rtxn
                    .get(&tomb_table, &change.id.to_bytes())?
                    .and_then(|b| postcard::from_bytes::<i64>(&b).ok());
                drop(rtxn);
                if let Some(tomb_ts) = tomb_ts {
                    let newest_mutation = change
                        .mutations
                        .iter()
                        .map(|m| m.clock.ts_ms)
                        .max()
                        .unwrap_or(0);
                    if newest_mutation <= tomb_ts as u64 {
                        continue;
                    }
                }
            }

            let mut merged_doc =
                local_opt.unwrap_or_else(|| Document::with_id(change.id, Vec::new()));
            let mut clock_map = read_clock_map(&merged_doc);
            let mut any_changed = false;

            for mutation in change.mutations {
                if mutation.field.starts_with('_') {
                    continue;
                }

                let local_clock = clock_map.get(&mutation.field).cloned().unwrap_or_default();

                if self.g_set_fields.contains(&mutation.field) {
                    // G-Set: merge by union regardless of clock direction.
                    if let Some(Value::Array(ref remote_arr)) = mutation.value {
                        let local_arr = match merged_doc.get(&mutation.field) {
                            Some(Value::Array(a)) => a.clone(),
                            _ => Vec::new(),
                        };
                        let merged_arr = gset_union(&local_arr, remote_arr);
                        if merged_arr.len() > local_arr.len() {
                            merged_doc.set(mutation.field.clone(), Value::Array(merged_arr));
                            any_changed = true;
                        }
                        // Advance the clock to the winner so future exports are correct.
                        if mutation.clock.dominates(&local_clock) {
                            clock_map.insert(mutation.field, mutation.clock);
                        }
                    }
                    // Non-array value for a G-Set field: ignore (misconfiguration).
                } else if mutation.clock.dominates(&local_clock) {
                    // LWW-register: remote field wins.
                    match mutation.value {
                        Some(val) => merged_doc.set(mutation.field.clone(), val),
                        None => {
                            merged_doc.remove(&mutation.field);
                        }
                    }
                    clock_map.insert(mutation.field, mutation.clock);
                    any_changed = true;
                }
            }

            if any_changed {
                let highest_ts = clock_map.values().map(|c| c.ts_ms).max().unwrap_or(0);
                merged_doc.set(CRDT_CLOCKS_FIELD, clock_map_to_value(&clock_map));
                merged_doc.set("_changed_at", Value::Int(highest_ts as i64));

                // Atomic replace; also clears any local tombstone so the
                // merge is not later exported to peers as a deletion.
                col.replace_with_id(merged_doc)?;
                applied += 1;
            }
        }

        Ok(applied)
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn gset_union(local: &[Value], remote: &[Value]) -> Vec<Value> {
    let mut result = local.to_vec();
    for item in remote {
        if !result.contains(item) {
            result.push(item.clone());
        }
    }
    result
}

fn make_clock_value(ts_ms: u64, node_id: &str) -> Value {
    Value::Object(vec![
        ("t".into(), Value::Int(ts_ms as i64)),
        ("n".into(), Value::Str(node_id.to_string())),
    ])
}

fn parse_clock_value(v: &Value) -> Option<FieldClock> {
    if let Value::Object(fields) = v {
        let ts_ms = fields.iter().find(|(k, _)| k == "t").and_then(|(_, v)| {
            if let Value::Int(n) = v {
                Some(*n as u64)
            } else {
                None
            }
        })?;
        let node_id = fields
            .iter()
            .find(|(k, _)| k == "n")
            .and_then(|(_, v)| {
                if let Value::Str(s) = v {
                    Some(s.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        Some(FieldClock { ts_ms, node_id })
    } else {
        None
    }
}

fn read_clock_map(doc: &Document) -> HashMap<String, FieldClock> {
    let mut map = HashMap::new();
    if let Some(Value::Object(entries)) = doc.get(CRDT_CLOCKS_FIELD) {
        for (field, clock_val) in entries {
            if let Some(clock) = parse_clock_value(clock_val) {
                map.insert(field.clone(), clock);
            }
        }
    }
    map
}

fn clock_map_to_value(map: &HashMap<String, FieldClock>) -> Value {
    let mut entries: Vec<(String, Value)> = map
        .iter()
        .map(|(k, clock)| (k.clone(), make_clock_value(clock.ts_ms, &clock.node_id)))
        .collect();
    // Sort for deterministic serialization.
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Value::Object(entries)
}
