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
//! The escape and the terminator share the byte 0x00, and the 16-byte id that
//! follows a value may begin with 0xFF. So a value `v ++ "\0" ++ s` encodes as
//! `v`'s prefix ++ 0xFF ++ …, which sorts *among* the keys of `v` itself:
//!   • `v`'s equality range `prefix ++ 00…00 ..= prefix ++ FF…FF` also holds
//!     keys of every `v ++ "\0" ++ …` (with any id);
//!   • a key of `v` whose id starts with 0xFF can sort after keys of
//!     `v ++ "\0" ++ s` — `"a"` with id `FF 70 …` after `"a\0b"`.
//! The value part of a key is still unambiguous: it is everything but the last
//! 16 bytes, and those value parts sort in value order. Range bounds are built
//! to contain every match (see [`index_range_cmp`]); readers that count keys
//! or rely on key order must compare value parts, or rule the case out.
//! Generated ids start with 0xFF only after the year 10889; caller-supplied
//! ones can, and `derive_doc_id` hashes into all 16 bytes (about 1 in 256).
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
/// Type tag leading an encoded `Str` key.
pub(crate) const TAG_STR: u8 = 0x40;
/// Type tag leading an encoded `Bytes` key. Nothing indexable sorts above it.
pub(crate) const TAG_BYTES: u8 = 0x50;

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
            buf.push(TAG_STR);
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
            buf.push(TAG_BYTES);
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
///
/// The range holds every key whose value satisfies the bound, and may hold a
/// few that do not: callers post-filter (by document or by decoded key value).
/// For fixed-width types the range is exact. For Str and Bytes it is widened
/// where an escaped NUL makes key order disagree with value order (see the
/// module docs):
///
/// * `> v`: every `v ++ "\0" ++ …` sorts inside `v`'s own block, below
///   `prefix ++ FF…FF`. The range starts after `prefix ++ FF` instead, which
///   admits only the keys of `v` whose id starts with 0xFF.
/// * `< v` and `<= v`, for a `v` containing NUL: a shorter value `w` with
///   `v = w ++ "\0" ++ …` sorts after `v` when its id starts with 0xFF. The
///   range ends at `w`'s last possible key instead, which admits the values
///   between `v` and that key as well.
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
            } else if is_variable_length(&key) {
                key.push(0xFF);
                Bound::Excluded(key)
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
            let ceiling = nul_prefix_ceiling(&key);
            let bound = if inclusive {
                key.extend_from_slice(&[0xFFu8; 16]);
                Bound::Included(key)
            } else {
                key.extend_from_slice(&[0x00u8; 16]);
                Bound::Excluded(key)
            };
            match (ceiling, bound) {
                (Some(c), Bound::Included(k) | Bound::Excluded(k)) if c > k => Bound::Included(c),
                (_, bound) => bound,
            }
        }
    };

    Some((start, end))
}

/// Whether an encoded value prefix is a Str or Bytes one.
fn is_variable_length(prefix: &[u8]) -> bool {
    matches!(prefix.first(), Some(&(TAG_STR | TAG_BYTES)))
}

/// Whether an encoded value prefix is a Str or Bytes value containing NUL: the
/// only values whose keys can sort out of value order (see the module docs).
#[inline]
pub(crate) fn has_escaped_nul(prefix: &[u8]) -> bool {
    is_variable_length(prefix) && prefix.len() > 2 && prefix[1..prefix.len() - 1].contains(&0)
}

/// For a Str/Bytes prefix whose value contains NUL, the largest key of any
/// shorter value `w` with `value = w ++ "\0" ++ …`: `w`'s prefix ++ FF…FF.
/// Inside the escaped body every 0x00 starts an escape, so `w`'s prefix is
/// this prefix up to and including that 0x00.
fn nul_prefix_ceiling(prefix: &[u8]) -> Option<Vec<u8>> {
    if !is_variable_length(prefix) || prefix.len() < 2 {
        return None;
    }
    let body = &prefix[1..prefix.len() - 1];
    body.iter()
        .enumerate()
        .filter(|&(_, &b)| b == 0)
        .map(|(i, _)| {
            let mut key = prefix[..i + 2].to_vec();
            key.extend_from_slice(&[0xFF; 16]);
            key
        })
        .max()
}

/// Extract the ULID from an index key (last 16 bytes).
pub fn ulid_from_index_key(key: &[u8]) -> Option<Ulid> {
    if key.len() < 16 {
        return None;
    }
    let bytes: [u8; 16] = key[key.len() - 16..].try_into().ok()?;
    Some(Ulid::from_bytes(bytes))
}

