//! Resolve positive filters from index keys. Residual predicates still require
//! documents; negation and existence cannot be answered by sparse indexes.
use std::collections::HashSet;
use std::ops::Bound;

use super::planner::plan_full;
use super::{Filter, QueryPlan};
use crate::document::Value;
use crate::engine::{ReadTxn, ScanFlow};
use crate::error::TalaDbError;
use crate::index::{
    CompoundIndexDef, IndexDef, META_INDEX_ARRAYS_TABLE, compound_table_name, decode_value_prefix,
    index_array_count, index_table_name, meta_key,
};

// Kept private to filter execution: a capped collection requests streaming,
// while a real storage/decoding error must still reach the caller.
pub(super) enum ResolveError {
    Limit,
    Database(TalaDbError),
}
impl From<TalaDbError> for ResolveError {
    fn from(error: TalaDbError) -> Self {
        Self::Database(error)
    }
}
type ResolveResult<T> = Result<T, ResolveError>;

pub(super) struct Candidates {
    pub ids: HashSet<[u8; 16]>,
    pub exact: bool,
}

pub(super) fn resolve(
    filter: &Filter,
    indexes: &[IndexDef],
    compounds: &[CompoundIndexDef],
    txn: &dyn ReadTxn,
    collection: &str,
    limit: usize,
) -> ResolveResult<Option<Candidates>> {
    match filter {
        Filter::And(children) => resolve_and(children, indexes, compounds, txn, collection, limit),
        Filter::Or(children) => {
            let mut result = Candidates {
                ids: HashSet::new(),
                exact: true,
            };
            for child in children {
                let Some(next) = resolve(child, indexes, compounds, txn, collection, limit)? else {
                    return Ok(None);
                };
                for id in next.ids {
                    if result.ids.len() >= limit && !result.ids.contains(&id) {
                        return Err(ResolveError::Limit);
                    }
                    result.ids.insert(id);
                }
                result.exact &= next.exact;
            }
            Ok(Some(result))
        }
        Filter::Eq(field, value)
        | Filter::Gt(field, value)
        | Filter::Gte(field, value)
        | Filter::Lt(field, value)
        | Filter::Lte(field, value)
            if field != "_id" && indexes.iter().any(|i| &i.field == field) && indexable(value) =>
        {
            let plan = plan_full(filter, indexes, &[], &[]);
            let mut ids = HashSet::new();
            scan_plan(
                &plan,
                &[filter],
                txn,
                collection,
                &mut Scan::all(&mut ids, limit),
            )?;
            Ok(Some(Candidates { ids, exact: true }))
        }
        Filter::In(field, values)
            if field != "_id"
                && indexes.iter().any(|i| &i.field == field)
                && values.iter().all(indexable) =>
        {
            let mut ids = HashSet::new();
            if !values.is_empty() {
                let plan = plan_full(filter, indexes, &[], &[]);
                scan_plan(
                    &plan,
                    &[filter],
                    txn,
                    collection,
                    &mut Scan::all(&mut ids, limit),
                )?;
            }
            Ok(Some(Candidates { ids, exact: true }))
        }
        _ => Ok(None),
    }
}

// A capped preview estimates whether a branch is cheaper to enumerate than
// to probe for an existing, smaller candidate set. It never changes eligibility.
const PREVIEW_KEYS: usize = 64;
// Nested branches under a selected AND seed stop at this many IDs per seed ID.
const SEED_FANOUT: usize = 16;
const MIN_NESTED_IDS: usize = 256;

struct Branch<'a> {
    plan: QueryPlan,
    predicates: Vec<&'a Filter>,
    field: Option<&'a str>,
}

fn leaf_field(filter: &Filter) -> Option<&str> {
    match filter {
        Filter::Eq(f, v)
        | Filter::Gt(f, v)
        | Filter::Gte(f, v)
        | Filter::Lt(f, v)
        | Filter::Lte(f, v)
            if f != "_id" && indexable(v) =>
        {
            Some(f)
        }
        Filter::In(f, values) if f != "_id" && values.iter().all(indexable) => Some(f),
        _ => None,
    }
}

fn comparison(filter: &Filter) -> bool {
    matches!(
        filter,
        Filter::Gt(..) | Filter::Gte(..) | Filter::Lt(..) | Filter::Lte(..)
    )
}

