use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use ulid::Ulid;
use web_time::{SystemTime, UNIX_EPOCH};

/// Packs `(prev_ms_48 << 16) | seq_16` into a single u64 so that the common
/// path is one lock-free CAS.  48 bits for the millisecond timestamp are
/// sufficient until year 10895; 16 bits allow 65 536 distinct IDs per ms
/// per process (any more would exceed even a very hot write loop).
static ULID_STATE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn new_ulid_pub() -> Ulid {
    new_ulid()
}

fn new_ulid() -> Ulid {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // Monotonic per-process counter within the same millisecond keeps bulk
    // inserts from any thread sortable by ULID.
    let seq = loop {
        let cur = ULID_STATE.load(Ordering::Relaxed);
        let cur_ms = cur >> 16;
        let cur_seq = (cur & 0xFFFF) as u16;
        let (next_ms, next_seq) = if ms == cur_ms {
            (cur_ms, cur_seq.wrapping_add(1))
        } else {
            (ms, 0u16)
        };
        let packed = (next_ms << 16) | u64::from(next_seq);
        if ULID_STATE
            .compare_exchange_weak(cur, packed, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            break next_seq;
        }
    };

    // 80-bit random payload. Entropy failure must not be silently ignored: a
    // zeroed random field makes IDs predictable and collision-prone across
    // processes.
    let mut buf = [0u8; 10];
    getrandom::fill(&mut buf)
        .expect("taladb: system entropy source failed while generating a ULID");
    // Upper 16 bits of the random field = monotonic sequence; lower 64 bits = random.
    let rand_lo = u64::from_le_bytes(buf[..8].try_into().unwrap());
    let random = (u128::from(seq) << 64) | u128::from(rand_lo);
    Ulid::from_parts(ms, random)
}

/// A dynamically-typed value that maps to JSON conceptually but serializes via postcard.
/// Uses Vec<(String, Value)> for objects (not HashMap) to guarantee deterministic serialization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Str(_) => "string",
            Self::Bytes(_) => "bytes",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(b) = self {
            Some(*b)
        } else {
            None
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        if let Self::Int(n) = self {
            Some(*n)
        } else {
            None
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        if let Self::Float(f) = self {
            Some(*f)
        } else {
            None
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        if let Self::Str(s) = self {
            Some(s.as_str())
        } else {
            None
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        if let Self::Bytes(b) = self {
            Some(b.as_slice())
        } else {
            None
        }
    }

    pub fn as_array(&self) -> Option<&[Self]> {
        if let Self::Array(a) = self {
            Some(a.as_slice())
        } else {
            None
        }
    }

    /// The fields of an object value, in insertion order.
    pub fn as_object(&self) -> Option<&[(String, Self)]> {
        if let Self::Object(o) = self {
            Some(o.as_slice())
        } else {
            None
        }
    }

    /// Look up a key in an object value.
    ///
    /// Returns `None` if this is not an object, or has no such key. Objects are
    /// a `Vec` of pairs rather than a map (to keep serialization
    /// deterministic), so this is a linear scan — fine for the document-shaped
    /// objects it exists for, not for large ones.
    pub fn get(&self, key: &str) -> Option<&Self> {
        self.as_object()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// True for [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Compare two Values for ordering (used by query engine range ops).
    /// Returns None if the values are not comparable (different types).
    pub fn partial_cmp_numeric(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a.partial_cmp(b),
            (Self::Float(a), Self::Float(b)) => a.partial_cmp(b),
            (Self::Int(a), Self::Float(b)) => cmp_i64_f64(*a, *b),
            (Self::Float(a), Self::Int(b)) => cmp_i64_f64(*b, *a).map(std::cmp::Ordering::reverse),
            (Self::Str(a), Self::Str(b)) => a.partial_cmp(b),
            (Self::Bool(a), Self::Bool(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

/// Build a `Value` from the Rust type that maps onto it.
///
/// Constructing a document meant naming the variant for every field —
/// `("year".into(), Value::Int(1887))` — which is noise at every call site and
/// gets worse the more fields a document has. With these, `1887.into()` works,
/// and so does collecting an iterator of anything convertible.
macro_rules! value_from {
    ($($ty:ty => $variant:ident $(as $cast:ty)?),* $(,)?) => {
        $(
            impl From<$ty> for Value {
                fn from(v: $ty) -> Self {
                    Value::$variant(v $(as $cast)?)
                }
            }
        )*
    };
}

value_from! {
    bool => Bool,
    i8 => Int as i64,
    i16 => Int as i64,
    i32 => Int as i64,
    i64 => Int,
    u8 => Int as i64,
    u16 => Int as i64,
    u32 => Int as i64,
    f32 => Float as f64,
    f64 => Float,
    String => Str,
    Vec<u8> => Bytes,
    Vec<Value> => Array,
    Vec<(String, Value)> => Object,
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::Str(v.to_owned())
    }
}

impl From<&[u8]> for Value {
    fn from(v: &[u8]) -> Self {
        Self::Bytes(v.to_vec())
    }
}

/// `None` becomes [`Value::Null`] — the document model has no notion of an
/// absent-but-present field, so an optional field is either null or omitted.
impl<T: Into<Self>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Self::Null, Into::into)
    }
}

impl<T: Into<Self>> FromIterator<T> for Value {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::Array(iter.into_iter().map(Into::into).collect())
    }
}

/// Human-readable rendering, for logs, error messages, and `{}` in tests.
///
/// This is **not** a serialization format — it is lossy on purpose (`Bytes`
/// renders as a length, not its contents) and must not be parsed. Use serde for
/// round-tripping.
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => f.write_str("null"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(n) => write!(f, "{n}"),
            Self::Float(x) => write!(f, "{x}"),
            Self::Str(s) => f.write_str(s),
            Self::Bytes(b) => write!(f, "<{} bytes>", b.len()),
            Self::Array(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            Self::Object(fields) => {
                f.write_str("{")?;
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{key}: {value}")?;
                }
                f.write_str("}")
            }
        }
    }
}

