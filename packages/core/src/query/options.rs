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
/// use taladb::query::options::{FindOptions, SortSpec};
///
/// let opts = FindOptions {
///     sort: vec![SortSpec::desc("createdAt"), SortSpec::asc("name")],
///     skip: 20,
///     limit: Some(10),
///     fields: Some(vec!["name".into(), "email".into()]),
///     exclude: None,
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
    /// If `Some`, these fields are stripped from each document — the
    /// complement of `fields`, for when a document has one large field the
    /// caller does not need (an embedding). `_id` is never removed. Applied
    /// after `fields` if both are set.
    pub exclude: Option<Vec<String>>,
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
///   Null < Bool < Int/Float (numeric) < Str < Bytes < Array/Object
///
/// This must be a total order — reflexive, antisymmetric and transitive — or
/// the standard library's sort may panic ("user-provided comparison function
/// does not correctly implement a total order") or return an arbitrary order.
/// Numbers therefore compare by their exact value, Int against Float included:
///
/// * an `i64` is never rounded through `f64`: `2^53 + 1` sorts after the float
///   `2^53` instead of equal to it (`document::cmp_i64_f64`);
/// * `-0.0`, `0.0` and `Int(0)` are equal, so they tie on `_id` as before;
/// * every NaN, of either sign and any payload, sorts after `+inf` and equal
///   to every other NaN. The `$gt`/`$lt` filters never match NaN; a sort has to
///   put it somewhere, and after every number is where the index puts the NaN
///   that `f64::NAN` and JavaScript produce.
///
/// Arrays and objects all compare equal to one another.
pub(crate) fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,

        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Bool(_), _) => Ordering::Less,
        (_, Value::Bool(_)) => Ordering::Greater,

        // Numeric: Int and Float compare by exact value across types
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => cmp_floats(*x, *y),
        (Value::Int(x), Value::Float(y)) => cmp_int_float(*x, *y),
        (Value::Float(x), Value::Int(y)) => cmp_int_float(*y, *x).reverse(),
        (Value::Int(_) | Value::Float(_), _) => Ordering::Less,
        (_, Value::Int(_) | Value::Float(_)) => Ordering::Greater,

        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Str(_), _) => Ordering::Less,
        (_, Value::Str(_)) => Ordering::Greater,

        (Value::Bytes(x), Value::Bytes(y)) => x.cmp(y),
        (Value::Bytes(_), _) => Ordering::Less,
        (_, Value::Bytes(_)) => Ordering::Greater,

        // Arrays and objects: one class of equal values, ordered by `_id`.
        _ => Ordering::Equal,
    }
}

/// Floats by value, with every NaN equal to every other and above `+inf`.
/// `partial_cmp` decides any pair without a NaN, so `-0.0 == 0.0`.
fn cmp_floats(x: f64, y: f64) -> std::cmp::Ordering {
    x.partial_cmp(&y)
        .unwrap_or_else(|| x.is_nan().cmp(&y.is_nan()))
}

