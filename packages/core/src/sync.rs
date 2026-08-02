//! Sync adapter interface for TalaDB.
//!
//! Defines the `SyncAdapter` trait and a built-in `LastWriteWins` (LWW)
//! implementation. A sync adapter is responsible for:
//!
//! 1. **Exporting** a changeset — the set of document mutations since a given
//!    logical clock / version vector.
//! 2. **Importing** a remote changeset — merging foreign mutations into the
//!    local database according to a conflict resolution policy.
//!
//! The adapter sits *above* the storage engine and works through the public
//! `Collection` API, so it is storage-agnostic.
//!
//! Changeset format
//! ----------------
//! A `Changeset` is a `Vec<Change>` where each `Change` records:
//! - The collection name
//! - The document ULID (ID)
//! - The operation (Upsert / Delete)
//! - A `u64` wall-clock timestamp (milliseconds since Unix epoch)
//! - The full document body (for Upserts)
//!
//! Last-Write-Wins conflict resolution
//! ------------------------------------
//! `LastWriteWins` merges by comparing `changed_at` timestamps. The change
//! with the higher timestamp wins. Equal-timestamp upserts are broken by
//! comparing the serialized document bytes (greater bytes win) — a symmetric
//! comparison every replica resolves identically, ensuring convergence
//! without coordination. Deletes win ties against upserts.

use std::collections::HashMap;
use std::sync::Arc;
use web_time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::document::{Document, Value};
#[allow(unused_imports)] // trait needed for begin_read/begin_write method dispatch
use crate::engine::StorageBackend;
use crate::error::TalaDbError;
use crate::index::{quarantine_table_name, tomb_table_name};
use crate::query::filter::Filter;

// ---------------------------------------------------------------------------
// Mutation hook types (Phase 2)
// ---------------------------------------------------------------------------

/// A post-commit mutation event emitted after every successful write.
///
/// Only changed data is included — `Insert` carries the full document,
/// `Update` carries only the fields that changed (removed fields use
/// `Value::Null` as a tombstone), and `Delete` carries only the id.
#[derive(Debug, Clone)]
pub enum SyncEvent {
    Insert {
        collection: String,
        id: String,
        document: Document,
    },
    Update {
        collection: String,
        id: String,
        /// Changed fields only. Removed fields also appear here with
        /// `Value::Null` for backward compatibility with older receivers.
        changes: HashMap<String, Value>,
        /// Names of fields that were removed (unset) by this update.
        /// Disambiguates "field removed" from "field set to null".
        removed: Vec<String>,
    },
    Delete {
        collection: String,
        id: String,
    },
}

/// Receiver for post-commit mutation events.
///
/// Implementations **must be non-blocking**. Long-running work (HTTP requests,
/// disk I/O) must be offloaded to a background thread or async task inside
/// `on_event` — the call happens on the writer thread after commit returns.
///
/// `Arc<dyn SyncHook>` is stored inside `Collection` so one hook instance can
/// be shared across many collections.
pub trait SyncHook: Send + Sync {
    fn on_event(&self, event: SyncEvent);
}

/// No-operation sync hook. Default when sync is disabled — zero overhead.
pub struct NoopSyncHook;

impl SyncHook for NoopSyncHook {
    #[inline]
    fn on_event(&self, _event: SyncEvent) {}
}

// ---------------------------------------------------------------------------
// Test helper — RecordingSyncHook
// ---------------------------------------------------------------------------

/// Captures every event for use in unit tests.
///
/// Available only when `cfg(test)`.
#[cfg(test)]
pub struct RecordingSyncHook {
    events: std::sync::Mutex<Vec<SyncEvent>>,
}