fn resolve_and(
    children: &[Filter],
    indexes: &[IndexDef],
    compounds: &[CompoundIndexDef],
    txn: &dyn ReadTxn,
    collection: &str,
    limit: usize,
) -> ResolveResult<Option<Candidates>> {
    let mut branches = Vec::new();
    let mut covered = vec![false; children.len()];
    if !compounds.is_empty() {
        let plan = plan_full(&Filter::And(children.to_vec()), &[], &[], compounds);
        if let Some(fields) = compound_fields(&plan) {
            let predicates: Vec<_> = fields
                .iter()
                .filter_map(|field| {
                    children
                        .iter()
                        .find(|f| matches!(f, Filter::Eq(name, _) if name == field))
                })
                .collect();
            for (i, child) in children.iter().enumerate() {
                covered[i] = predicates.iter().any(|p| {
                    matches!((child, p),
                    (Filter::Eq(a, x), Filter::Eq(b, y)) if a == b && x == y)
                });
            }
            branches.push(Branch {
                plan,
                predicates,
                field: None,
            });
        }
    }
    for (i, child) in children.iter().enumerate() {
        if covered[i] {
            continue;
        }
        let Some(field) = leaf_field(child) else {
            continue;
        };
        if !indexes.iter().any(|idx| idx.field == field) {
            continue;
        }
        let plan = if matches!(child, Filter::In(_, v) if v.is_empty()) {
            QueryPlan::IndexOr { plans: vec![] }
        } else {
            plan_full(child, indexes, &[], &[])
        };
        covered[i] = true;
        // Only scalar fields guarantee that one key can satisfy all predicates.
        // Array comparisons can be witnessed by separate elements.
        if comparison(child)
            && let Some(existing) = branches
                .iter_mut()
                .find(|b| b.field == Some(field) && b.predicates.iter().all(|p| comparison(p)))
        {
            let scalar = txn
                .get(
                    META_INDEX_ARRAYS_TABLE,
                    meta_key(collection, field).as_bytes(),
                )?
                .and_then(|bytes| index_array_count(&bytes))
                == Some(0);
            if scalar {
                existing.plan = intersect_ranges(&existing.plan, &plan, field);
                existing.predicates.push(child);
                continue;
            }
        }
        branches.push(Branch {
            plan,
            predicates: vec![child],
            field: Some(field),
        });
    }

    let mut result: Option<Candidates> = None;
    if branches.len() == 1 {
        let branch = &branches[0];
        let mut ids = HashSet::new();
        scan_plan(
            &branch.plan,
            &branch.predicates,
            txn,
            collection,
            &mut Scan::all(&mut ids, limit),
        )?;
        result = Some(Candidates { ids, exact: true });
    } else if !branches.is_empty() {
        let mut previews = Vec::with_capacity(branches.len());
        for branch in &branches {
            let mut ids = HashSet::new();
            let mut scan = Scan {
                ids: &mut ids,
                allowed: None,
                remaining: Some(PREVIEW_KEYS),
                seen: 0,
                limit,
            };
            scan_plan(&branch.plan, &branch.predicates, txn, collection, &mut scan)?;
            let complete = scan.seen < PREVIEW_KEYS;
            let seen = scan.seen;
            // An exhausted, empty branch proves this AND false immediately.
            if complete && ids.is_empty() {
                return Ok(Some(Candidates { ids, exact: true }));
            }
            previews.push((ids, complete, seen));
        }
        // Prefer a completely enumerated small branch over an unknown wide one.
        let first = (0..branches.len())
            .min_by_key(|&i| (!previews[i].1, previews[i].0.len()))
            .unwrap();
        let (mut ids, complete, _) = std::mem::take(&mut previews[first]);
        if !complete {
            scan_plan(
                &branches[first].plan,
                &branches[first].predicates,
                txn,
                collection,
                &mut Scan::all(&mut ids, limit),
            )?;
        }
        for (i, branch) in branches.iter().enumerate() {
            if i == first {
                continue;
            }
            if ids.is_empty() {
                break;
            }
            let (preview, complete, seen) = &previews[i];
            if *complete {
                ids.retain(|id| preview.contains(id));
            } else if let Some(prefixes) = point_prefixes(&branch.plan)
                && ids.len().saturating_mul(prefixes.len()) < *seen
            {
                probe_points(txn, collection, branch, &prefixes, &mut ids)?;
            } else {
                // Retain only IDs in the current intersection, avoiding another
                // full-size set even when a range still needs a complete walk.
                let mut next = HashSet::new();
                scan_plan(
                    &branch.plan,
                    &branch.predicates,
                    txn,
                    collection,
                    &mut Scan {
                        ids: &mut next,
                        allowed: Some(&ids),
                        remaining: None,
                        seen: 0,
                        limit,
                    },
                )?;
                ids = next;
            }
        }
        result = Some(Candidates { ids, exact: true });
    }
    let mut exact = true;
    for (child, covered) in children.iter().zip(covered) {
        if covered || matches!(child, Filter::All) {
            continue;
        }
        if result.as_ref().is_some_and(|r| r.ids.is_empty()) {
            break;
        }
        // Past a small multiple of the seed, reading the seed's documents is
        // cheaper than enumerating the nested branch, however large the limit.
        let nested_limit = result.as_ref().map_or(limit, |seed| {
            limit.min(
                seed.ids
                    .len()
                    .saturating_mul(SEED_FANOUT)
                    .max(MIN_NESTED_IDS),
            )
        });
        let next = match resolve(child, indexes, compounds, txn, collection, nested_limit) {
            // A broad nested branch must not discard a small seed already
            // selected by this AND. Check the remaining predicate on that seed.
            Err(ResolveError::Limit) if result.is_some() => {
                exact = false;
                continue;
            }
            other => other?,
        };
        if let Some(next) = next {
            exact &= next.exact;
            if let Some(current) = &mut result {
                intersect(&mut current.ids, next.ids);
            } else {
                result = Some(next);
            }
        } else {
            exact = false;
        }
    }
    if let Some(current) = &mut result {
        current.exact = current.ids.is_empty() || exact;
    }
    Ok(result)
}

