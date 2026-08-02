use std::ops::Bound;

use crate::document::Value;
use crate::fts::FtsDef;
use crate::index::{
    CompoundIndexDef, IndexDef, compound_range_eq, index_range_cmp, index_range_eq,
};
use crate::query::filter::Filter;

/// The execution plan produced by the query planner.
#[derive(Debug)]
pub enum QueryPlan {
    /// No usable index — scan all documents in the collection.
    FullScan,

    /// Direct primary-key lookups — `_id` equality / `$in` filters resolve to
    /// point gets on the docs table without touching any index.
    ById { ids: Vec<ulid::Ulid> },

    /// Use an index to do an exact equality lookup.
    IndexEq {
        field: String,
        start: Vec<u8>,
        end: Vec<u8>,
    },

    /// Use an index for a range scan (gt/gte/lt/lte).
    IndexRange {
        field: String,
        start: Bound<Vec<u8>>,
        end: Bound<Vec<u8>>,
    },

    /// Use an index for an $in lookup (union of point lookups).
    IndexIn {
        field: String,
        ranges: Vec<(Vec<u8>, Vec<u8>)>, // (start, end) per value
    },

    /// Union the results of multiple index-backed sub-plans ($or when all
    /// branches are index-accelerated).
    IndexOr { plans: Vec<QueryPlan> },

    /// Intersect the results of multiple index-backed sub-plans ($and where
    /// more than one conjunct is indexed).
    ///
    /// Without this the planner picks the *first* indexed conjunct and
    /// post-filters the rest, so `status == "active" AND userId == X` scans
    /// every active row to find one user's. Intersecting id sets instead bounds
    /// the work by the most selective branch, and only documents satisfying
    /// every indexed conjunct are ever decoded.
    IndexAnd { plans: Vec<QueryPlan> },

    /// Full-text search via the inverted token index.
    FtsSearch {
        field: String,
        /// Tokenized query terms — all must match (AND semantics).
        tokens: Vec<String>,
    },

    /// Compound index equality scan — all prefix fields are constrained by Eq.
    CompoundIndexEq {
        /// Ordered field names forming the compound key.
        fields: Vec<String>,
        start: Vec<u8>,
        end: Vec<u8>,
    },
}

/// Select the best execution plan for a filter given available indexes.
///
/// Strategy (greedy, not cost-based):
/// - For `And`, pick the first sub-filter that has an indexed field and convert it
///   to an index plan; remaining sub-filters become post-filters in the executor.
/// - For `Or`, use `IndexOr` when every branch is index-accelerated.
/// - `Contains` maps to `FtsSearch` when an FTS index exists for the field.
/// - Single equality / range / in — use index if available.
pub fn plan(filter: &Filter, indexes: &[IndexDef]) -> QueryPlan {
    plan_with_fts(filter, indexes, &[])
}

/// Extended planner that also considers full-text search indexes.
pub fn plan_with_fts(filter: &Filter, indexes: &[IndexDef], fts_indexes: &[FtsDef]) -> QueryPlan {
    plan_full(filter, indexes, fts_indexes, &[])
}

/// Full planner that considers single-field indexes, FTS indexes, and compound indexes.
pub fn plan_full(
    filter: &Filter,
    indexes: &[IndexDef],
    fts_indexes: &[FtsDef],
    compound_indexes: &[CompoundIndexDef],
) -> QueryPlan {
    let indexed_fields: Vec<&str> = indexes.iter().map(|i| i.field.as_str()).collect();
    let fts_fields: Vec<&str> = fts_indexes.iter().map(|i| i.field.as_str()).collect();
    plan_inner(filter, &indexed_fields, &fts_fields, compound_indexes)
}