#[cfg(test)]
impl RecordingSyncHook {
    pub fn new() -> Self {
        Self {
            events: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Drain and return all recorded events in the order they were received.
    pub fn take(&self) -> Vec<SyncEvent> {
        self.events.lock().unwrap().drain(..).collect()
    }

    /// Number of events recorded so far without draining.
    pub fn len(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.lock().unwrap().is_empty()
    }
}

#[cfg(test)]
impl Default for RecordingSyncHook {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl SyncHook for RecordingSyncHook {
    fn on_event(&self, event: SyncEvent) {
        self.events.lock().unwrap().push(event);
    }
}

// ---------------------------------------------------------------------------
// Changeset types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChangeOp {
    /// Insert or replace a document.
    Upsert(Document),
    /// Delete a document by ID.
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Change {
    pub collection: String,
    pub id: Ulid,
    pub op: ChangeOp,
    /// Wall-clock timestamp in milliseconds since Unix epoch.
    pub changed_at: u64,
}

/// An ordered list of document changes to exchange between replicas.
pub type Changeset = Vec<Change>;

// ---------------------------------------------------------------------------
// Tolerant import-time validation (schema-evolution safety net)
// ---------------------------------------------------------------------------
//
// Sync import is the boundary TalaDB does NOT control: the peer may run an
// older or newer app version, so a foreign-shaped or malformed document can
// arrive at any time. A local-first, Last-Write-Wins engine must never
// hard-reject on this boundary — doing so either silently loses a legitimately
// newer write or wedges the whole pull batch. Instead the import path consults
// an optional [`ImportValidator`] that returns one of three *tolerant*
// decisions per document. The batch always runs to completion.
//
// The validator carries the *policy* (a schema parse, a `_v` migration,
// coercion of a below-current shape); the core carries only the *mechanism*
// (call the hook, honour its decision, keep going, retain rejects). This keeps
// the JS-side schema (Zod/Valibot) and per-document `_v` migration where they
// belong while still enforcing "validate, never cast" inside `import_changes`
// itself rather than only at a higher layer.

/// What an [`ImportValidator`] decides to do with one incoming upsert.
pub enum ImportDecision {
    /// Store the document exactly as received, subject to the usual
    /// Last-Write-Wins comparison.
    Accept,
    /// Store this normalized form instead of the received document — e.g. after
    /// running a `_v` migration, filling defaults, or coercing a field. Subject
    /// to the same Last-Write-Wins comparison.
    Coerce(Document),
    /// Drop this document silently — it is not our concern (e.g. a collection
    /// this client does not model). Counted in [`ImportReport::skipped`].
    Skip,
    /// Set the document aside in the collection's quarantine table with the
    /// given human-readable reason, rather than applying or discarding it.
    /// Counted in [`ImportReport::quarantined`]. Recoverable by an operator or
    /// a later migration.
    Quarantine(String),
}

/// A per-document gate consulted on every imported upsert.
///
/// Implementations MUST be deterministic and side-effect free: two replicas
/// running the *same* validator over the *same* document must reach the same
/// [`ImportDecision`], or the databases will diverge. This is the same
/// determinism contract Last-Write-Wins already relies on. Delete changes are
/// never passed through the validator — a tombstone has no shape to check.
pub trait ImportValidator: Send + Sync {
    /// Inspect one imported document. `collection` is the target collection
    /// name; `doc` is the remote document (id preserved). Return
    /// [`ImportDecision::Coerce`] to substitute a normalized form.
    fn check(&self, collection: &str, doc: &Document) -> ImportDecision;
}

/// Outcome counts for a validated import pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Documents actually written locally (upserted or deleted) after LWW.
    pub applied: u64,
    /// Changes not applied: either the validator asked to
    /// [`ImportDecision::Skip`], or the change carried a `changed_at` too far
    /// ahead of the local clock to be usable as an ordering token (see
    /// [`MAX_CLOCK_SKEW_MS`]).
    pub skipped: u64,
    /// Upserts diverted to the quarantine table via
    /// [`ImportDecision::Quarantine`].
    pub quarantined: u64,
}

/// A document set aside by the import validator, retained for later recovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuarantineRecord {
    pub document: Document,
    pub reason: String,
    /// The `changed_at` the rejected change carried, so a later replay can
    /// still honour Last-Write-Wins ordering.
    pub changed_at: u64,
}

// ---------------------------------------------------------------------------
// Built-in structural validator
// ---------------------------------------------------------------------------
//
// A minimal, deterministic [`ImportValidator`] that language bindings can drive
// from a per-collection schema descriptor. It is deliberately *structural* (not
// a full Zod/Valibot equivalent): the import boundary can only safely do
// structural checks + tolerant normalization, while the rich schema stays on
// the strict, local `insert` path. See docs/guide/schema-and-sync-standards.md.

/// Expected primitive shape of one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    Bool,
    Int,
    Float,
    Str,
    Bytes,
    Array,
    Object,
    /// Accepts any value — use to require presence without constraining type.
    Any,
}