// Point probes work for both scalar and array equality: each matching element
// has the same value-prefix ++ ID key. Batch the reads to open each table once.
fn point_prefixes(plan: &QueryPlan) -> Option<Vec<Vec<u8>>> {
    match plan {
        QueryPlan::IndexEq { start, .. } | QueryPlan::CompoundIndexEq { start, .. } => {
            Some(vec![start[..start.len() - 16].to_vec()])
        }
        QueryPlan::IndexIn { ranges, .. } => Some(
            ranges
                .iter()
                .map(|(s, _)| s[..s.len() - 16].to_vec())
                .collect(),
        ),
        QueryPlan::IndexOr { plans } => {
            let mut prefixes = Vec::new();
            for plan in plans {
                prefixes.extend(point_prefixes(plan)?);
            }
            Some(prefixes)
        }
        _ => None,
    }
}

fn probe_points(
    txn: &dyn ReadTxn,
    collection: &str,
    branch: &Branch<'_>,
    prefixes: &[Vec<u8>],
    ids: &mut HashSet<[u8; 16]>,
) -> Result<(), TalaDbError> {
    let table = if let Some(field) = branch.field {
        index_table_name(collection, field)
    } else {
        let fields = compound_fields(&branch.plan).unwrap();
        compound_table_name(
            collection,
            &fields.iter().map(String::as_str).collect::<Vec<_>>(),
        )
    };
    let candidates: Vec<_> = ids.iter().copied().collect();
    let mut accepted = HashSet::new();
    for chunk in candidates.chunks(256) {
        for prefix in prefixes {
            // A NaN equality key can exist, but never satisfies equality.
            if !key_matches(prefix, &branch.predicates, branch.field.is_none())? {
                continue;
            }
            let keys: Vec<_> = chunk
                .iter()
                .map(|id| {
                    let mut key = prefix.clone();
                    key.extend_from_slice(id);
                    key
                })
                .collect();
            let refs: Vec<_> = keys.iter().map(Vec::as_slice).collect();
            for (id, value) in chunk.iter().zip(txn.get_many(&table, &refs)?) {
                if value.is_some() {
                    accepted.insert(*id);
                }
            }
        }
    }
    *ids = accepted;
    Ok(())
}