/// Order an `i64` against an `f64` exactly, without routing either through the
/// other's type.
///
/// `i64 as f64` silently rounds above 2^53, which made `$gt`/`$lt` return the
/// wrong answer for large integers: `Int(9007199254740993)` rounds to
/// `9007199254740992.0` and compared `Equal` to that float instead of
/// `Greater`. `f64 as i64` is worse — it saturates, so every float past
/// `i64::MAX` collapsed onto the same integer.
///
/// Instead: reject NaN, handle the out-of-i64-range floats by sign, then split
/// the remaining float into its integer and fractional parts and compare in the
/// integer domain. Every step is exact.
fn cmp_i64_f64(a: i64, b: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;

    if b.is_nan() {
        return None;
    }
    // 2^63 — the first f64 above `i64::MAX`. Both bounds are exactly
    // representable, so these comparisons are themselves exact.
    const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;
    if b >= TWO_POW_63 {
        return Some(Ordering::Less);
    }
    if b < -TWO_POW_63 {
        return Some(Ordering::Greater);
    }

    // `b` now sits in [i64::MIN, i64::MAX], so truncation is lossless.
    let truncated = b.trunc();
    let whole = truncated as i64;
    match a.cmp(&whole) {
        // Equal integer parts — the fraction breaks the tie. `trunc` rounds
        // toward zero, so a positive fraction makes `b` larger and a negative
        // one makes it smaller.
        Ordering::Equal => Some(if b > truncated {
            Ordering::Less
        } else if b < truncated {
            Ordering::Greater
        } else {
            Ordering::Equal
        }),
        ord => Some(ord),
    }
}

/// FNV-1a (128-bit) offset basis and prime, per the reference specification.
///
/// FNV is chosen over a stronger hash on purpose: [`derive_doc_id`] must be
/// reimplemented **byte-identically** in TypeScript (see `deriveDocId` in
/// `@taladb/*`), and FNV-1a is short enough to port without ambiguity. It is a
/// non-cryptographic hash — that is fine here, because the input is a remote
/// primary key from an origin the client already trusts, not adversarial input.
const FNV1A128_OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const FNV1A128_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// Derive a stable, deterministic [`Ulid`] from a remote primary key.
///
/// The engine assigns ULIDs and **ignores caller-supplied `_id`s**, so a
/// document replicated from a remote origin has no stable local identity to
/// merge on: re-fetching the same row would insert a duplicate. Hashing the
/// remote key into the ULID gives that identity back — the same `(collection,
/// key)` always maps to the same document, which is what makes replication
/// upserts idempotent, resumable, and safe to run concurrently from the
/// bootstrap walk and an on-demand fetch.
///
/// `collection` is part of the preimage so the same remote id in two different
/// collections cannot collide.
///
/// # Ordering caveat
///
/// The result is a hash, so its ULID timestamp prefix is **not chronological**.
/// Documents written with a derived id do not come back in insertion order from
/// an unordered `find()`. Reads over replicated collections must carry an
/// explicit sort. (Documents written via `insert`/`insert_many` are unaffected —
/// they still get monotonic ULIDs.)
pub fn derive_doc_id(collection: &str, key: &str) -> Ulid {
    // A 0x00 separator keeps the preimage unambiguous: without it,
    // ("ab", "c") and ("a", "bc") would hash identically.
    const SEPARATOR: [u8; 1] = [0u8];
    let mut hash = FNV1A128_OFFSET_BASIS;
    for &byte in collection
        .as_bytes()
        .iter()
        .chain(SEPARATOR.iter())
        .chain(key.as_bytes().iter())
    {
        hash ^= u128::from(byte);
        hash = hash.wrapping_mul(FNV1A128_PRIME);
    }
    Ulid::from_bytes(hash.to_be_bytes())
}