impl FieldType {
    /// Whether `value` satisfies this type. `Null` always passes (a present but
    /// null optional field is not a type error; required-ness is checked
    /// separately).
    pub fn matches(self, value: &Value) -> bool {
        matches!(
            (self, value),
            (_, Value::Null)
                | (Self::Any, _)
                | (Self::Bool, Value::Bool(_))
                | (Self::Int, Value::Int(_))
                | (Self::Float, Value::Float(_))
                | (Self::Str, Value::Str(_))
                | (Self::Bytes, Value::Bytes(_))
                | (Self::Array, Value::Array(_))
                | (Self::Object, Value::Object(_))
        )
    }
}

/// A per-collection structural schema consulted on import.
///
/// A document whose `_v` is **below** [`version`](Self::version) is upgraded in
/// place (missing `defaults` filled, `_v` stamped) rather than rejected —
/// additive-only migration. A document whose `_v` is **above** the known
/// version is accepted untouched (the peer is ahead, not wrong). Otherwise the
/// document is checked: a missing/null [`required`](Self::required) field or a
/// [`types`](Self::types) mismatch quarantines it; extra fields are allowed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StructuralSchema {
    /// Current document shape version. `0` disables the migration step.
    #[serde(default)]
    pub version: i64,
    /// Fields that must be present and non-null.
    #[serde(default)]
    pub required: Vec<String>,
    /// Expected primitive type per field. Fields absent here accept any type.
    #[serde(default)]
    pub types: HashMap<String, FieldType>,
    /// Values applied to missing fields when upgrading a below-version document.
    #[serde(default)]
    pub defaults: Vec<(String, Value)>,
    /// Field renames `(from, to)` applied when upgrading a below-version
    /// document: if `from` is present and `to` is absent, the value moves from
    /// `from` to `to`. Applied before [`defaults`](Self::defaults), so a rename
    /// takes precedence over a default for the same target field.
    #[serde(default)]
    pub renames: Vec<(String, String)>,
}

impl StructuralSchema {
    /// Reject a schema whose migration directives can never run.
    ///
    /// [`renames`](Self::renames) and [`defaults`](Self::defaults) are only
    /// applied to a document whose `_v` is *below* [`version`](Self::version).
    /// With `version == 0` the migration step is skipped entirely — but
    /// [`required`](Self::required) and [`types`](Self::types) still apply, so a
    /// rename schema declared without a version would quarantine every single
    /// document it was written to upgrade. Fail loudly at schema-build time
    /// instead.
    pub fn validate(&self) -> Result<(), TalaDbError> {
        if self.version == 0 && !(self.renames.is_empty() && self.defaults.is_empty()) {
            return Err(TalaDbError::Config(
                "sync schema declares `renames`/`defaults` but no `version`: the import \
                 migration step only runs for documents below `version`, so with version 0 \
                 they would never be applied and documents missing those fields would be \
                 quarantined instead of upgraded"
                    .into(),
            ));
        }
        if self.version < 0 {
            return Err(TalaDbError::Config(
                "sync schema `version` must not be negative".into(),
            ));
        }
        Ok(())
    }

    /// Evaluate one document against this schema, returning a tolerant decision.
    pub fn evaluate(&self, doc: &Document) -> ImportDecision {
        let doc_v = doc.get("_v").and_then(Value::as_int).unwrap_or(0);

        // Peer is ahead of us — accept untouched rather than reject.
        if self.version != 0 && doc_v > self.version {
            return ImportDecision::Accept;
        }

        // Below current version: additive upgrade (rename, fill defaults, stamp `_v`).
        let coerced = if self.version != 0 && doc_v < self.version {
            let mut up = doc.clone();
            for (from, to) in &self.renames {
                if up.contains_key(from)
                    && !up.contains_key(to)
                    && let Some(v) = up.remove(from)
                {
                    up.set(to.clone(), v);
                }
            }
            for (k, def) in &self.defaults {
                if !up.contains_key(k) {
                    up.set(k.clone(), def.clone());
                }
            }
            up.set("_v", Value::Int(self.version));
            Some(up)
        } else {
            None
        };

        let target = coerced.as_ref().unwrap_or(doc);

        for field in &self.required {
            match target.get(field) {
                None | Some(Value::Null) => {
                    return ImportDecision::Quarantine(format!("missing required field `{field}`"));
                }
                _ => {}
            }
        }
        for (field, ty) in &self.types {
            if let Some(v) = target.get(field)
                && !ty.matches(v)
            {
                return ImportDecision::Quarantine(format!(
                    "field `{field}` expected {ty:?}, got {}",
                    v.type_name()
                ));
            }
        }

        match coerced {
            Some(d) => ImportDecision::Coerce(d),
            None => ImportDecision::Accept,
        }
    }
}

