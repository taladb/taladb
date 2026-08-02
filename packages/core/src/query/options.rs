//! Query options — sort, pagination, and projection for `find_with_options`.

use std::cmp::Ordering;

use crate::document::{Document, Value};

// ---------------------------------------------------------------------------
// Sort
// ---------------------------------------------------------------------------

/// Direction for a sort key.
#[derive(Debug, Clone, PartialEq)]
pub enum SortDirection {
    /// Ascending (A→Z, 0→9, false→true).
    Asc,
    /// Descending (Z→A, 9→0, true→false).
    Desc,
}

/// A single sort key: field name + direction.
#[derive(Debug, Clone)]
pub struct SortSpec {
    pub field: String,
    pub direction: SortDirection,
}

impl SortSpec {
    pub fn asc(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            direction: SortDirection::Asc,
        }
    }
    pub fn desc(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            direction: SortDirection::Desc,
        }
    }
}

// ---------------------------------------------------------------------------
// FindOptions
// ---------------------------------------------------------------------------

/// Options for `Collection::find_with_options`.
///
/// All fields are optional; an empty `FindOptions` is equivalent to calling
/// the plain `find` method.
///
/// ```
/// use taladb_core::query::options::{FindOptions, SortSpec};
///
/// let opts = FindOptions {
///     sort: vec![SortSpec::desc("createdAt"), SortSpec::asc("name")],
///     skip: 20,
///     limit: Some(10),
///     fields: Some(vec!["name".into(), "email".into()]),
///     timeout: Some(std::time::Duration::from_secs(5)),
/// };
/// ```
#[derive(Debug, Default, Clone)]
pub struct FindOptions {
    /// Sort order — applied after filtering, before skip/limit.
    /// Multiple specs are applied left-to-right (primary key first).
    pub sort: Vec<SortSpec>,
    /// Number of documents to skip from the front of the result set.
    pub skip: u64,
    /// Maximum number of documents to return (`None` = unlimited).
    pub limit: Option<u64>,
    /// If `Some`, only the listed fields (plus `_id`) are returned.
    /// Fields not in the list are stripped from each document.
    pub fields: Option<Vec<String>>,
    /// If `Some`, the query will return [`TalaDbError::QueryTimeout`] if
    /// it runs longer than the specified duration.  The check is performed
    /// between documents in the filter loop, so the actual elapsed time may
    /// slightly exceed the limit on very large individual documents.
    pub timeout: Option<std::time::Duration>,
}

// ---------------------------------------------------------------------------
// Sort comparison
// ---------------------------------------------------------------------------

/// Compare two `Value`s for ordering.
///
/// Ordering is defined as:
///   Null < Bool < Int/Float (numeric) < Str < Bytes < Array < Object
///
/// Mixed numeric types (Int vs Float) are compared numerically.
pub(crate) fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,

        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Bool(_), _) => Ordering::Less,
        (_, Value::Bool(_)) => Ordering::Greater,

        // Numeric: Int and Float compare numerically across types
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
        (Value::Int(_) | Value::Float(_), _) => Ordering::Less,
        (_, Value::Int(_) | Value::Float(_)) => Ordering::Greater,

        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Str(_), _) => Ordering::Less,
        (_, Value::Str(_)) => Ordering::Greater,

        (Value::Bytes(x), Value::Bytes(y)) => x.cmp(y),
        (Value::Bytes(_), _) => Ordering::Less,
        (_, Value::Bytes(_)) => Ordering::Greater,

        // Arrays and objects — compare by debug representation (stable but not meaningful)
        _ => Ordering::Equal,
    }
}

/// One document's sort keys, lifted out ahead of the sort.
///
/// Sorting `Document`s directly is deceptively expensive: every comparison
/// re-resolves each sort field by scanning the document's field list and
/// comparing key strings, and every swap moves the whole struct. Both costs are
/// paid O(n log n) times. Extracting the keys once (decorate-sort-undecorate)
/// turns each comparison into a couple of `Value` compares over a small, cheap
/// row, and the sort then shuffles these rows instead of the documents.
struct KeyRow {
    /// Values of the sort fields, in spec order. `None` = field absent.
    keys: Vec<Option<Value>>,
    /// Tiebreaker, so the ordering is total (see `cmp_rows`).
    id: ulid::Ulid,
    /// Position of the source document in the input vector.
    idx: usize,
}

fn key_rows(docs: &[Document], sort: &[SortSpec]) -> Vec<KeyRow> {
    docs.iter()
        .enumerate()
        .map(|(idx, d)| KeyRow {
            keys: sort.iter().map(|s| d.get(&s.field).cloned()).collect(),
            id: d.id,
            idx,
        })
        .collect()
}

/// Total order over pre-extracted keys. Mirrors `cmp_by_spec`, including the
/// `_id` tiebreak that makes a bounded sort agree with a full one.
fn cmp_rows(a: &KeyRow, b: &KeyRow, sort: &[SortSpec]) -> Ordering {
    for (i, spec) in sort.iter().enumerate() {
        let ord = match (&a.keys[i], &b.keys[i]) {
            (Some(x), Some(y)) => cmp_values(x, y),
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        let ord = if spec.direction == SortDirection::Desc {
            ord.reverse()
        } else {
            ord
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.id.cmp(&b.id)
}

/// Rebuild `docs` in the order given by `rows`, moving (never cloning) each
/// document into its new slot.
fn gather(docs: &mut Vec<Document>, rows: &[KeyRow]) {
    let mut slots: Vec<Option<Document>> = std::mem::take(docs).into_iter().map(Some).collect();
    *docs = rows
        .iter()
        .map(|r| slots[r.idx].take().expect("each index is visited once"))
        .collect();
}

pub fn sort_documents(docs: &mut Vec<Document>, sort: &[SortSpec]) {
    if sort.is_empty() || docs.len() < 2 {
        return;
    }
    let mut rows = key_rows(docs, sort);
    rows.sort_by(|a, b| cmp_rows(a, b, sort));
    gather(docs, &rows);
}

/// Sort only the first `keep` documents — the rest are discarded.
///
/// Complexity: O(n + k log k) rather than O(n log n), where n = docs.len() and
/// k = keep. Use when the caller can only ever observe the leading `keep`
/// documents (`find` with sort + limit; `$sort` followed by `$skip`/`$limit`),
/// instead of ordering the whole set and throwing nearly all of it away.
///
/// Because `cmp_rows` is a **total** order (ties broken on the unique `_id`),
/// the result is exactly the first `keep` documents of a full sort — so paging
/// with a growing `keep` stays consistent and never repeats or drops a document.
///
/// Returns with `docs.len() == min(keep, docs.len())`.
pub fn partial_sort_documents(docs: &mut Vec<Document>, sort: &[SortSpec], keep: usize) {
    if keep == 0 {
        docs.clear();
        return;
    }
    if sort.is_empty() {
        docs.truncate(keep);
        return;
    }
    let mut rows = key_rows(docs, sort);
    if keep < rows.len() {
        // Partition so the first `keep` rows are the `keep` smallest (in
        // arbitrary order among themselves), discard the rest, then order those.
        rows.select_nth_unstable_by(keep - 1, |a, b| cmp_rows(a, b, sort));
        rows.truncate(keep);
    }
    rows.sort_by(|a, b| cmp_rows(a, b, sort));
    gather(docs, &rows);
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// Apply a projection to a document: keep only the listed fields (plus `_id`).
pub fn project_document(mut doc: Document, fields: &[String]) -> Document {
    doc.fields.retain(|(k, _)| fields.iter().any(|f| f == k));
    doc
}