/// A database document with a ULID primary key and ordered fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: Ulid,
    pub fields: Vec<(String, Value)>,
}

impl Document {
    pub fn new(fields: Vec<(String, Value)>) -> Self {
        Self {
            id: new_ulid(),
            fields: dedupe_fields(fields),
        }
    }

    pub fn with_id(id: Ulid, fields: Vec<(String, Value)>) -> Self {
        Self {
            id,
            fields: dedupe_fields(fields),
        }
    }

    /// Return the value at `key`, supporting dot-notation for nested objects.
    ///
    /// `"name"` returns the top-level `name` field.
    /// `"address.city"` traverses into a nested `Value::Object`.
    /// Deep paths like `"a.b.c"` are resolved recursively.
    pub fn get(&self, key: &str) -> Option<&Value> {
        if let Some(dot) = key.find('.') {
            let (head, tail) = (&key[..dot], &key[dot + 1..]);
            let parent = self
                .fields
                .iter()
                .find(|(k, _)| k == head)
                .map(|(_, v)| v)?;
            value_get_nested(parent, tail)
        } else {
            self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
        }
    }

    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        let key = key.into();
        if let Some(entry) = self.fields.iter_mut().find(|(k, _)| k == &key) {
            entry.1 = value;
        } else {
            self.fields.push((key, value));
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        if let Some(pos) = self.fields.iter().position(|(k, _)| k == key) {
            Some(self.fields.remove(pos).1)
        } else {
            None
        }
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

/// Drop duplicate field names, keeping the first occurrence.
///
/// `get`/`set` only ever see the first entry for a name, but duplicates would
/// all be serialized, silently bloating storage and resurfacing after a
/// deserialize → mutate → serialize cycle. Normalizing at construction keeps
/// the in-memory and on-disk views consistent.
fn dedupe_fields(fields: Vec<(String, Value)>) -> Vec<(String, Value)> {
    // Fast path: small field lists with unique names allocate nothing extra.
    let has_dup = fields
        .iter()
        .enumerate()
        .any(|(i, (k, _))| fields[..i].iter().any(|(k2, _)| k2 == k));
    if !has_dup {
        return fields;
    }
    let mut out: Vec<(String, Value)> = Vec::with_capacity(fields.len());
    for (k, v) in fields {
        if !out.iter().any(|(k2, _)| k2 == &k) {
            out.push((k, v));
        }
    }
    out
}

/// Traverse a `Value::Object` using a dot-separated path.
///
/// Returns `None` when nesting exceeds 64 levels to prevent stack overflow
/// from adversarially crafted field paths.
pub fn value_get_nested<'a>(val: &'a Value, path: &str) -> Option<&'a Value> {
    value_get_nested_depth(val, path, 0)
}

