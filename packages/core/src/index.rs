//! Secondary index key encoding.
//!
//! Index keys are structured as:
//!   [type_prefix: 1 byte] [encoded_value: N bytes] [ulid: 16 bytes]
//!
//! Variable-length types (Str, Bytes) use null-escape + terminator encoding so
//! range scans for exact-equality never spuriously include longer prefixes:
//!   • 0x00 inside the value → [0x00, 0xFF]
//!   • end of value          → [0x00]
//! Fixed-width types have known widths, so no terminator is needed.
//!
//! Type prefixes ensure cross-type sort order:
//!   0x00 = Null
//!   0x10 = Bool(false)
//!   0x11 = Bool(true)
//!   0x20 = Int  (i64 big-endian, XOR 0x8000_0000_0000_0000 for correct signed sort)
//!   0x30 = Float (IEEE 754 bits, sign-magnitude encoded for sort correctness)
//!   0x40 = Str  (null-escaped UTF-8 bytes + 0x00 terminator)
//!   0x50 = Bytes (null-escaped bytes + 0x00 terminator)
//!   0x60 = Array / Object (not indexable — skipped silently)
//!
//! Note that `Int` and `Float` carry *distinct* tags, so index order groups all
//! integers before all floats. The sort comparator (`query::options::cmp_values`)
//! instead compares the two numerically, so on a field holding both, index order
//! is **not** sort order — see [`TAG_INT`]. Readers that rely on index order
//! being sort order must handle that case.

use std::collections::HashSet;
use std::ops::Bound;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::document::Value;

/// Inclusive/exclusive byte-range bounds for an index scan.
pub type IndexBounds = Option<(Bound<Vec<u8>>, Bound<Vec<u8>>)>;

// ---------------------------------------------------------------------------
// Index key encoding
// ---------------------------------------------------------------------------

/// Type tag leading an encoded `Int` key.
///
/// `TAG_INT < TAG_FLOAT`, so a byte-ordered index scan yields every integer
/// before every float — while `cmp_values` orders the two numerically. A field
/// carrying both representations therefore has an index order that disagrees
/// with sort order, and any fast path substituting one for the other must first
/// rule that out (see `Collection::aggregate_via_sorted_index`).
pub(crate) const TAG_INT: u8 = 0x20;
/// Type tag leading an encoded `Float` key. See [`TAG_INT`].
pub(crate) const TAG_FLOAT: u8 = 0x30;

pub fn encode_index_key(value: &Value, id: Ulid) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    encode_value_prefix(value, &mut buf)?;
    buf.extend_from_slice(&id.to_bytes());
    Some(buf)
}

/// Every index key a document's value contributes — one per element for an
/// array, one for anything else, none for a value that cannot be indexed.
///
/// This is what makes an array *queryable*. `{ tags: ['rust', 'db'] }` is
/// indexed under both `'rust'` and `'db'`, so `find({ tags: 'rust' })` is an
/// ordinary index lookup that happens to land on a document whose field is a
/// list. Without per-element keys the array is unindexable, and a containment
/// match would find only the documents a full scan reached — silently missing
/// every document the planner answered from the index.
///
/// A document contributes at most one key per *distinct* element: repeated
/// elements encode identically (the key is `value ++ ulid`), so `['a', 'a']`
/// yields the single key `'a'`. Counting entries in an exact-value range is
/// therefore still counting documents, which `count_plan_entries` relies on.
///
/// Nested arrays and objects are skipped, as they are elsewhere: they have no
/// order-preserving byte encoding.
pub fn encode_index_keys(value: &Value, id: Ulid) -> Vec<Vec<u8>> {
    match value {
        Value::Array(items) => {
            // `seen` rather than `keys.contains(&key)`: the array length is
            // caller-controlled, and a linear scan inside the loop made one
            // insert quadratic in it. Measured on a debug build, a single
            // document went from 170 ms at 2,000 elements to 4.5 s at 16,000 —
            // a frozen UI in the browser or on a phone, from one write.
            //
            // `keys` still carries the output so element order is preserved.
            // Returning the set directly would be cheaper by one clone per key
            // but would hand back a nondeterministic order, which is a poor
            // trade for something this easy to debug into.
            let mut keys: Vec<Vec<u8>> = Vec::with_capacity(items.len());
            let mut seen: HashSet<Vec<u8>> = HashSet::with_capacity(items.len());
            for item in items {
                if let Some(key) = encode_index_key(item, id)
                    && seen.insert(key.clone())
                {
                    keys.push(key);
                }
            }
            keys
        }
        other => encode_index_key(other, id).into_iter().collect(),
    }
}