fn plan_inner(
    filter: &Filter,
    indexed_fields: &[&str],
    fts_fields: &[&str],
    compound_indexes: &[CompoundIndexDef],
) -> QueryPlan {
    match filter {
        Filter::All => QueryPlan::FullScan,

        // `_id` is the primary key — resolve to direct gets, no index needed.
        // Values that are not valid ULID strings can never match (the filter
        // compares against `doc.id.to_string()`), so they contribute no ids.
        Filter::Eq(field, value) if field == "_id" => {
            let ids = match value {
                crate::document::Value::Str(s) => {
                    ulid::Ulid::from_string(s).ok().into_iter().collect()
                }
                _ => Vec::new(),
            };
            QueryPlan::ById { ids }
        }

        Filter::In(field, values) if field == "_id" => {
            let ids = values
                .iter()
                .filter_map(|v| match v {
                    crate::document::Value::Str(s) => ulid::Ulid::from_string(s).ok(),
                    _ => None,
                })
                .collect();
            QueryPlan::ById { ids }
        }

        Filter::Eq(field, value) if indexed_fields.contains(&field.as_str()) => {
            eq_plan(field, value).unwrap_or(QueryPlan::FullScan)
        }

        Filter::Gt(field, value) if indexed_fields.contains(&field.as_str()) => {
            lower_bound_plan(field, value, false).unwrap_or(QueryPlan::FullScan)
        }

        Filter::Gte(field, value) if indexed_fields.contains(&field.as_str()) => {
            lower_bound_plan(field, value, true).unwrap_or(QueryPlan::FullScan)
        }

        Filter::Lt(field, value) if indexed_fields.contains(&field.as_str()) => {
            upper_bound_plan(field, value, false).unwrap_or(QueryPlan::FullScan)
        }

        Filter::Lte(field, value) if indexed_fields.contains(&field.as_str()) => {
            upper_bound_plan(field, value, true).unwrap_or(QueryPlan::FullScan)
        }

        Filter::In(field, values) if indexed_fields.contains(&field.as_str()) => {
            let mut ranges: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(values.len());
            for v in values {
                if let Some(range) = index_range_eq(v) {
                    ranges.push(range);
                }
                // 0.0 and -0.0 compare equal but encode to different keys.
                if let Value::Float(f) = v
                    && *f == 0.0
                    && let Some(range) = index_range_eq(&Value::Float(-*f))
                {
                    ranges.push(range);
                }
            }
            if !ranges.is_empty() {
                return QueryPlan::IndexIn {
                    field: field.clone(),
                    ranges,
                };
            }
            QueryPlan::FullScan
        }

        // Full-text search — use FTS index when available
        Filter::Contains(field, query) if fts_fields.contains(&field.as_str()) => {
            use crate::fts::tokenize;
            let tokens = tokenize(query);
            if tokens.is_empty() {
                return QueryPlan::FullScan;
            }
            QueryPlan::FtsSearch {
                field: field.clone(),
                tokens,
            }
        }

        // For And: try compound indexes first, then single-field indexes
        Filter::And(filters) => {
            // Collect all Eq constraints from this And expression
            let eq_map: Vec<(&str, &crate::document::Value)> = filters
                .iter()
                .filter_map(|f| {
                    if let Filter::Eq(field, val) = f {
                        Some((field.as_str(), val))
                    } else {
                        None
                    }
                })
                .collect();

            // Try compound indexes: find one whose fields are all covered by Eq constraints
            for cidx in compound_indexes {
                let values: Option<Vec<&crate::document::Value>> = cidx
                    .fields
                    .iter()
                    .map(|f| {
                        eq_map
                            .iter()
                            .find(|(k, _)| *k == f.as_str())
                            .map(|(_, v)| *v)
                    })
                    .collect();
                if let Some(vals) = values {
                    let mut variants: Vec<Vec<crate::document::Value>> =
                        vec![vals.iter().map(|v| (*v).clone()).collect()];
                    for i in 0..vals.len() {
                        if matches!(vals[i], crate::document::Value::Float(f) if *f == 0.0) {
                            let alternate = crate::document::Value::Float(
                                if matches!(vals[i], crate::document::Value::Float(f) if f.is_sign_negative())
                                {
                                    0.0
                                } else {
                                    -0.0
                                },
                            );
                            let mut additional = variants.clone();
                            for variant in &mut additional {
                                variant[i] = alternate.clone();
                            }
                            variants.extend(additional);
                        }
                    }
                    let plans: Vec<_> = variants
                        .into_iter()
                        .filter_map(|values| {
                            let refs: Vec<_> = values.iter().collect();
                            compound_range_eq(&refs).map(|(start, end)| {
                                QueryPlan::CompoundIndexEq {
                                    fields: cidx.fields.clone(),
                                    start,
                                    end,
                                }
                            })
                        })
                        .collect();
                    if plans.len() == 1 {
                        return plans.into_iter().next().unwrap();
                    } else if !plans.is_empty() {
                        return QueryPlan::IndexOr { plans };
                    }
                }
            }

            // Two-sided range on one indexed field → a single bounded scan,
            // instead of the half-open scan the single-field fallback would
            // pick (which post-filters the far bound over the whole tail).
            if let Some(plan) = bounded_range_from_and(filters, indexed_fields) {
                return plan;
            }

            // More than one conjunct is index-backed → intersect their id sets
            // rather than picking one arbitrarily and post-filtering the rest.
            //
            // Only *equality* branches qualify. A range branch can cover a huge
            // slice of the index, so scanning it to intersect can cost more than
            // the post-filter it replaces — whereas an equality branch is
            // exactly the selective shape intersection is for. `_id` plans are
            // excluded too: they are already point lookups, and a single one is
            // strictly cheaper than any intersection.
            let indexed_eq: Vec<QueryPlan> = filters
                .iter()
                .filter(|f| {
                    matches!(f, Filter::Eq(field, _)
                        if field != "_id" && indexed_fields.contains(&field.as_str()))
                })
                .filter_map(|f| {
                    match plan_inner(f, indexed_fields, fts_fields, compound_indexes) {
                        QueryPlan::FullScan => None,
                        plan => Some(plan),
                    }
                })
                .collect();
            if indexed_eq.len() > 1 {
                return QueryPlan::IndexAnd { plans: indexed_eq };
            }

            // Fall back to single-field index on any sub-filter
            for f in filters {
                let sub = plan_inner(f, indexed_fields, fts_fields, compound_indexes);
                if !matches!(sub, QueryPlan::FullScan) {
                    return sub;
                }
            }
            QueryPlan::FullScan
        }

        // Or: use IndexOr only when every branch is index-backed
        Filter::Or(filters) => {
            let sub_plans: Vec<QueryPlan> = filters
                .iter()
                .map(|f| plan_inner(f, indexed_fields, fts_fields, compound_indexes))
                .collect();
            if sub_plans.iter().all(|p| !matches!(p, QueryPlan::FullScan)) {
                return QueryPlan::IndexOr { plans: sub_plans };
            }
            QueryPlan::FullScan
        }

        // Not / Ne / Nin / Exists / Regex / unindexed Contains — full scan
        _ => QueryPlan::FullScan,
    }
}

