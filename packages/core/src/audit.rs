//! Append-only audit log for TalaDB.
//!
//! When enabled on a [`Collection`] via [`Collection::with_audit_log`], every
//! successful mutation (`insert`, `insert_many`, `update_one`, `update_many`,
//! `delete_one`, `delete_many`) appends a structured entry to the `_audit`
//! collection stored in the same database.
//!
//! ## Audit entry fields
//! | Field        | Type  | Description                                   |
//! |--------------|-------|-----------------------------------------------|
//! | `_id`        | Ulid  | Unique audit entry ID (auto-generated)        |
//! | `collection` | Str   | Name of the collection that was mutated       |
//! | `op`         | Str   | Operation: `"insert"`, `"update"`, `"delete"` |
//! | `doc_id`     | Str   | ULID of the document affected                 |
//! | `ts`         | Int   | Wall-clock timestamp (ms since Unix epoch)    |
//! | `caller`     | Str   | Caller identity string (app-supplied)         |
//!
//! ## Append-only guarantee
//! The audit table is intentionally **not** exposed via the normal
//! [`Collection`] API.  There is no `update` or `delete` path for audit
//! records.  Records can only be inserted by the audit machinery and read
//! via [`read_audit_log`].
//!
//! ## Usage
//! ```rust,ignore
//! let col = db.collection("orders")?
//!     .with_audit_log("service:checkout".into());
//!
//! col.insert(vec![("amount".into(), Value::Int(42))])?;
//! // → audit entry written: op="insert", caller="service:checkout"
//!
//! let entries = read_audit_log(db.backend(), None, None)?;
//! ```

use ulid::Ulid;

use crate::document::{Document, Value};
use crate::engine::StorageBackend;
use crate::error::TalaDbError;
use crate::time::now_ms;

/// Internal table name for audit log entries.
pub(crate) const AUDIT_TABLE: &str = "docs::_audit";

/// Operation type recorded in an audit entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditOp {
    Insert,
    Update,
    Delete,
}

impl AuditOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// A single audit log entry.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub id: Ulid,
    pub collection: String,
    pub op: AuditOp,
    pub doc_id: String,
    pub ts: i64,
    pub caller: String,
}

/// Write one audit entry to the `_audit` table within the mutation's own
/// write transaction, so the audit row commits atomically with the mutation:
/// a crash can never persist one without the other, and an audit failure
/// rolls the mutation back rather than leaving the API result ambiguous.
pub(crate) fn write_audit_entry(
    wtxn: &mut dyn crate::engine::WriteTxn,
    collection: &str,
    op: AuditOp,
    doc_id: &str,
    caller: &str,
) -> Result<(), TalaDbError> {
    let entry_id = crate::document::new_ulid_pub();
    let doc = Document {
        id: entry_id,
        fields: vec![
            ("collection".into(), Value::Str(collection.into())),
            ("op".into(), Value::Str(op.as_str().into())),
            ("doc_id".into(), Value::Str(doc_id.into())),
            ("ts".into(), Value::Int(now_ms() as i64)),
            ("caller".into(), Value::Str(caller.into())),
        ],
    };
    let doc_bytes = postcard::to_allocvec(&doc)?;
    wtxn.put(AUDIT_TABLE, &entry_id.to_bytes(), &doc_bytes)?;
    Ok(())
}

/// Read all audit log entries, optionally filtered by collection and/or
/// operation type.  Entries are returned in insertion order (ULID-sorted).
///
/// `collection_filter` — if `Some`, only entries for that collection are
/// returned.
/// `op_filter` — if `Some`, only entries with that operation are returned.
pub fn read_audit_log(
    backend: &dyn StorageBackend,
    collection_filter: Option<&str>,
    op_filter: Option<AuditOp>,
) -> Result<Vec<AuditEntry>, TalaDbError> {
    read_audit_log_since(backend, None, collection_filter, op_filter)
}

/// Like [`read_audit_log`], but skips entries with `id <= since`.  This
/// uses a ULID range scan, so tailing the log with `since = last_seen_id`
/// is O(new_entries) instead of O(total_entries).
pub fn read_audit_log_since(
    backend: &dyn StorageBackend,
    since: Option<Ulid>,
    collection_filter: Option<&str>,
    op_filter: Option<AuditOp>,
) -> Result<Vec<AuditEntry>, TalaDbError> {
    let rtxn = backend.begin_read()?;
    let pairs = match since {
        Some(s) => {
            let start_bytes = s.to_bytes();
            rtxn.range(
                AUDIT_TABLE,
                std::ops::Bound::Excluded(&start_bytes[..]),
                std::ops::Bound::Unbounded,
            )
            .unwrap_or_default()
        }
        None => rtxn.scan_all(AUDIT_TABLE).unwrap_or_default(),
    };
    drop(rtxn);

    let mut entries = Vec::with_capacity(pairs.len());
    for (_, v) in pairs {
        let doc: Document = postcard::from_bytes(&v)?;

        let collection = match doc.get("collection") {
            Some(Value::Str(s)) => s.clone(),
            _ => continue,
        };
        let op_str = match doc.get("op") {
            Some(Value::Str(s)) => s.as_str(),
            _ => continue,
        };
        let op = match op_str {
            "insert" => AuditOp::Insert,
            "update" => AuditOp::Update,
            "delete" => AuditOp::Delete,
            _ => continue,
        };
        let doc_id = match doc.get("doc_id") {
            Some(Value::Str(s)) => s.clone(),
            _ => continue,
        };
        let ts = match doc.get("ts") {
            Some(Value::Int(i)) => *i,
            _ => 0,
        };
        let caller = match doc.get("caller") {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        };

        if let Some(cf) = collection_filter
            && collection != cf
        {
            continue;
        }
        if let Some(ref of_) = op_filter
            && &op != of_
        {
            continue;
        }

        entries.push(AuditEntry {
            id: doc.id,
            collection,
            op,
            doc_id,
            ts,
            caller,
        });
    }
    Ok(entries)
}