type KeyRange = (Bound<Vec<u8>>, Bound<Vec<u8>>);

fn ranges(plan: &QueryPlan) -> Vec<KeyRange> {
    match plan {
        QueryPlan::IndexEq { start, end, .. } => {
            vec![(Bound::Included(start.clone()), Bound::Included(end.clone()))]
        }
        QueryPlan::IndexRange { start, end, .. } => vec![(start.clone(), end.clone())],
        QueryPlan::IndexIn { ranges, .. } => ranges
            .iter()
            .map(|(s, e)| (Bound::Included(s.clone()), Bound::Included(e.clone())))
            .collect(),
        QueryPlan::IndexOr { plans } => plans.iter().flat_map(ranges).collect(),
        _ => unreachable!("only single-field plans are narrowed"),
    }
}

fn intersect_ranges(a: &QueryPlan, b: &QueryPlan, field: &str) -> QueryPlan {
    let mut plans = Vec::new();
    for (alo, ahi) in ranges(a) {
        for (blo, bhi) in ranges(b) {
            let start = tighter(alo.clone(), blo, true);
            let end = tighter(ahi.clone(), bhi, false);
            let valid = match (&start, &end) {
                (Bound::Unbounded, _) | (_, Bound::Unbounded) => true,
                (Bound::Included(a), Bound::Included(b)) => a <= b,
                (
                    Bound::Included(a) | Bound::Excluded(a),
                    Bound::Included(b) | Bound::Excluded(b),
                ) => a < b,
            };
            if valid
                && !plans.iter().any(|p| {
                    matches!(p,
                QueryPlan::IndexRange { start: s, end: e, .. } if s == &start && e == &end)
                })
            {
                plans.push(QueryPlan::IndexRange {
                    field: field.into(),
                    start,
                    end,
                });
            }
        }
    }
    QueryPlan::IndexOr { plans }
}

fn tighter(a: Bound<Vec<u8>>, b: Bound<Vec<u8>>, lower: bool) -> Bound<Vec<u8>> {
    use std::cmp::Ordering;
    match (&a, &b) {
        (Bound::Unbounded, _) => b,
        (_, Bound::Unbounded) => a,
        (Bound::Included(x) | Bound::Excluded(x), Bound::Included(y) | Bound::Excluded(y)) => {
            match x.cmp(y) {
                Ordering::Equal if matches!(b, Bound::Excluded(_)) => b,
                Ordering::Equal => a,
                Ordering::Greater if lower => a,
                Ordering::Less if !lower => a,
                _ => b,
            }
        }
    }
}

fn indexable(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_))
}

fn intersect(a: &mut HashSet<[u8; 16]>, mut b: HashSet<[u8; 16]>) {
    if b.len() < a.len() {
        std::mem::swap(a, &mut b);
    }
    a.retain(|id| b.contains(id));
}

fn compound_fields(plan: &QueryPlan) -> Option<&[String]> {
    match plan {
        QueryPlan::CompoundIndexEq { fields, .. } => Some(fields),
        QueryPlan::IndexOr { plans } => {
            let fields = compound_fields(plans.first()?)?;
            plans
                .iter()
                .all(|p| compound_fields(p) == Some(fields))
                .then_some(fields)
        }
        _ => None,
    }
}

fn scan_plan(
    plan: &QueryPlan,
    predicates: &[&Filter],
    txn: &dyn ReadTxn,
    collection: &str,
    scan: &mut Scan<'_>,
) -> ResolveResult<()> {
    if scan.remaining == Some(0) {
        return Ok(());
    }
    match plan {
        QueryPlan::IndexEq { field, start, end } => scan_keys(
            txn,
            &index_table_name(collection, field),
            Bound::Included(start),
            Bound::Included(end),
            predicates,
            false,
            scan,
        ),
        QueryPlan::IndexRange { field, start, end } => scan_keys(
            txn,
            &index_table_name(collection, field),
            start.as_ref().map(Vec::as_slice),
            end.as_ref().map(Vec::as_slice),
            predicates,
            false,
            scan,
        ),
        QueryPlan::IndexIn { field, ranges } => {
            for (start, end) in ranges {
                scan_keys(
                    txn,
                    &index_table_name(collection, field),
                    Bound::Included(start),
                    Bound::Included(end),
                    predicates,
                    false,
                    scan,
                )?;
            }
            Ok(())
        }
        QueryPlan::CompoundIndexEq { fields, start, end } => {
            let refs: Vec<_> = fields.iter().map(String::as_str).collect();
            scan_keys(
                txn,
                &compound_table_name(collection, &refs),
                Bound::Included(start),
                Bound::Included(end),
                predicates,
                true,
                scan,
            )
        }
        QueryPlan::IndexOr { plans } => {
            for plan in plans {
                scan_plan(plan, predicates, txn, collection, scan)?;
            }
            Ok(())
        }
        _ => Err(TalaDbError::InvalidOperation("unsupported covered filter plan".into()).into()),
    }
}