/// An [`ImportValidator`] backed by a per-collection [`StructuralSchema`] map.
///
/// Collections with no registered schema are accepted as-is — import stays
/// tolerant by default and a peer syncing a collection this client does not
/// model is never spuriously quarantined.
pub struct SchemaValidator {
    schemas: HashMap<String, StructuralSchema>,
}

impl SchemaValidator {
    pub fn new(schemas: HashMap<String, StructuralSchema>) -> Self {
        Self { schemas }
    }

    /// Build a validator, rejecting any schema whose directives can never run
    /// (see [`StructuralSchema::validate`]). Prefer this over [`new`](Self::new)
    /// at every binding boundary: a schema that silently does nothing is worse
    /// than one that fails to load, because the documents it was meant to
    /// upgrade get quarantined instead.
    pub fn try_new(schemas: HashMap<String, StructuralSchema>) -> Result<Self, TalaDbError> {
        for (collection, schema) in &schemas {
            schema.validate().map_err(|e| match e {
                TalaDbError::Config(msg) => {
                    TalaDbError::Config(format!("collection `{collection}`: {msg}"))
                }
                other => other,
            })?;
        }
        Ok(Self { schemas })
    }
}

impl ImportValidator for SchemaValidator {
    fn check(&self, collection: &str, doc: &Document) -> ImportDecision {
        match self.schemas.get(collection) {
            Some(schema) => schema.evaluate(doc),
            None => ImportDecision::Accept,
        }
    }
}

// ---------------------------------------------------------------------------
// SyncAdapter trait
// ---------------------------------------------------------------------------

/// Interface for syncing a TalaDB database with a remote peer.
pub trait SyncAdapter: Send + Sync {
    /// Export all changes that occurred after `since_ms` (exclusive).
    /// Returns a `Changeset` that can be sent to a remote peer.
    fn export_changes(
        &self,
        db: &crate::Database,
        collections: &[&str],
        since_ms: u64,
    ) -> Result<Changeset, TalaDbError>;

    /// Import a remote `Changeset` and merge it into the local database.
    /// Returns the number of documents actually changed (upserted or deleted).
    fn import_changes(
        &self,
        db: &crate::Database,
        changeset: Changeset,
    ) -> Result<u64, TalaDbError>;
}

// ---------------------------------------------------------------------------
// LastWriteWins implementation
// ---------------------------------------------------------------------------

/// Resolves conflicts by keeping the change with the highest `changed_at`
/// timestamp. Equal-timestamp upserts are broken by comparing serialized
/// document bytes; deletes win ties against upserts.
///
/// Optionally holds an [`ImportValidator`] (see [`with_validator`]) that gates
/// every imported upsert before the Last-Write-Wins comparison. With no
/// validator attached, import is unvalidated (the historical behaviour).
///
/// [`with_validator`]: LastWriteWins::with_validator
#[derive(Default)]
pub struct LastWriteWins {
    validator: Option<Arc<dyn ImportValidator>>,
}

impl LastWriteWins {
    pub fn new() -> Self {
        Self { validator: None }
    }

    /// Attach a tolerant import-time validator. Every upsert imported through
    /// this adapter is passed to `validator.check()`; deletes are unaffected.
    pub fn with_validator(mut self, validator: Arc<dyn ImportValidator>) -> Self {
        self.validator = Some(validator);
        self
    }
}