fn encode_value_prefix(value: &Value, buf: &mut Vec<u8>) -> Option<()> {
    match value {
        Value::Null => {
            buf.push(0x00);
        }
        Value::Bool(false) => {
            buf.push(0x10);
        }
        Value::Bool(true) => {
            buf.push(0x11);
        }
        Value::Int(n) => {
            buf.push(TAG_INT);
            // XOR with sign bit to make two's-complement sort as unsigned big-endian
            let sortable = (*n as u64) ^ 0x8000_0000_0000_0000u64;
            buf.extend_from_slice(&sortable.to_be_bytes());
        }
        Value::Float(f) => {
            buf.push(TAG_FLOAT);
            let bits = f.to_bits();
            // IEEE 754 sort: if sign bit set, flip all bits; else flip just sign bit
            let sortable = if bits >> 63 == 1 {
                !bits
            } else {
                bits ^ 0x8000_0000_0000_0000
            };
            buf.extend_from_slice(&sortable.to_be_bytes());
        }
        Value::Str(s) => {
            buf.push(0x40);
            for &b in s.as_bytes() {
                if b == 0x00 {
                    buf.extend_from_slice(&[0x00, 0xFF]);
                } else {
                    buf.push(b);
                }
            }
            buf.push(0x00); // terminator
        }
        Value::Bytes(b) => {
            buf.push(0x50);
            for &byte in b {
                if byte == 0x00 {
                    buf.extend_from_slice(&[0x00, 0xFF]);
                } else {
                    buf.push(byte);
                }
            }
            buf.push(0x00); // terminator
        }
        Value::Array(_) | Value::Object(_) => return None, // not indexable
    }
    Some(())
}

/// Build the range bounds for an index scan on an exact value.
/// Returns (start_inclusive, end_inclusive) byte ranges.
pub fn index_range_eq(value: &Value) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut start = Vec::new();
    encode_value_prefix(value, &mut start)?;
    let mut end = start.clone();
    // Append min/max ULID bytes to bracket the exact value
    start.extend_from_slice(&[0x00u8; 16]);
    end.extend_from_slice(&[0xFFu8; 16]);
    Some((start, end))
}

/// Build range bounds for gt/gte/lt/lte index scans.
pub fn index_range_cmp(
    lower: Option<(&Value, bool)>, // (value, inclusive)
    upper: Option<(&Value, bool)>,
) -> IndexBounds {
    let start = match lower {
        None => Bound::Unbounded,
        Some((v, inclusive)) => {
            let mut key = Vec::new();
            encode_value_prefix(v, &mut key)?;
            if inclusive {
                key.extend_from_slice(&[0x00u8; 16]);
                Bound::Included(key)
            } else {
                key.extend_from_slice(&[0xFFu8; 16]);
                Bound::Excluded(key)
            }
        }
    };

    let end = match upper {
        None => Bound::Unbounded,
        Some((v, inclusive)) => {
            let mut key = Vec::new();
            encode_value_prefix(v, &mut key)?;
            if inclusive {
                key.extend_from_slice(&[0xFFu8; 16]);
                Bound::Included(key)
            } else {
                key.extend_from_slice(&[0x00u8; 16]);
                Bound::Excluded(key)
            }
        }
    };

    Some((start, end))
}