// ---------------------------------------------------------------------------
// Numeric cross-type planning
//
// Index keys are type-prefixed (Int = 0x20, Float = 0x30), so Int keys sort
// strictly below Float keys. The post-filter compares Int and Float values
// numerically (`partial_cmp_numeric`), which means a single-type byte range
// can *miss* documents the filter would match:
//
//   - `Lt/Lte(Int n)`  byte range ends inside the Int block → every Float
//     value, however small, falls outside the scan.
//   - `Gt/Gte(Float f)` byte range starts inside the Float block → every Int
//     value, however large, falls outside the scan.
//
// The post-filter can only remove false positives, never recover misses, so
// those cases must scan a second, conservatively-widened range of the other
// numeric type. Widening is safe (the post-filter trims extras); narrowing
// is not.
// ---------------------------------------------------------------------------

/// Plan a lower-bounded scan (`Gt`/`Gte`). Float bounds get an extra Int-block
/// range so integer values above the bound are not missed.
fn lower_bound_plan(field: &str, value: &Value, inclusive: bool) -> Option<QueryPlan> {
    let (start, end) = index_range_cmp(Some((value, inclusive)), None)?;
    let primary = QueryPlan::IndexRange {
        field: field.to_string(),
        start,
        end,
    };
    if let Value::Float(f) = value
        && !f.is_nan()
    {
        // Every Int ≥ floor(f) may satisfy the filter. `as` saturates at
        // the i64 limits, which stays conservative; the post-filter trims
        // the at-most-one extra integer below the bound.
        let twin_lo = Value::Int(f.floor() as i64);
        let twin_hi = Value::Int(i64::MAX);
        if let Some((s, e)) = index_range_cmp(Some((&twin_lo, true)), Some((&twin_hi, true))) {
            return Some(QueryPlan::IndexOr {
                plans: vec![
                    primary,
                    QueryPlan::IndexRange {
                        field: field.to_string(),
                        start: s,
                        end: e,
                    },
                ],
            });
        }
    }
    Some(primary)
}

/// Plan an upper-bounded scan (`Lt`/`Lte`). Int bounds get an extra
/// Float-block range so fractional values below the bound are not missed.
fn upper_bound_plan(field: &str, value: &Value, inclusive: bool) -> Option<QueryPlan> {
    let (start, end) = index_range_cmp(None, Some((value, inclusive)))?;
    let primary = QueryPlan::IndexRange {
        field: field.to_string(),
        start,
        end,
    };
    if let Value::Int(n) = value {
        // The i64→f64 conversion rounds by at most half an ULP, so one ULP up
        // is a safe inclusive ceiling for every Float ≤ n.
        let twin_hi = Value::Float(f64_next_up(*n as f64));
        let twin_lo = Value::Float(f64::NEG_INFINITY);
        if let Some((s, e)) = index_range_cmp(Some((&twin_lo, true)), Some((&twin_hi, true))) {
            return Some(QueryPlan::IndexOr {
                plans: vec![
                    primary,
                    QueryPlan::IndexRange {
                        field: field.to_string(),
                        start: s,
                        end: e,
                    },
                ],
            });
        }
    }
    Some(primary)
}