struct Scan<'a> {
    ids: &'a mut HashSet<[u8; 16]>,
    allowed: Option<&'a HashSet<[u8; 16]>>,
    remaining: Option<usize>,
    seen: usize,
    limit: usize,
}
impl<'a> Scan<'a> {
    fn all(ids: &'a mut HashSet<[u8; 16]>, limit: usize) -> Self {
        Self {
            ids,
            allowed: None,
            remaining: None,
            seen: 0,
            limit,
        }
    }
}

fn malformed() -> TalaDbError {
    TalaDbError::InvalidOperation("invalid secondary index key".into())
}

fn key_matches(
    mut prefix: &[u8],
    predicates: &[&Filter],
    compound: bool,
) -> Result<bool, TalaDbError> {
    let mut matches = true;
    if compound {
        for filter in predicates {
            let (value, rest) = decode_value_prefix(prefix).ok_or_else(malformed)?;
            matches &= filter.matches_index_value(&value);
            prefix = rest;
        }
    } else {
        let (value, rest) = decode_value_prefix(prefix).ok_or_else(malformed)?;
        matches = predicates.iter().all(|f| f.matches_index_value(&value));
        prefix = rest;
    }
    if !prefix.is_empty() {
        return Err(malformed());
    }
    Ok(matches)
}

fn scan_keys(
    txn: &dyn ReadTxn,
    table: &str,
    start: Bound<&[u8]>,
    end: Bound<&[u8]>,
    predicates: &[&Filter],
    compound: bool,
    scan: &mut Scan<'_>,
) -> ResolveResult<()> {
    if scan.remaining == Some(0) {
        return Ok(());
    }
    let mut overflow = false;
    txn.scan(table, start, end, &mut |key, _| {
        let split = key.len().checked_sub(16).ok_or_else(malformed)?;
        let id = key[split..].try_into().map_err(|_| malformed())?;
        let matches = key_matches(&key[..split], predicates, compound)?;
        if matches && scan.allowed.is_none_or(|ids| ids.contains(&id)) {
            if scan.ids.len() >= scan.limit && !scan.ids.contains(&id) {
                overflow = true;
                return Ok(ScanFlow::Stop);
            }
            scan.ids.insert(id);
        }
        scan.seen += 1;
        if let Some(remaining) = &mut scan.remaining {
            *remaining -= 1;
            if *remaining == 0 {
                return Ok(ScanFlow::Stop);
            }
        }
        Ok(ScanFlow::Continue)
    })?;
    if overflow {
        Err(ResolveError::Limit)
    } else {
        Ok(())
    }
}