/// Extract the ULID from an index key (last 16 bytes).
pub fn ulid_from_index_key(key: &[u8]) -> Option<Ulid> {
    if key.len() < 16 {
        return None;
    }
    let bytes: [u8; 16] = key[key.len() - 16..].try_into().ok()?;
    Some(Ulid::from_bytes(bytes))
}

// ---------------------------------------------------------------------------
// Index metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDef {
    pub collection: String,
    pub field: String,
}

pub fn meta_key(collection: &str, field: &str) -> String {
    format!("{collection}::{field}")
}

pub fn index_table_name(collection: &str, field: &str) -> String {
    format!("idx::{collection}::{field}")
}

pub fn docs_table_name(collection: &str) -> String {
    format!("docs::{collection}")
}

/// Table-name prefixes left behind by the replication engine removed in 0.11.
///
/// Nothing reads or writes these any more. They are named here only so the v1→v2
/// migration can find and drop them in databases created by an earlier release —
/// see [`crate::migration::BUILTIN_MIGRATIONS`].
pub(crate) const REMOVED_TABLE_PREFIXES: [&str; 2] = ["tomb::", "quarantine::"];

pub const META_INDEXES_TABLE: &str = "meta::indexes";
pub const META_COMPOUND_TABLE: &str = "meta::compound_indexes";
pub const META_VERSION_TABLE: &str = "meta::db_version";
pub const META_VERSION_KEY: &[u8] = b"version";

/// Stores the **application** migration version, distinct from the engine
/// storage-schema version in [`META_VERSION_TABLE`]. User-defined `openDB`
/// migrations advance this counter; built-in storage migrations never touch it.
pub const META_USER_VERSION_TABLE: &str = "meta::user_version";

// ---------------------------------------------------------------------------
// Compound index metadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompoundIndexDef {
    pub collection: String,
    /// Ordered list of fields in the compound key.
    pub fields: Vec<String>,
}

/// Meta-table key for a compound index: `"{collection}::{f1}::{f2}::..."`.
pub fn compound_meta_key(collection: &str, fields: &[&str]) -> String {
    format!("{}::{}", collection, fields.join("::"))
}

/// redb table for a compound index.
pub fn compound_table_name(collection: &str, fields: &[&str]) -> String {
    format!("cidx::{}::{}", collection, fields.join("::"))
}

// ---------------------------------------------------------------------------
// Compound index key encoding
//
// Key layout: [field1_encoded][field2_encoded]...[ulid: 16 B]
//
// Each field uses the same `encode_value_prefix` format as single-field
// indexes — the null-escape terminator for variable-length types is what
// allows safe concatenation of multiple fields without ambiguity.
// ---------------------------------------------------------------------------

/// Encode a compound index key from `values` (one per field) plus the ULID.
pub fn encode_compound_key(values: &[&Value], id: Ulid) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    for v in values {
        encode_value_prefix(v, &mut buf)?;
    }
    buf.extend_from_slice(&id.to_bytes());
    Some(buf)
}

/// Every compound key a document's values contribute.
///
/// A single array member expands to one key per element, so a compound index
/// answers `{ role: 'admin', tags: 'urgent' }` for a document whose `tags` is a
/// list — the same containment semantics the matcher applies. Without this, an
/// indexed compound query would return fewer documents than the identical query
/// on an unindexed collection, which is the worst kind of index bug: correct
/// until somebody adds an index.
///
/// **At most one member may be an array**, as in MongoDB. Two arrays would make
/// the key count their product, so a pair of modest lists could put thousands of
/// entries in the index for one document. That is refused at write time
/// ([`CompoundIndexMultipleArrays`](crate::TalaDbError)) rather than silently
/// admitted.
pub fn encode_compound_keys(
    values: &[&Value],
    id: Ulid,
) -> Result<Vec<Vec<u8>>, crate::TalaDbError> {
    let array_members = values
        .iter()
        .filter(|v| matches!(v, Value::Array(_)))
        .count();
    if array_members > 1 {
        return Err(crate::TalaDbError::CompoundIndexMultipleArrays);
    }
    if array_members == 0 {
        return Ok(encode_compound_key(values, id).into_iter().collect());
    }

    // Same quadratic-dedup fix as `encode_index_keys` — see the comment there.
    let mut keys: Vec<Vec<u8>> = Vec::new();
    let mut seen: HashSet<Vec<u8>> = HashSet::new();
    let position = values
        .iter()
        .position(|v| matches!(v, Value::Array(_)))
        .expect("array_members == 1");
    let Value::Array(items) = values[position] else {
        unreachable!("position found an array")
    };
    for item in items {
        // Nested arrays and objects have no encoding; skipping one element is
        // the same rule `encode_index_keys` follows.
        if matches!(item, Value::Array(_) | Value::Object(_)) {
            continue;
        }
        let mut expanded: Vec<&Value> = values.to_vec();
        expanded[position] = item;
        if let Some(key) = encode_compound_key(&expanded, id)
            && seen.insert(key.clone())
        {
            keys.push(key);
        }
    }
    Ok(keys)
}