/// Plan an equality scan. `0.0` and `-0.0` compare equal in filters but
/// encode to different index keys, so both ranges are scanned.
fn eq_plan(field: &str, value: &Value) -> Option<QueryPlan> {
    let (start, end) = index_range_eq(value)?;
    if let Value::Float(f) = value
        && *f == 0.0
    {
        let (s2, e2) = index_range_eq(&Value::Float(-*f))?;
        return Some(QueryPlan::IndexIn {
            field: field.to_string(),
            ranges: vec![(start, end), (s2, e2)],
        });
    }
    Some(QueryPlan::IndexEq {
        field: field.to_string(),
        start,
        end,
    })
}

/// One-ULP increment (bit twiddling — avoids requiring Rust ≥ 1.86 for
/// `f64::next_up`). NaN and +∞ are returned unchanged.
fn f64_next_up(f: f64) -> f64 {
    if f.is_nan() || f == f64::INFINITY {
        return f;
    }
    let bits = if f == 0.0 {
        1 // smallest positive subnormal (covers -0.0 too)
    } else if f > 0.0 {
        f.to_bits() + 1
    } else {
        f.to_bits() - 1
    };
    f64::from_bits(bits)
}

/// One-ULP decrement. NaN and -∞ are returned unchanged.
fn f64_next_down(f: f64) -> f64 {
    -f64_next_up(-f)
}

/// Plan a two-sided range (`lower ≤ field {<,≤} upper`) on a single indexed
/// field as one bounded index scan, instead of a half-open scan that
/// post-filters the far bound. The executor always re-applies the full filter
/// (see `executor::execute`), so a conservatively *wide* range is safe — it
/// only affects how much of the index is scanned, never the result.
///
/// For numeric bounds this emits an `IndexOr` of an Int-typed and a Float-typed
/// bounded sub-range, each converted outward, so values stored as either
/// numeric type are covered (index keys are type-prefixed — the cross-type
/// correctness the single-bound planners handle for one side). Same-typed
/// string/bool bounds emit a single exact range; mismatched types return
/// `None` so the caller falls back to single-bound planning.
fn bounded_range_plan(
    field: &str,
    lower: (&Value, bool),
    upper: (&Value, bool),
) -> Option<QueryPlan> {
    let (lv, li) = lower;
    let (uv, ui) = upper;
    let both_numeric = matches!(lv, Value::Int(_) | Value::Float(_))
        && matches!(uv, Value::Int(_) | Value::Float(_));

    if both_numeric {
        // Int-typed sub-range: convert each float bound outward to a whole
        // number so no integer in the true range is excluded.
        let int_lo = match lv {
            Value::Int(n) => (Value::Int(*n), li),
            Value::Float(f) => (Value::Int(f.floor() as i64), true),
            _ => return None,
        };
        let int_hi = match uv {
            Value::Int(n) => (Value::Int(*n), ui),
            Value::Float(f) => (Value::Int(f.ceil() as i64), true),
            _ => return None,
        };
        // Float-typed sub-range: nudge integer bounds one ULP outward so
        // rounding on the i64→f64 conversion can never drop a boundary value.
        let flt_lo = match lv {
            Value::Float(f) => (Value::Float(*f), li),
            Value::Int(n) => (Value::Float(f64_next_down(*n as f64)), true),
            _ => return None,
        };
        let flt_hi = match uv {
            Value::Float(f) => (Value::Float(*f), ui),
            Value::Int(n) => (Value::Float(f64_next_up(*n as f64)), true),
            _ => return None,
        };

        let mut plans = Vec::with_capacity(2);
        if let Some((start, end)) =
            index_range_cmp(Some((&int_lo.0, int_lo.1)), Some((&int_hi.0, int_hi.1)))
        {
            plans.push(QueryPlan::IndexRange {
                field: field.to_string(),
                start,
                end,
            });
        }
        if let Some((start, end)) =
            index_range_cmp(Some((&flt_lo.0, flt_lo.1)), Some((&flt_hi.0, flt_hi.1)))
        {
            plans.push(QueryPlan::IndexRange {
                field: field.to_string(),
                start,
                end,
            });
        }
        return match plans.len() {
            0 => None,
            1 => plans.pop(),
            _ => Some(QueryPlan::IndexOr { plans }),
        };
    }

    // Same non-numeric type — one exact bounded range.
    if std::mem::discriminant(lv) == std::mem::discriminant(uv) {
        let (start, end) = index_range_cmp(Some((lv, li)), Some((uv, ui)))?;
        return Some(QueryPlan::IndexRange {
            field: field.to_string(),
            start,
            end,
        });
    }

    None
}