impl SyncAdapter for LastWriteWins {
    fn export_changes(
        &self,
        db: &crate::Database,
        collections: &[&str],
        since_ms: u64,
    ) -> Result<Changeset, TalaDbError> {
        let mut changes = Vec::new();

        for &col_name in collections {
            let col = db.collection(col_name)?;
            // Use the _changed_at secondary index for an O(log N) range scan
            // instead of a full table scan. The index is auto-created on first
            // mutation by ensure_changed_at_index().
            let docs = if since_ms == 0 {
                col.find(Filter::All)?
            } else {
                col.find(Filter::Gt(
                    "_changed_at".into(),
                    Value::Int(since_ms as i64),
                ))?
            };
            for doc in docs {
                // Documents replicated *in* from an authoritative origin are not
                // local edits and must never be replicated back out — that would
                // hand the origin its own catalog as if the user had authored it.
                // The check has to live here, not in the sync hook: when the cursor
                // is 0 (every pass today, since cursors are stubbed) the branch
                // above exports the whole collection via `find(Filter::All)`, which
                // no hook suppression can reach.
                if doc.get(crate::collection::REMOTE_ORIGIN_FIELD) == Some(&Value::Bool(true)) {
                    continue;
                }

                let changed_at = doc_changed_at(&doc);

                changes.push(Change {
                    collection: col_name.to_string(),
                    id: doc.id,
                    op: ChangeOp::Upsert(doc),
                    changed_at,
                });
            }

            // Export tombstones so remote replicas learn about deletions.
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
                        "sync: corrupt tombstone timestamp, defaulting to epoch (0)"
                    );
                    0
                });
                let changed_at = timestamp_from_i64(ts);
                if changed_at > since_ms {
                    let mut id_arr = [0u8; 16];
                    id_arr.copy_from_slice(&key_bytes);
                    let id = Ulid::from_bytes(id_arr);
                    changes.push(Change {
                        collection: col_name.to_string(),
                        id,
                        op: ChangeOp::Delete,
                        changed_at,
                    });
                }
            }
        }

        Ok(changes)
    }

    fn import_changes(
        &self,
        db: &crate::Database,
        changeset: Changeset,
    ) -> Result<u64, TalaDbError> {
        // Preserve the historical return contract (documents applied) while the
        // full tolerant outcome is available via [`LastWriteWins::import_report`].
        self.import_report(db, changeset).map(|r| r.applied)
    }
}