/// Range bounds for an exact-equality scan on the given prefix values.
/// Appends [0x00; 16] and [0xFF; 16] ULID bounds so all documents with this
/// compound key prefix are included.
pub fn compound_range_eq(values: &[&Value]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut prefix = Vec::new();
    for v in values {
        encode_value_prefix(v, &mut prefix)?;
    }
    let mut start = prefix.clone();
    start.extend_from_slice(&[0x00u8; 16]);
    let mut end = prefix;
    end.extend_from_slice(&[0xFFu8; 16]);
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_sort_order() {
        let values = [
            Value::Int(-100),
            Value::Int(-1),
            Value::Int(0),
            Value::Int(1),
            Value::Int(100),
        ];
        let id = Ulid::nil();
        let keys: Vec<Vec<u8>> = values
            .iter()
            .map(|v| encode_index_key(v, id).unwrap())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "int index keys must sort in numeric order");
    }

    #[test]
    fn float_sort_order() {
        let values = [
            Value::Float(-1.0),
            Value::Float(0.0),
            Value::Float(0.5),
            Value::Float(1.0),
        ];
        let id = Ulid::nil();
        let keys: Vec<Vec<u8>> = values
            .iter()
            .map(|v| encode_index_key(v, id).unwrap())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "float index keys must sort in numeric order");
    }

    #[test]
    fn string_sort_order() {
        let values = [
            Value::Str("alpha".into()),
            Value::Str("beta".into()),
            Value::Str("gamma".into()),
        ];
        let id = Ulid::nil();
        let keys: Vec<Vec<u8>> = values
            .iter()
            .map(|v| encode_index_key(v, id).unwrap())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(
            keys, sorted,
            "string index keys must sort lexicographically"
        );
    }

    #[test]
    fn non_indexable_values_return_none() {
        let arr = Value::Array(vec![]);
        let obj = Value::Object(vec![]);
        assert!(encode_index_key(&arr, Ulid::nil()).is_none());
        assert!(encode_index_key(&obj, Ulid::nil()).is_none());
    }

    #[test]
    fn eq_range_does_not_match_longer_prefixes() {
        // "alpha" equality scan must NOT pull in "alphabet".
        let (start, end) = index_range_eq(&Value::Str("alpha".into())).unwrap();
        let longer_key =
            encode_index_key(&Value::Str("alphabet".into()), Ulid::from_bytes([0xAA; 16])).unwrap();
        assert!(
            longer_key < start || longer_key > end,
            "longer prefix 'alphabet' must fall outside the 'alpha' range"
        );
    }

    #[test]
    fn eq_range_matches_exact_value() {
        let id = Ulid::from_bytes([0x55; 16]);
        let (start, end) = index_range_eq(&Value::Str("alpha".into())).unwrap();
        let exact = encode_index_key(&Value::Str("alpha".into()), id).unwrap();
        assert!(
            exact >= start && exact <= end,
            "exact value must lie inside its own range"
        );
    }
}