/// Visit a covered index plan only when each matching document is emitted once.
/// Array equality on a single key is unique; range unions require scalar-only
/// metadata. Other filter shapes stream document projections instead.
pub(super) fn visit_unique(
    filter: &Filter,
    indexes: &[IndexDef],
    compounds: &[CompoundIndexDef],
    txn: &dyn ReadTxn,
    collection: &str,
    accept: &mut dyn FnMut([u8; 16]) -> Result<(), TalaDbError>,
) -> Result<bool, TalaDbError> {
    let (plan, predicates, table, compound) = match filter {
        Filter::And(children) => {
            let plan = plan_full(filter, indexes, &[], compounds);
            if let QueryPlan::CompoundIndexEq { fields, .. } = &plan {
                let predicates: Vec<_> = fields
                    .iter()
                    .filter_map(|field| {
                        children
                            .iter()
                            .find(|p| matches!(p, Filter::Eq(f, _) if f == field))
                    })
                    .collect();
                if !children.iter().all(|child| {
                    matches!(child, Filter::All) || predicates.iter().any(|p|
                    matches!((child, p), (Filter::Eq(a, x), Filter::Eq(b, y)) if a == b && x == y))
                }) {
                    return Ok(false);
                }
                let table = compound_table_name(
                    collection,
                    &fields.iter().map(String::as_str).collect::<Vec<_>>(),
                );
                (plan, predicates, table, true)
            } else {
                let predicates: Vec<_> = children
                    .iter()
                    .filter(|f| !matches!(f, Filter::All))
                    .collect();
                let Some(field) = predicates.first().and_then(|f| leaf_field(f)) else {
                    return Ok(false);
                };
                if !indexes.iter().any(|i| i.field == field)
                    || !predicates
                        .iter()
                        .all(|p| comparison(p) && leaf_field(p) == Some(field))
                    || !scalar_field(txn, collection, field)?
                {
                    return Ok(false);
                }
                let mut plan = plan_full(predicates[0], indexes, &[], &[]);
                for p in &predicates[1..] {
                    plan = intersect_ranges(&plan, &plan_full(p, indexes, &[], &[]), field);
                }
                (plan, predicates, index_table_name(collection, field), false)
            }
        }
        _ => {
            let Some(field) = leaf_field(filter) else {
                return Ok(false);
            };
            if !indexes.iter().any(|i| i.field == field) {
                return Ok(false);
            }
            let plan = if matches!(filter, Filter::In(_, v) if v.is_empty()) {
                QueryPlan::IndexOr { plans: vec![] }
            } else {
                plan_full(filter, indexes, &[], &[])
            };
            if !matches!(plan, QueryPlan::IndexEq { .. }) && !scalar_field(txn, collection, field)?
            {
                return Ok(false);
            }
            (
                plan,
                vec![filter],
                index_table_name(collection, field),
                false,
            )
        }
    };
    let intervals = if let QueryPlan::CompoundIndexEq { start, end, .. } = &plan {
        vec![(Bound::Included(start.clone()), Bound::Included(end.clone()))]
    } else {
        union_ranges(ranges(&plan))
    };
    for (start, end) in intervals {
        txn.scan(
            &table,
            start.as_ref().map(Vec::as_slice),
            end.as_ref().map(Vec::as_slice),
            &mut |key, _| {
                let split = key.len().checked_sub(16).ok_or_else(malformed)?;
                if key_matches(&key[..split], &predicates, compound)? {
                    accept(key[split..].try_into().map_err(|_| malformed())?)?;
                }
                Ok(ScanFlow::Continue)
            },
        )?;
    }
    Ok(true)
}

/// A two-sided range on one indexed field — planned as the `IndexAnd` of its
/// lower and upper bound by `planner::bounded_range_from_and` — narrowed to the
/// key ranges both bounds share, when that is provably the same answer.
///
/// The planner intersects document IDs because an array document has one key
/// per element, and different elements can satisfy each bound. A field whose
/// array counter is zero in this snapshot has at most one key per document, so
/// a document is in both bounds' ID sets exactly when its single key lies in
/// both bounds' key ranges: the pairwise intersection [`intersect_ranges`]
/// computes, including the Int/Float twin ranges. The result is disjoint and in
/// key order, so it can be walked without a dedup set and stop at a limit.
///
/// `None` keeps the ID intersection: any other plan shape, a field that holds
/// arrays, and a field without a counter (written before counters existed).
pub(super) fn scalar_range_plan(
    plan: &QueryPlan,
    txn: &dyn ReadTxn,
    collection: &str,
) -> Result<Option<QueryPlan>, TalaDbError> {
    let QueryPlan::IndexAnd { plans } = plan else {
        return Ok(None);
    };
    let [lower, upper] = plans.as_slice() else {
        return Ok(None);
    };
    let (Some(field), Some(other)) = (range_field(lower), range_field(upper)) else {
        return Ok(None);
    };
    if field != other || !scalar_field(txn, collection, field)? {
        return Ok(None);
    }
    let narrowed = intersect_ranges(lower, upper, field);
    Ok(Some(QueryPlan::IndexOr {
        plans: union_ranges(ranges(&narrowed))
            .into_iter()
            .map(|(start, end)| QueryPlan::IndexRange {
                field: field.to_string(),
                start,
                end,
            })
            .collect(),
    }))
}