/// Decode one value prefix, leaving subsequent compound fields unconsumed.
pub(crate) fn decode_value_prefix(bytes: &[u8]) -> Option<(Value, &[u8])> {
    let (&tag, rest) = bytes.split_first()?;
    match tag {
        0x00 => Some((Value::Null, rest)),
        0x10 | 0x11 => Some((Value::Bool(tag == 0x11), rest)),
        TAG_INT | TAG_FLOAT => {
            let word = u64::from_be_bytes(rest.get(..8)?.try_into().ok()?);
            let value = if tag == TAG_INT {
                Value::Int((word ^ 0x8000_0000_0000_0000) as i64)
            } else {
                let bits = if word >> 63 == 1 {
                    word ^ 0x8000_0000_0000_0000
                } else {
                    !word
                };
                Value::Float(f64::from_bits(bits))
            };
            Some((value, &rest[8..]))
        }
        0x40 | 0x50 => {
            let mut value = Vec::new();
            let mut rest = rest;
            loop {
                let (&byte, tail) = rest.split_first()?;
                rest = tail;
                if byte == 0 {
                    if rest.first() == Some(&0xff) {
                        value.push(0);
                        rest = &rest[1..];
                    } else {
                        break;
                    }
                } else {
                    value.push(byte);
                }
            }
            Some((
                if tag == 0x40 {
                    Value::Str(String::from_utf8(value).ok()?)
                } else {
                    Value::Bytes(value)
                },
                rest,
            ))
        }
        _ => None,
    }
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

// Optional, transactionally maintained array-document counts. Missing or
// malformed records mean unknown, never scalar-only. Existing index definitions
// and keys keep their encoding; rebuilding an old index enables this fast path.
pub(crate) const META_INDEX_ARRAYS_TABLE: &str = "meta::index_arrays";

pub(crate) fn index_array_count(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

pub(crate) fn adjust_index_arrays(
    txn: &mut dyn crate::engine::WriteTxn,
    collection: &str,
    field: &str,
    removed: u64,
    added: u64,
) -> Result<(), crate::error::TalaDbError> {
    if removed == added {
        return Ok(());
    }
    let key = meta_key(collection, field);
    let Some(bytes) = txn.get(META_INDEX_ARRAYS_TABLE, key.as_bytes())? else {
        return Ok(()); // legacy index: do not invent a partial count
    };
    let count = index_array_count(&bytes)
        .and_then(|n| n.checked_sub(removed))
        .and_then(|n| n.checked_add(added));
    if let Some(count) = count {
        txn.put(
            META_INDEX_ARRAYS_TABLE,
            key.as_bytes(),
            &count.to_le_bytes(),
        )?;
    } else {
        // Unknown metadata disables narrowing without preventing the write.
        txn.delete(META_INDEX_ARRAYS_TABLE, key.as_bytes())?;
    }
    Ok(())
}
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

    // -- Str/Bytes keys next to ids starting 0xFF -------------------------

    /// Str and Bytes values over the bytes that matter — NUL (escaped as
    /// `00 FF`), 0xFF (Bytes only) and two ordinary ones — at every length up to
    /// `max_len`, plus 0xFF runs longer than a bound's 16 bytes of id padding.
    fn varlen_pool(max_len: usize) -> Vec<Value> {
        let mut words: Vec<Vec<u8>> = vec![vec![]];
        let mut level = vec![vec![]];
        for _ in 0..max_len {
            level = level
                .iter()
                .flat_map(|w: &Vec<u8>| {
                    [0x00u8, 0x01, b'a', 0xFF].map(|b| {
                        let mut w = w.clone();
                        w.push(b);
                        w
                    })
                })
                .collect();
            words.extend(level.iter().cloned());
        }
        let mut long = vec![0x00];
        long.extend([0xFF; 17]);
        words.push(long);
        let mut long = vec![b'a', 0x00];
        long.extend([0xFF; 16]);
        words.push(long.clone());
        long.pop();
        long.push(b'b');
        words.push(long);
        words.push(vec![0xFF; 20]);
        let mut out = Vec::new();
        for w in words {
            if let Ok(s) = String::from_utf8(w.clone()) {
                out.push(Value::Str(s));
            }
            out.push(Value::Bytes(w));
        }
        out
    }

    /// Edge ids — minimum, maximum, every interesting 0xFF lead — and seeded
    /// random ones, half of them forced to start with 0xFF.
    fn adversarial_ids(random: usize) -> Vec<Ulid> {
        let mut ids: Vec<[u8; 16]> = vec![[0x00; 16], [0xFF; 16], [0x01; 16], [0x7F; 16]];
        for lead in [
            &[0xFF, 0x00][..],
            &[0xFF, 0x01],
            &[0xFF, 0x61],
            &[0xFF, 0x62, 0x00],
            &[0xFF, 0xFF, 0x00],
            &[0xFF, 0x00, 0xFF],
        ] {
            let mut id = [0u8; 16];
            id[..lead.len()].copy_from_slice(lead);
            ids.push(id);
        }
        let mut state = 0xD1B5_4A32_D192_ED03u64;
        for i in 0..random {
            let mut id = [0u8; 16];
            for chunk in id.chunks_mut(8) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                chunk.copy_from_slice(&state.to_be_bytes());
            }
            if i % 2 == 0 {
                id[0] = 0xFF;
            }
            ids.push(id);
        }
        ids.into_iter().map(Ulid::from_bytes).collect()
    }

    fn raw(v: &Value) -> (u8, &[u8]) {
        match v {
            Value::Str(s) => (0, s.as_bytes()),
            Value::Bytes(b) => (1, b),
            _ => unreachable!("variable-length values only"),
        }
    }

    fn within(key: &[u8], (start, end): &(Bound<Vec<u8>>, Bound<Vec<u8>>)) -> bool {
        let above = match start {
            Bound::Included(s) => key >= s.as_slice(),
            Bound::Excluded(s) => key > s.as_slice(),
            Bound::Unbounded => true,
        };
        let below = match end {
            Bound::Included(e) => key <= e.as_slice(),
            Bound::Excluded(e) => key < e.as_slice(),
            Bound::Unbounded => true,
        };
        above && below
    }

    /// Every key of every value satisfying a bound lies inside its range, for
    /// all four comparisons, whatever the id.
    #[test]
    fn range_bounds_hold_every_matching_key() {
        let pool = varlen_pool(4);
        let ids = adversarial_ids(6);
        let keys: Vec<(&Value, Vec<u8>)> = pool
            .iter()
            .flat_map(|w| {
                ids.iter()
                    .map(move |id| (w, encode_index_key(w, *id).unwrap()))
            })
            .collect();
        for v in &pool {
            let (tv, rv) = raw(v);
            for (op, bounds) in [
                ("gt", index_range_cmp(Some((v, false)), None)),
                ("gte", index_range_cmp(Some((v, true)), None)),
                ("lt", index_range_cmp(None, Some((v, false)))),
                ("lte", index_range_cmp(None, Some((v, true)))),
            ] {
                let bounds = bounds.unwrap();
                for (w, key) in &keys {
                    let (tw, rw) = raw(w);
                    let ord = rw.cmp(rv);
                    let matches = match op {
                        "gt" => ord.is_gt(),
                        "gte" => ord.is_ge(),
                        "lt" => ord.is_lt(),
                        _ => ord.is_le(),
                    };
                    if tw == tv && matches {
                        assert!(
                            within(key, &bounds),
                            "op {op} on {v:?} misses {w:?} key {key:02x?}"
                        );
                    }
                }
            }
        }
    }

    /// The widening admits only the keys it has to. Besides matches, a range
    /// may hold: for `> v`, keys of `v` whose id starts with 0xFF; for any
    /// bound, keys of a shorter `w` with `v = w ++ "\0" ++ …` whose id starts
    /// with 0xFF, and for an upper bound everything in the block of `v`'s
    /// shortest such `w` (`w ++ "\0" ++ …`); and for `<= v`, every
    /// `v ++ "\0" ++ …`, as before.
    #[test]
    fn range_bounds_admit_only_the_documented_extras() {
        let pool = varlen_pool(4);
        let ids = adversarial_ids(6);
        let nul_prefixes = |v: &[u8]| -> Vec<Vec<u8>> {
            (0..v.len())
                .filter(|&i| v[i] == 0)
                .map(|i| v[..i].to_vec())
                .collect()
        };
        for v in &pool {
            let (tv, rv) = raw(v);
            let cuts = nul_prefixes(rv);
            let shortest = cuts.first().map(|w| {
                let mut w = w.clone();
                w.push(0);
                w
            });
            let mut extension = rv.to_vec();
            extension.push(0);
            for (op, bounds) in [
                ("gt", index_range_cmp(Some((v, false)), None)),
                ("gte", index_range_cmp(Some((v, true)), None)),
                ("lt", index_range_cmp(None, Some((v, false)))),
                ("lte", index_range_cmp(None, Some((v, true)))),
            ] {
                let bounds = bounds.unwrap();
                for w in &pool {
                    let (tw, rw) = raw(w);
                    if tw != tv {
                        continue;
                    }
                    let ord = rw.cmp(rv);
                    let matches = match op {
                        "gt" => ord.is_gt(),
                        "gte" => ord.is_ge(),
                        "lt" => ord.is_lt(),
                        _ => ord.is_le(),
                    };
                    for id in &ids {
                        let key = encode_index_key(w, *id).unwrap();
                        if matches || !within(&key, &bounds) {
                            continue;
                        }
                        let ff = id.to_bytes()[0] == 0xFF;
                        let upper = op.starts_with('l');
                        let allowed = (ff && cuts.iter().any(|c| c.as_slice() == rw))
                            || (op == "gt" && ff && rw == rv)
                            || (upper && shortest.as_ref().is_some_and(|p| rw.starts_with(p)))
                            || (op == "lte" && rw.starts_with(&extension));
                        assert!(allowed, "{op} {v:?} admits {w:?} with id {id}");
                    }
                }
            }
        }
    }

    /// An equality range holds every key of the value, and its keys of the
    /// range's own length are exactly those (what `count` relies on).
    #[test]
    fn equality_range_keys_of_its_length_are_exactly_the_value() {
        let pool = varlen_pool(4);
        let ids = adversarial_ids(6);
        for v in &pool {
            let (start, end) = index_range_eq(v).unwrap();
            for w in &pool {
                for id in &ids {
                    let key = encode_index_key(w, *id).unwrap();
                    let inside = key >= start && key <= end;
                    if w == v {
                        assert!(inside, "{v:?} range misses its own key {key:02x?}");
                    }
                    assert_eq!(inside && key.len() == start.len(), w == v, "{v:?} vs {w:?}");
                }
            }
        }
    }

    /// Value parts (keys without the id) sort in value order. Whole keys sort
    /// in `(value, id)` order except for one shape, which the sorted-page walk
    /// guards against: the key first in key order holds a value containing NUL
    /// and the other key's id starts with 0xFF.
    #[test]
    fn keys_disagree_with_value_order_only_after_an_escaped_nul_and_an_ff_id() {
        let pool = varlen_pool(3);
        let ids = adversarial_ids(4);
        let keys: Vec<(&Value, Ulid, Vec<u8>)> = pool
            .iter()
            .flat_map(|w| {
                ids.iter()
                    .map(move |id| (w, *id, encode_index_key(w, *id).unwrap()))
            })
            .collect();
        let mut disagreements = 0;
        for (a, ida, ka) in &keys {
            for (b, idb, kb) in &keys {
                if raw(a).0 != raw(b).0 {
                    continue;
                }
                let value_order = raw(a).1.cmp(raw(b).1);
                let (va, vb) = (&ka[..ka.len() - 16], &kb[..kb.len() - 16]);
                assert_eq!(va.cmp(vb), value_order, "value parts of {a:?} and {b:?}");
                if ka < kb && value_order.then(ida.cmp(idb)).is_gt() {
                    disagreements += 1;
                    assert!(has_escaped_nul(va), "{a:?} before {b:?}");
                    assert_eq!(idb.to_bytes()[0], 0xFF, "{a:?} before {b:?}");
                }
            }
        }
        assert!(disagreements > 0, "the pool must contain the reordering");
    }

    #[test]
    fn escaped_nul_is_detected_only_in_variable_length_values() {
        let prefix = |v: &Value| {
            let mut p = Vec::new();
            encode_value_prefix(v, &mut p).unwrap();
            p
        };
        assert!(has_escaped_nul(&prefix(&Value::Str("a\0b".into()))));
        assert!(has_escaped_nul(&prefix(&Value::Bytes(vec![0]))));
        assert!(!has_escaped_nul(&prefix(&Value::Str("ab".into()))));
        assert!(!has_escaped_nul(&prefix(&Value::Str(String::new()))));
        assert!(!has_escaped_nul(&prefix(&Value::Bytes(vec![0xFF, 1]))));
        // Fixed-width encodings hold zero bytes as data.
        assert!(!has_escaped_nul(&prefix(&Value::Int(0))));
        assert!(!has_escaped_nul(&prefix(&Value::Float(1.0))));
    }
}