/// An Int against a Float, exactly; a NaN float is above every Int.
fn cmp_int_float(x: i64, y: f64) -> std::cmp::Ordering {
    crate::document::cmp_i64_f64(x, y).unwrap_or(std::cmp::Ordering::Less)
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

/// Remove the listed fields from a document. `_id` is not a field here — it
/// lives in `doc.id` — so it can never be excluded.
pub fn exclude_fields(mut doc: Document, exclude: &[String]) -> Document {
    doc.fields.retain(|(k, _)| !exclude.iter().any(|f| f == k));
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::{self, Equal, Greater, Less};

    const TWO_53: i64 = 1 << 53;
    const TWO_63: f64 = 9_223_372_036_854_775_808.0;

    /// Every value the comparator has an edge for, then seeded random ones:
    /// raw f64 bit patterns (NaN payloads of both signs included), raw i64s,
    /// and Int/Float pairs straddling 2^53 and the i64 limits.
    fn pool() -> Vec<Value> {
        let mut v = vec![
            Value::Null,
            Value::Bool(false),
            Value::Bool(true),
            Value::Str(String::new()),
            Value::Str("a".into()),
            Value::Str("a\0".into()),
            Value::Str("a\0b".into()),
            Value::Str("é".into()),
            Value::Bytes(vec![]),
            Value::Bytes(vec![0]),
            Value::Bytes(vec![0, 0xFF]),
            Value::Bytes(vec![1]),
            Value::Array(vec![]),
            Value::Array(vec![Value::Int(1)]),
            Value::Object(vec![]),
            Value::Object(vec![("k".into(), Value::Null)]),
        ];
        for n in [
            i64::MIN,
            i64::MIN + 1,
            -TWO_53 - 1,
            -TWO_53,
            -TWO_53 + 1,
            -1,
            0,
            1,
            TWO_53 - 1,
            TWO_53,
            TWO_53 + 1,
            TWO_53 + 2,
            1 << 62,
            i64::MAX - 1,
            i64::MAX,
        ] {
            v.push(Value::Int(n));
        }
        for bits in [
            0x7FF8_0000_0000_0000u64, // f64::NAN
            0xFFF8_0000_0000_0000,    // -NaN, x86's default NaN
            0x7FF0_0000_0000_0001,
            0x7FFF_FFFF_FFFF_FFFF,
            0xFFF0_0000_0000_0001,
            0xFFFF_FFFF_FFFF_FFFF,
            1,                     // smallest subnormal
            0x8000_0000_0000_0001, // its negative
        ] {
            v.push(Value::Float(f64::from_bits(bits)));
        }
        for f in [
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.0,
            -0.0,
            f64::MAX,
            f64::MIN,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            0.1,
            0.5,
            -0.5,
            1.0,
            -1.0,
            (TWO_53 - 1) as f64,
            TWO_53 as f64,
            (TWO_53 + 2) as f64,
            -(TWO_53 as f64),
            TWO_63,
            -TWO_63,
            9_223_372_036_854_774_784.0, // largest f64 below 2^63
            1e300,
            -1e300,
        ] {
            v.push(Value::Float(f));
        }
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..400 {
            let r = next();
            v.push(Value::Float(f64::from_bits(r)));
            v.push(Value::Int(r as i64));
            let near = TWO_53 + (r % 9) as i64 - 4;
            v.push(Value::Int(near));
            v.push(Value::Float(near as f64));
            let wide = i64::MAX - (r % 4096) as i64;
            v.push(Value::Int(wide));
            v.push(Value::Float(wide as f64));
            v.push(Value::Int(-wide));
            v.push(Value::Float(-(wide as f64)));
        }
        v
    }

    /// Independent numeric oracle: NaN above everything, zeros equal, and
    /// Int-vs-Float by comparing integer parts via `floor` in i128 (the
    /// comparator uses `trunc` in i64).
    fn oracle(a: &Value, b: &Value) -> Option<Ordering> {
        fn int_float(x: i64, y: f64) -> Ordering {
            if y.is_nan() || y >= TWO_63 {
                return Less;
            }
            if y < -TWO_63 {
                return Greater;
            }
            let floor = y.floor();
            match i128::from(x).cmp(&(floor as i128)) {
                Equal if y == floor => Equal,
                Equal => Less,
                ord => ord,
            }
        }
        let canon = |f: f64| if f == 0.0 { 0.0 } else { f };
        Some(match (a, b) {
            (Value::Int(x), Value::Int(y)) => x.cmp(y),
            (Value::Int(x), Value::Float(y)) => int_float(*x, *y),
            (Value::Float(x), Value::Int(y)) => int_float(*y, *x).reverse(),
            (Value::Float(x), Value::Float(y)) => match (x.is_nan(), y.is_nan()) {
                (true, true) => Equal,
                (true, false) => Greater,
                (false, true) => Less,
                (false, false) => canon(*x).total_cmp(&canon(*y)),
            },
            _ => return None,
        })
    }

    #[test]
    fn cmp_values_is_a_total_preorder() {
        let values = pool();
        for a in &values {
            assert_eq!(cmp_values(a, a), Equal, "reflexive: {a:?}");
            for b in &values {
                assert_eq!(
                    cmp_values(a, b),
                    cmp_values(b, a).reverse(),
                    "antisymmetric: {a:?} vs {b:?}"
                );
            }
        }
        // A relation is a total preorder exactly when some arrangement puts
        // every pair in order, with equal values adjacent. Sort, then check
        // every pair of the result rather than trusting the sort.
        let mut sorted = values;
        sorted.sort_by(cmp_values);
        for i in 0..sorted.len() {
            for j in i + 1..sorted.len() {
                let ord = cmp_values(&sorted[i], &sorted[j]);
                assert_ne!(ord, Greater, "{:?} > {:?}", sorted[i], sorted[j]);
                if ord == Equal {
                    assert_eq!(
                        cmp_values(&sorted[j - 1], &sorted[j]),
                        Equal,
                        "equal values must be contiguous: {:?} .. {:?}",
                        sorted[i],
                        sorted[j]
                    );
                }
            }
        }
    }

    #[test]
    fn numbers_compare_by_exact_value() {
        let values = pool();
        for a in &values {
            for b in &values {
                if let Some(expected) = oracle(a, b) {
                    assert_eq!(cmp_values(a, b), expected, "{a:?} vs {b:?}");
                }
            }
        }
    }

    /// Wherever the old comparator was already well-defined — no NaN, and no
    /// Int that `as f64` rounds — the order is unchanged.
    #[test]
    fn finite_order_is_unchanged() {
        let old = |a: &Value, b: &Value| -> Ordering {
            match (a, b) {
                (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Equal),
                (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Equal),
                (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Equal),
                _ => cmp_values(a, b),
            }
        };
        let exact = |v: &Value| match v {
            Value::Float(f) => !f.is_nan(),
            Value::Int(n) => n.unsigned_abs() <= TWO_53 as u64,
            _ => true,
        };
        let values: Vec<Value> = pool().into_iter().filter(exact).collect();
        for a in &values {
            for b in &values {
                assert_eq!(cmp_values(a, b), old(a, b), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn documented_positions() {
        let nan = Value::Float(f64::NAN);
        let neg_nan = Value::Float(-f64::NAN);
        let inf = Value::Float(f64::INFINITY);
        assert_eq!(cmp_values(&nan, &neg_nan), Equal);
        assert_eq!(cmp_values(&nan, &inf), Greater);
        assert_eq!(cmp_values(&neg_nan, &inf), Greater);
        assert_eq!(cmp_values(&nan, &Value::Int(i64::MAX)), Greater);
        assert_eq!(cmp_values(&nan, &Value::Str(String::new())), Less);
        assert_eq!(cmp_values(&Value::Bool(true), &neg_nan), Less);
        assert_eq!(cmp_values(&Value::Float(-0.0), &Value::Float(0.0)), Equal);
        assert_eq!(cmp_values(&Value::Float(-0.0), &Value::Int(0)), Equal);
        assert_eq!(
            cmp_values(&Value::Int(TWO_53 + 1), &Value::Float(TWO_53 as f64)),
            Greater
        );
        assert_eq!(
            cmp_values(&Value::Int(i64::MAX), &Value::Float(TWO_63)),
            Less
        );
        assert_eq!(
            cmp_values(&Value::Int(i64::MIN), &Value::Float(-TWO_63)),
            Equal
        );
    }
}