/// The field of a plan made only of key ranges on one field.
fn range_field(plan: &QueryPlan) -> Option<&str> {
    match plan {
        QueryPlan::IndexRange { field, .. } => Some(field),
        QueryPlan::IndexOr { plans } => {
            let field = range_field(plans.first()?)?;
            plans
                .iter()
                .all(|p| range_field(p) == Some(field))
                .then_some(field)
        }
        _ => None,
    }
}

fn scalar_field(txn: &dyn ReadTxn, collection: &str, field: &str) -> Result<bool, TalaDbError> {
    Ok(txn
        .get(
            META_INDEX_ARRAYS_TABLE,
            meta_key(collection, field).as_bytes(),
        )?
        .and_then(|b| index_array_count(&b))
        == Some(0))
}

// Merge overlapping ranges (including duplicate $in entries) before visiting
// scalar keys. A scalar document contributes one key, so no ID set is needed.
fn union_ranges(mut ranges: Vec<KeyRange>) -> Vec<KeyRange> {
    ranges.sort_by(|(a, _), (b, _)| match (a, b) {
        (Bound::Unbounded, Bound::Unbounded) => std::cmp::Ordering::Equal,
        (Bound::Unbounded, _) => std::cmp::Ordering::Less,
        (_, Bound::Unbounded) => std::cmp::Ordering::Greater,
        (Bound::Included(a), Bound::Excluded(b)) => a.cmp(b).then(std::cmp::Ordering::Less),
        (Bound::Excluded(a), Bound::Included(b)) => a.cmp(b).then(std::cmp::Ordering::Greater),
        (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) => {
            a.cmp(b)
        }
    });
    let mut out: Vec<KeyRange> = Vec::new();
    for (start, end) in ranges {
        if let Some((_, previous_end)) = out.last_mut() {
            let overlap = match (&*previous_end, &start) {
                (Bound::Unbounded, _) | (_, Bound::Unbounded) => true,
                (Bound::Included(a), Bound::Included(b)) => a >= b,
                (
                    Bound::Included(a) | Bound::Excluded(a),
                    Bound::Included(b) | Bound::Excluded(b),
                ) => a > b,
            };
            if overlap {
                let extend = match (&*previous_end, &end) {
                    (Bound::Unbounded, _) => false,
                    (_, Bound::Unbounded) => true,
                    (Bound::Excluded(a), Bound::Included(b)) => b >= a,
                    (
                        Bound::Included(a) | Bound::Excluded(a),
                        Bound::Included(b) | Bound::Excluded(b),
                    ) => b > a,
                };
                if extend {
                    *previous_end = end;
                }
                continue;
            }
        }
        out.push((start, end));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Collection;
    use crate::engine::{RedbBackend, StorageBackend};
    use std::sync::Arc;

    fn collection(values: Vec<Value>) -> (Arc<RedbBackend>, Collection) {
        let backend = Arc::new(RedbBackend::open_in_memory().unwrap());
        let col = Collection::new("docs", backend.clone());
        col.insert_many(
            values
                .into_iter()
                .map(|v| vec![("n".to_string(), v)])
                .collect(),
        )
        .unwrap();
        col.create_index("n").unwrap();
        (backend, col)
    }

    fn two_sided(lo: Value, hi: Value) -> QueryPlan {
        let filter = Filter::And(vec![
            Filter::Gte("n".into(), lo),
            Filter::Lt("n".into(), hi),
        ]);
        let index = IndexDef {
            collection: "docs".into(),
            field: "n".into(),
        };
        plan_full(&filter, &[index], &[], &[])
    }

    fn bounded(plan: &QueryPlan) -> bool {
        matches!(plan, QueryPlan::IndexRange { start, end, .. }
            if !matches!(start, Bound::Unbounded) && !matches!(end, Bound::Unbounded))
    }

    #[test]
    fn scalar_fields_get_bounded_ranges() {
        let (backend, _col) = collection((0..100).map(Value::Int).collect());
        let txn = backend.begin_read().unwrap();
        let plan = two_sided(Value::Int(10), Value::Int(20));
        assert!(matches!(plan, QueryPlan::IndexAnd { .. }));
        let Some(QueryPlan::IndexOr { plans }) =
            scalar_range_plan(&plan, txn.as_ref(), "docs").unwrap()
        else {
            panic!("a scalar field must be narrowed");
        };
        // Int [10, 20), and the Float block up to just above 20: Float keys sort
        // above every Int key, so a Float such as 19.5 lives there. Neither
        // window is open-ended, unlike the two halves the planner emits.
        assert_eq!(plans.len(), 2, "{plans:?}");
        assert!(plans.iter().all(bounded), "{plans:?}");
    }

    #[test]
    fn mixed_numeric_bounds_keep_both_type_blocks() {
        let (backend, _col) = collection(vec![Value::Int(1), Value::Float(2.5)]);
        let txn = backend.begin_read().unwrap();
        let plan = two_sided(Value::Float(0.5), Value::Int(10));
        let Some(QueryPlan::IndexOr { plans }) =
            scalar_range_plan(&plan, txn.as_ref(), "docs").unwrap()
        else {
            panic!("a scalar field must be narrowed");
        };
        // [0.5, ∞) ∪ Int[0, MAX] intersected with Int(-∞, 10) ∪ Float(-∞, 10⁺]:
        // an Int window and a Float window.
        assert_eq!(plans.len(), 2, "{plans:?}");
    }

    #[test]
    fn contradictory_bounds_find_nothing() {
        let (backend, col) = collection((0..10).map(Value::Int).collect());
        let txn = backend.begin_read().unwrap();
        let plan = two_sided(Value::Int(8), Value::Int(3));
        // The Int windows cancel out. An Int lower bound cannot exclude the
        // Float block by key, so a bounded Float window remains for the
        // post-filter, which finds nothing.
        let Some(QueryPlan::IndexOr { plans }) =
            scalar_range_plan(&plan, txn.as_ref(), "docs").unwrap()
        else {
            panic!("a scalar field must be narrowed");
        };
        assert!(plans.iter().all(bounded), "{plans:?}");
        let filter = Filter::And(vec![
            Filter::Gte("n".into(), Value::Int(8)),
            Filter::Lt("n".into(), Value::Int(3)),
        ]);
        assert!(col.find(filter).unwrap().is_empty());
    }

    #[test]
    fn array_fields_keep_the_id_intersection() {
        let (backend, _col) = collection(vec![
            Value::Int(1),
            Value::Array(vec![Value::Int(1), Value::Int(20)]),
        ]);
        let txn = backend.begin_read().unwrap();
        let plan = two_sided(Value::Int(5), Value::Int(10));
        assert!(
            scalar_range_plan(&plan, txn.as_ref(), "docs")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn other_plan_shapes_are_left_alone() {
        let (backend, _col) = collection((0..10).map(Value::Int).collect());
        let txn = backend.begin_read().unwrap();
        let index = |field: &str| IndexDef {
            collection: "docs".into(),
            field: field.into(),
        };
        let equalities = plan_full(
            &Filter::And(vec![
                Filter::Eq("n".into(), Value::Int(1)),
                Filter::Eq("m".into(), Value::Int(2)),
            ]),
            &[index("n"), index("m")],
            &[],
            &[],
        );
        assert!(matches!(equalities, QueryPlan::IndexAnd { .. }));
        assert!(
            scalar_range_plan(&equalities, txn.as_ref(), "docs")
                .unwrap()
                .is_none()
        );
        let one_sided = plan_full(
            &Filter::Gte("n".into(), Value::Int(1)),
            &[index("n")],
            &[],
            &[],
        );
        assert!(
            scalar_range_plan(&one_sided, txn.as_ref(), "docs")
                .unwrap()
                .is_none()
        );
        let fields_differ = QueryPlan::IndexAnd {
            plans: vec![
                QueryPlan::IndexRange {
                    field: "n".into(),
                    start: Bound::Unbounded,
                    end: Bound::Unbounded,
                },
                QueryPlan::IndexRange {
                    field: "m".into(),
                    start: Bound::Unbounded,
                    end: Bound::Unbounded,
                },
            ],
        };
        assert!(
            scalar_range_plan(&fields_differ, txn.as_ref(), "docs")
                .unwrap()
                .is_none()
        );
    }
}