/// Scan an `And`'s direct sub-filters for a lower **and** upper bound on the
/// same indexed field and, if found, plan it as one bounded range.
fn bounded_range_from_and(filters: &[Filter], indexed_fields: &[&str]) -> Option<QueryPlan> {
    for field in indexed_fields {
        let mut lower: Option<(&Value, bool)> = None;
        let mut upper: Option<(&Value, bool)> = None;
        for f in filters {
            match f {
                Filter::Gt(fl, v) if fl == field => lower = Some((v, false)),
                Filter::Gte(fl, v) if fl == field => lower = Some((v, true)),
                Filter::Lt(fl, v) if fl == field => upper = Some((v, false)),
                Filter::Lte(fl, v) if fl == field => upper = Some((v, true)),
                _ => {}
            }
        }
        if let (Some(lo), Some(hi)) = (lower, upper)
            && let Some(plan) = bounded_range_plan(field, lo, hi)
        {
            return Some(plan);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Value;

    fn indexes(fields: &[&str]) -> Vec<IndexDef> {
        fields
            .iter()
            .map(|f| IndexDef {
                collection: "col".into(),
                field: f.to_string(),
            })
            .collect()
    }

    #[test]
    fn eq_with_index() {
        let f = Filter::Eq("age".into(), Value::Int(30));
        let plan = plan(&f, &indexes(&["age"]));
        assert!(matches!(plan, QueryPlan::IndexEq { .. }));
    }

    #[test]
    fn eq_without_index() {
        let f = Filter::Eq("age".into(), Value::Int(30));
        let plan = plan(&f, &indexes(&[]));
        assert!(matches!(plan, QueryPlan::FullScan));
    }

    #[test]
    fn range_with_index() {
        // A Float lower bound plans the Float range plus an Int-block twin
        // range (Int keys sort below Float keys), unioned via IndexOr.
        let f = Filter::Gte("score".into(), Value::Float(5.0));
        let plan = plan(&f, &indexes(&["score"]));
        match plan {
            QueryPlan::IndexOr { plans } => {
                assert_eq!(plans.len(), 2);
                assert!(
                    plans
                        .iter()
                        .all(|p| matches!(p, QueryPlan::IndexRange { .. }))
                );
            }
            other => panic!("expected IndexOr of two ranges, got {other:?}"),
        }
    }

    #[test]
    fn range_with_index_string_bound_stays_single_range() {
        // Non-numeric bounds have no cross-type twin.
        let f = Filter::Gte("name".into(), Value::Str("m".into()));
        let plan = plan(&f, &indexes(&["name"]));
        assert!(matches!(plan, QueryPlan::IndexRange { .. }));
    }

    #[test]
    fn in_with_index() {
        let f = Filter::In(
            "status".into(),
            vec![Value::Str("active".into()), Value::Str("pending".into())],
        );
        let plan = plan(&f, &indexes(&["status"]));
        assert!(matches!(plan, QueryPlan::IndexIn { .. }));
    }

    #[test]
    fn and_picks_indexed_subfilter() {
        let f = Filter::And(vec![
            Filter::Eq("unindexed".into(), Value::Int(1)),
            Filter::Eq("age".into(), Value::Int(30)),
        ]);
        let plan = plan(&f, &indexes(&["age"]));
        assert!(matches!(plan, QueryPlan::IndexEq { field, .. } if field == "age"));
    }

    #[test]
    fn or_all_indexed_uses_index_or() {
        let f = Filter::Or(vec![
            Filter::Eq("age".into(), Value::Int(30)),
            Filter::Eq("age".into(), Value::Int(40)),
        ]);
        let plan = plan(&f, &indexes(&["age"]));
        assert!(matches!(plan, QueryPlan::IndexOr { .. }));
    }

    #[test]
    fn or_partially_unindexed_falls_back() {
        let f = Filter::Or(vec![
            Filter::Eq("age".into(), Value::Int(30)),
            Filter::Eq("unindexed".into(), Value::Int(40)),
        ]);
        let plan = plan(&f, &indexes(&["age"]));
        assert!(matches!(plan, QueryPlan::FullScan));
    }
}