fn value_get_nested_depth<'a>(val: &'a Value, path: &str, depth: u8) -> Option<&'a Value> {
    if depth >= 64 {
        return None;
    }
    if let Some(dot) = path.find('.') {
        let (head, tail) = (&path[..dot], &path[dot + 1..]);
        match val {
            Value::Object(fields) => {
                let child = fields.iter().find(|(k, _)| k == head).map(|(_, v)| v)?;
                value_get_nested_depth(child, tail, depth + 1)
            }
            _ => None,
        }
    } else {
        match val {
            Value::Object(fields) => fields.iter().find(|(k, _)| k == path).map(|(_, v)| v),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    /// Integers past 2^53 have no exact `f64`, so the old `a as f64` cast made
    /// range queries answer `Equal` where the true answer is `Greater`/`Less`.
    #[test]
    fn int_float_comparison_is_exact_beyond_2_pow_53() {
        // 2^53 + 1 — the first integer f64 cannot represent. It rounds down to
        // 2^53, which is the float we compare it against.
        let big = Value::Int(9_007_199_254_740_993);
        let as_float = Value::Float(9_007_199_254_740_992.0);
        assert_eq!(big.partial_cmp_numeric(&as_float), Some(Ordering::Greater));
        assert_eq!(as_float.partial_cmp_numeric(&big), Some(Ordering::Less));

        // Same story at the bottom of the range.
        let small = Value::Int(-9_007_199_254_740_993);
        let small_float = Value::Float(-9_007_199_254_740_992.0);
        assert_eq!(
            small.partial_cmp_numeric(&small_float),
            Some(Ordering::Less)
        );
        assert_eq!(
            small_float.partial_cmp_numeric(&small),
            Some(Ordering::Greater)
        );
    }

    /// `f64 as i64` saturates, so floats beyond the integer range must be
    /// handled by sign rather than by casting.
    #[test]
    fn int_float_comparison_handles_out_of_range_floats() {
        let max = Value::Int(i64::MAX);
        let min = Value::Int(i64::MIN);

        assert_eq!(
            max.partial_cmp_numeric(&Value::Float(1e300)),
            Some(Ordering::Less)
        );
        assert_eq!(
            min.partial_cmp_numeric(&Value::Float(-1e300)),
            Some(Ordering::Greater)
        );
        assert_eq!(
            max.partial_cmp_numeric(&Value::Float(f64::INFINITY)),
            Some(Ordering::Less)
        );
        assert_eq!(
            min.partial_cmp_numeric(&Value::Float(f64::NEG_INFINITY)),
            Some(Ordering::Greater)
        );
    }

    #[test]
    fn int_float_comparison_handles_fractions_and_nan() {
        // Fractions break ties on equal integer parts, in both sign directions.
        assert_eq!(
            Value::Int(3).partial_cmp_numeric(&Value::Float(3.5)),
            Some(Ordering::Less)
        );
        assert_eq!(
            Value::Int(-3).partial_cmp_numeric(&Value::Float(-3.5)),
            Some(Ordering::Greater)
        );
        assert_eq!(
            Value::Int(3).partial_cmp_numeric(&Value::Float(3.0)),
            Some(Ordering::Equal)
        );

        // NaN stays incomparable, as it was before.
        assert_eq!(
            Value::Int(1).partial_cmp_numeric(&Value::Float(f64::NAN)),
            None
        );
        assert_eq!(
            Value::Float(f64::NAN).partial_cmp_numeric(&Value::Int(1)),
            None
        );
    }

    #[test]
    fn value_from_impls_cover_the_rust_types_that_map_onto_it() {
        assert_eq!(Value::from(true), Value::Bool(true));
        assert_eq!(Value::from(42i32), Value::Int(42));
        assert_eq!(Value::from(42u8), Value::Int(42));
        assert_eq!(Value::from(1.5f32), Value::Float(1.5));
        assert_eq!(Value::from("hi"), Value::Str("hi".into()));
        assert_eq!(Value::from(String::from("hi")), Value::Str("hi".into()));
        assert_eq!(Value::from(vec![1u8, 2]), Value::Bytes(vec![1, 2]));

        // Optionals collapse to null rather than being dropped.
        assert_eq!(Value::from(None::<i64>), Value::Null);
        assert_eq!(Value::from(Some(7i64)), Value::Int(7));

        // Collecting builds an array without naming the variant.
        let arr: Value = [1i64, 2, 3].into_iter().collect();
        assert_eq!(
            arr,
            Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
        );
    }

    #[test]
    fn value_accessors_cover_every_variant() {
        assert_eq!(Value::Bool(true).as_bool(), Some(true));
        assert_eq!(Value::Int(1).as_int(), Some(1));
        assert_eq!(Value::Float(1.5).as_float(), Some(1.5));
        assert_eq!(Value::Str("s".into()).as_str(), Some("s"));
        assert_eq!(Value::Bytes(vec![1, 2]).as_bytes(), Some(&[1u8, 2][..]));
        assert_eq!(
            Value::Array(vec![Value::Int(1)]).as_array().unwrap().len(),
            1
        );
        assert!(Value::Null.is_null());

        let obj = Value::Object(vec![("a".into(), Value::Int(1))]);
        assert_eq!(obj.as_object().unwrap().len(), 1);
        assert_eq!(obj.get("a"), Some(&Value::Int(1)));
        assert_eq!(obj.get("missing"), None);

        // Accessors are type-checked, not coercing.
        assert_eq!(Value::Int(1).as_str(), None);
        assert_eq!(Value::Str("1".into()).as_int(), None);
        assert_eq!(Value::Int(1).get("a"), None);
    }

    #[test]
    fn value_display_is_readable_and_hides_byte_contents() {
        assert_eq!(Value::Null.to_string(), "null");
        assert_eq!(Value::Int(1887).to_string(), "1887");
        assert_eq!(Value::Str("Rizal".into()).to_string(), "Rizal");
        // Bytes render as a length: dumping a blob into a log line is never
        // what the caller wanted.
        assert_eq!(Value::Bytes(vec![0; 500]).to_string(), "<500 bytes>");
        assert_eq!(
            Value::Array(vec![Value::Int(1), Value::Str("a".into())]).to_string(),
            "[1, a]"
        );
        assert_eq!(
            Value::Object(vec![
                ("a".into(), Value::Int(1)),
                ("b".into(), Value::Bool(false))
            ])
            .to_string(),
            "{a: 1, b: false}"
        );
    }

    #[test]
    fn round_trip_postcard() {
        let doc = Document::new(vec![
            ("name".into(), Value::Str("Alice".into())),
            ("age".into(), Value::Int(30)),
            ("active".into(), Value::Bool(true)),
            ("score".into(), Value::Float(9.5)),
            (
                "tags".into(),
                Value::Array(vec![Value::Str("rust".into()), Value::Str("db".into())]),
            ),
        ]);

        let bytes = postcard::to_allocvec(&doc).unwrap();
        let decoded: Document = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(doc.id, decoded.id);
        assert_eq!(doc.fields, decoded.fields);
    }

    #[test]
    fn document_get_set_remove() {
        let mut doc = Document::new(vec![("x".into(), Value::Int(1))]);
        assert_eq!(doc.get("x"), Some(&Value::Int(1)));
        doc.set("x", Value::Int(2));
        assert_eq!(doc.get("x"), Some(&Value::Int(2)));
        doc.set("y", Value::Bool(true));
        assert_eq!(doc.get("y"), Some(&Value::Bool(true)));
        doc.remove("x");
        assert_eq!(doc.get("x"), None);
    }

    #[test]
    fn derive_doc_id_is_deterministic() {
        assert_eq!(
            derive_doc_id("products", "sku-123"),
            derive_doc_id("products", "sku-123")
        );
    }

    #[test]
    fn derive_doc_id_separates_collections() {
        // The same remote key in two collections must not collide.
        assert_ne!(derive_doc_id("products", "1"), derive_doc_id("orders", "1"));
    }

    #[test]
    fn derive_doc_id_separator_prevents_boundary_collision() {
        // Without the 0x00 separator these two preimages would be identical.
        assert_ne!(derive_doc_id("ab", "c"), derive_doc_id("a", "bc"));
    }

    /// Cross-language contract. These vectors are duplicated verbatim in the
    /// TypeScript `deriveDocId` test suite and **must** stay in lockstep: if the
    /// two implementations ever disagree, two clients assign different `_id`s to
    /// the same remote row and the replica silently forks into duplicates.
    #[test]
    fn derive_doc_id_cross_language_vectors() {
        let vectors: &[(&str, &str, &str)] = &[
            ("products", "sku-123", "56GC678DQYWW1Z98HPYJ90WVKH"),
            ("products", "1", "7Z6Y6H8NG96ZGN4PJVDP18CSY2"),
            ("orders", "1", "5VYD63GDV5KCDXV2A0SYRWSKVZ"),
            ("", "", "6J535PJ40THJQQH49BE174M53Z"),
            // Multi-byte UTF-8 in the key: the hash runs over raw bytes, so the
            // TypeScript port must feed it UTF-8 bytes, not UTF-16 code units.
            ("products", "sku-ñ-💡", "5NZ0PGNM2CF0BTHN0AAX8CWPAA"),
        ];
        for (collection, key, expected) in vectors {
            assert_eq!(
                derive_doc_id(collection, key).to_string(),
                *expected,
                "derive_doc_id({collection:?}, {key:?}) drifted — \
                 the TypeScript implementation must match these exactly"
            );
        }
    }
}