impl LastWriteWins {
    /// Merge a remote changeset, returning the full [`ImportReport`] (applied /
    /// skipped / quarantined). This is the tolerant import path: when a
    /// validator is attached, a document that fails validation is normalized,
    /// skipped, or quarantined — never dropped silently and never able to abort
    /// the batch. Storage/serialization errors still propagate as `Err`.
    pub fn import_report(
        &self,
        db: &crate::Database,
        changeset: Changeset,
    ) -> Result<ImportReport, TalaDbError> {
        let mut report = ImportReport::default();
        // Rejects are collected here and written in one transaction after the
        // batch, rather than one fsync'd commit per document.
        let mut rejects: Vec<(String, Ulid, QuarantineRecord)> = Vec::new();

        for change in changeset {
            // An unusable ordering token is rejected before the change can
            // influence anything, on both the upsert and delete paths.
            //
            // Skipping rather than erroring is deliberate: this is the tolerant
            // import path, and letting one malformed row abort the whole
            // changeset would just trade a data-integrity bug for a
            // denial-of-service one. The count surfaces on `ImportReport`.
            if !changed_at_is_plausible(change.changed_at) {
                tracing::warn!(
                    collection = %change.collection,
                    id = %change.id,
                    changed_at = change.changed_at,
                    "sync: skipping change whose changed_at is implausibly far in the \
                     future; it would outrank every local write permanently"
                );
                report.skipped += 1;
                continue;
            }

            let col = db.collection(&change.collection)?;

            match change.op {
                ChangeOp::Upsert(remote_doc) => {
                    // Tolerant validation gate. Runs *before* any Last-Write-Wins
                    // comparison so a foreign- or old-shaped document is
                    // normalized, set aside, or skipped rather than cast blindly
                    // into local storage. No validator attached => accept as-is
                    // (historical behaviour).
                    let remote_doc = match &self.validator {
                        None => remote_doc,
                        Some(v) => match v.check(&change.collection, &remote_doc) {
                            ImportDecision::Accept => remote_doc,
                            ImportDecision::Coerce(normalized) => normalized,
                            ImportDecision::Skip => {
                                report.skipped += 1;
                                continue;
                            }
                            ImportDecision::Quarantine(reason) => {
                                rejects.push((
                                    change.collection.clone(),
                                    change.id,
                                    QuarantineRecord {
                                        document: remote_doc,
                                        reason,
                                        changed_at: change.changed_at,
                                    },
                                ));
                                report.quarantined += 1;
                                continue;
                            }
                        },
                    };

                    // Look up local version of the document by ULID.
                    // _id is the Document::id field, not a field in the fields vec,
                    // so Filter::Eq("_id", ...) would never match. Use find_by_id instead.
                    let local = col.find_by_id(change.id)?;

                    let should_apply = match &local {
                        None => {
                            // No live document — but a local tombstone may
                            // record a newer deletion. Only resurrect when the
                            // remote upsert is strictly newer than the
                            // deletion (deletes win ties, matching the Delete
                            // import path below).
                            let tomb_ts = read_tombstone_ts(db, &change.collection, change.id)?
                                .map_or(0, timestamp_from_i64);
                            change.changed_at > tomb_ts
                        }
                        Some(local_doc) => {
                            // Absent, wrong-typed, or negative `_changed_at`
                            // floors to 0 — the document always loses conflicts.
                            let local_ts = doc_changed_at(local_doc);
                            // Remote wins if newer. Equal timestamps are broken
                            // by comparing the serialized document bytes — both
                            // replicas evaluate the same comparison, so they
                            // converge on the same winner without coordination.
                            change.changed_at > local_ts
                                || (change.changed_at == local_ts
                                    && doc_tie_break_wins(&remote_doc, local_doc)?)
                        }
                    };

                    if should_apply {
                        // Atomically replace (or insert) the remote doc
                        // preserving its ULID. This also clears any tombstone
                        // for the ID so the replace is not later exported as a
                        // deletion.
                        col.replace_with_id(remote_doc)?;
                        report.applied += 1;
                    }
                }

                ChangeOp::Delete => {
                    // Last-write-wins: only delete when the remote deletion is
                    // at least as new as the local document. A stale tombstone
                    // must not destroy a newer local write. Deletes win ties so
                    // concurrent upsert/delete at the same millisecond resolve
                    // identically on every replica.
                    let should_delete = match col.find_by_id(change.id)? {
                        None => false,
                        Some(local_doc) => change.changed_at >= doc_changed_at(&local_doc),
                    };
                    let existed = if should_delete {
                        col.delete_by_id_at(change.id, change.changed_at)?
                    } else {
                        false
                    };

                    // Always upsert the tombstone — even if the doc was already
                    // absent — so this replica can forward the deletion to any
                    // downstream peers that haven't seen it yet.
                    let tomb_table = tomb_table_name(&change.collection);
                    let mut wtxn = db.backend().begin_write()?;
                    // Infallible given the plausibility check above, which caps
                    // `changed_at` far below `i64::MAX`. Converting rather than
                    // casting keeps it that way: an `as i64` here would wrap a
                    // large value negative and silently skip the tombstone
                    // write, so the deletion would apply locally but never
                    // reach downstream peers.
                    let deleted_at = i64::try_from(change.changed_at).map_err(|_| {
                        TalaDbError::InvalidOperation("deletion timestamp exceeds i64::MAX".into())
                    })?;
                    let ts_bytes = postcard::to_allocvec(&deleted_at)?;
                    let existing_ts: i64 = wtxn
                        .get(&tomb_table, &change.id.to_bytes())?
                        .and_then(|b| postcard::from_bytes::<i64>(&b).ok())
                        .unwrap_or(0);
                    if deleted_at > existing_ts {
                        wtxn.put(&tomb_table, &change.id.to_bytes(), &ts_bytes)?;
                    }
                    wtxn.commit()?;

                    if existed {
                        report.applied += 1;
                    }
                }
            }
        }

        quarantine_documents(db, rejects)?;

        Ok(report)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write every validator-rejected document of an import batch into its
/// collection's quarantine table, keyed by ULID, in a **single** transaction.
/// Overwrites any earlier quarantine record for the same id (the latest
/// rejection is the useful one). Documents are retained verbatim so an operator
/// or a later migration can recover or reprocess them.
///
/// One commit for the whole batch, not one per reject: under the default
/// `flush_every_write` durability each commit is an fsync, and a per-document
/// commit also left the quarantine table holding an arbitrary prefix of the
/// batch if the process died midway.
fn quarantine_documents(
    db: &crate::Database,
    rejects: Vec<(String, Ulid, QuarantineRecord)>,
) -> Result<(), TalaDbError> {
    if rejects.is_empty() {
        return Ok(());
    }
    let mut wtxn = db.backend().begin_write()?;
    for (collection, id, record) in rejects {
        let bytes = postcard::to_allocvec(&record)?;
        wtxn.put(&quarantine_table_name(&collection), &id.to_bytes(), &bytes)?;
    }
    wtxn.commit()?;
    Ok(())
}

/// Read the tombstone timestamp for `id` in `collection`, if one exists.
fn read_tombstone_ts(
    db: &crate::Database,
    collection: &str,
    id: Ulid,
) -> Result<Option<i64>, TalaDbError> {
    let tomb_table = tomb_table_name(collection);
    let rtxn = db.backend().begin_read()?;
    Ok(rtxn
        .get(&tomb_table, &id.to_bytes())?
        .and_then(|b| postcard::from_bytes::<i64>(&b).ok()))
}

/// Deterministic tie-break for equal `changed_at` timestamps: the document
/// with the lexicographically greater postcard serialization wins. Both
/// replicas compare the same two byte strings, so they always pick the same
/// winner — guaranteeing convergence without a coordinator or per-replica IDs.
fn doc_tie_break_wins(remote: &Document, local: &Document) -> Result<bool, TalaDbError> {
    let remote_bytes = postcard::to_allocvec(remote)?;
    let local_bytes = postcard::to_allocvec(local)?;
    Ok(remote_bytes > local_bytes)
}

/// How far ahead of the local clock an incoming `changed_at` may legitimately
/// sit before it is treated as corrupt rather than merely skewed.
///
/// Sync timestamps are wall-clock milliseconds taken on whichever device
/// authored the write, so some skew is normal and must be tolerated — an
/// unsynchronised phone can be minutes or hours out. A full day is far beyond
/// any plausible skew while still being astronomically below the values that
/// cause the damage this bound exists to prevent.
pub const MAX_CLOCK_SKEW_MS: u64 = 24 * 60 * 60 * 1000;

/// Interpret a stored `_changed_at` (or tombstone) value as a millisecond
/// timestamp.
///
/// Sync timestamps are *stored* as `Value::Int(i64)` but *compared* as `u64`,
/// and a plain `as u64` maps every negative value onto `u64::MAX` — the largest
/// timestamp that exists. A single document carrying `_changed_at: -1` would
/// therefore win every conflict on every replica, permanently: no local edit
/// (stamped `now_ms()`) could ever displace it, and each replica would keep
/// re-sending it to the others.
///
/// A negative value is not a timestamp we can order, so it floors to the epoch
/// — which always *loses* instead of always winning. Failing towards "this
/// write is ancient" is the safe direction: the worst case is that a corrupt
/// row is overwritten by a good one.
fn timestamp_from_i64(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}

/// Read a document's `_changed_at` as a millisecond timestamp, defaulting to
/// the epoch when it is absent, the wrong type, or negative (see
/// [`timestamp_from_i64`]). A document with no usable timestamp always loses a
/// conflict.
fn doc_changed_at(doc: &Document) -> u64 {
    match doc.get("_changed_at") {
        Some(Value::Int(ts)) => timestamp_from_i64(*ts),
        _ => 0,
    }
}

/// Whether an incoming change's `changed_at` is usable as an ordering token.
///
/// Everything downstream of Last-Write-Wins trusts this number to decide which
/// version of a document survives, and it arrives from a peer. A value far
/// enough in the future outranks every timestamp the local clock can ever
/// produce, which makes the change permanently unbeatable rather than merely
/// newest — one such row freezes a document across the whole fleet.
///
/// Bounding it against the local clock (plus [`MAX_CLOCK_SKEW_MS`]) keeps
/// ordinary skew working while making that outcome unreachable. This also
/// subsumes the `> i64::MAX` case that
/// [`Collection::delete_by_id_at`](crate::collection::Collection) rejects, so
/// that guard now only ever fires for direct callers.
fn changed_at_is_plausible(changed_at: u64) -> bool {
    changed_at <= now_ms().saturating_add(MAX_CLOCK_SKEW_MS)
}

/// Current wall-clock time in milliseconds since Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Stamp a document field `_changed_at` with the current wall-clock time.
/// Call this before inserting/updating a document that participates in sync.
pub fn stamp(fields: &mut Vec<(String, Value)>) {
    fields.retain(|(k, _)| k != "_changed_at");
    fields.push(("_changed_at".into(), Value::Int(now_ms() as i64)));
}
