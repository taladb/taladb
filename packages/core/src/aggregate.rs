//! Aggregation pipeline for TalaDB.
//!
//! A pipeline is a sequence of [`Stage`]s applied to a collection's documents in order.
//!
//! # Supported stages
//!
//! | Stage | Description |
//! |---|---|
//! | `$match` | Filter documents (uses query index when available) |
//! | `$group` | Group by a field and compute per-group accumulators |
//! | `$sort`  | Sort the result set |
//! | `$skip`  | Skip N documents |
//! | `$limit` | Keep the first N documents |
//! | `$project` | Keep only the listed fields |
//!
//! # Example
//!
//! ```ignore
//! use taladb_core::aggregate::{Accumulator, GroupKey, Pipeline, Stage};
//! use taladb_core::query::Filter;
//! use taladb_core::query::options::SortSpec;
//!
//! let result = col.aggregate(vec![
//!     Stage::Match(Filter::Eq("status".into(), Value::Str("active".into()))),
//!     Stage::Group {
//!         key: GroupKey::Field("department".into()),
//!         accumulators: vec![
//!             ("total_salary".into(), Accumulator::Sum("salary".into())),
//!             ("count".into(), Accumulator::Count),
//!             ("avg_salary".into(), Accumulator::Avg("salary".into())),
//!         ],
//!     },
//!     Stage::Sort(vec![SortSpec::desc("total_salary")]),
//!     Stage::Limit(10),
//! ])?;
//! ```

use std::collections::HashMap;

use crate::clamp_to_usize;
use crate::document::{Document, Value};
use crate::error::TalaDbError;
use crate::query::options::{SortSpec, partial_sort_documents, sort_documents};

// ---------------------------------------------------------------------------
// Pipeline types
// ---------------------------------------------------------------------------

/// Key to group documents by in a `$group` stage.
#[derive(Debug, Clone)]
pub enum GroupKey {
    /// Group by the value of a single field. Documents where the field is
    /// absent are grouped under `Value::Null`.
    Field(String),
    /// Treat all documents as a single group (like SQL `GROUP BY NULL`).
    Null,
}

/// A per-group accumulator expression.
#[derive(Debug, Clone)]
pub enum Accumulator {
    /// Sum the numeric values of `field` across all documents in the group.
    Sum(String),
    /// Compute the arithmetic mean of `field` across the group.
    Avg(String),
    /// Return the minimum value of `field` in the group.
    Min(String),
    /// Return the maximum value of `field` in the group.
    Max(String),
    /// Count of documents in the group.
    Count,
    /// Collect all values of `field` into an array (like SQL `array_agg`).
    Push(String),
    /// Collect unique values of `field` into an array.
    AddToSet(String),
    /// Return the first value of `field` encountered in the group.
    First(String),
    /// Return the last value of `field` encountered in the group.
    Last(String),
}

/// A single pipeline stage.
#[derive(Debug, Clone)]
pub enum Stage {
    /// Filter documents. Uses the collection's indexes when run as the first stage.
    Match(crate::query::Filter),
    /// Group documents and compute accumulators.
    Group {
        key: GroupKey,
        /// `(output_field_name, accumulator)` pairs.
        accumulators: Vec<(String, Accumulator)>,
    },
    /// Sort the current document set.
    Sort(Vec<SortSpec>),
    /// Skip the first N documents.
    Skip(u64),
    /// Keep only the first N documents.
    Limit(u64),
    /// Reshape each document by keeping or dropping fields.
    ///
    /// MongoDB semantics: a projection is either an **inclusion** (`{a: 1}` —
    /// keep only the listed fields) or an **exclusion** (`{a: 0}` — keep
    /// everything *except* the listed fields). The two cannot be mixed, with the
    /// one exception that `_id` may be excluded alongside an inclusion
    /// (`{a: 1, _id: 0}`).
    Project {
        fields: Vec<String>,
        /// `true` → keep only `fields`. `false` → drop `fields`, keep the rest.
        include: bool,
        /// Whether the `_id` key survives (only ever present on `$group` output).
        keep_id: bool,
    },
}

/// A pipeline is an ordered list of stages.
pub type Pipeline = Vec<Stage>;

// ---------------------------------------------------------------------------
// JSON pipeline parsing (MongoDB-style syntax)
// ---------------------------------------------------------------------------

use crate::query::Filter;
use serde_json::Value as Json;

/// Parse a MongoDB-style aggregation pipeline — a JSON array of single-key
/// stage objects — into a [`Pipeline`]. `parse_filter` translates a `$match`
/// body into a [`Filter`]; each binding passes its own JSON→Filter parser so
/// the `$match` syntax stays identical to `find()`.
///
/// Stages: `$match`, `$group`, `$sort`, `$skip`, `$limit`, `$project`.
/// Accumulators: `$sum` (`$sum: 1` counts), `$avg`, `$min`, `$max`, `$count`.
pub fn parse_pipeline(
    value: &Json,
    parse_filter: &dyn Fn(&Json) -> Result<Filter, String>,
) -> Result<Pipeline, String> {
    let arr = value
        .as_array()
        .ok_or("aggregation pipeline must be an array of stages")?;
    let mut stages = Vec::with_capacity(arr.len());
    for (i, raw) in arr.iter().enumerate() {
        let obj = raw
            .as_object()
            .ok_or_else(|| format!("pipeline[{i}] must be an object"))?;
        if obj.len() != 1 {
            return Err(format!(
                "pipeline[{i}] must have exactly one stage operator (e.g. {{ \"$group\": ... }})"
            ));
        }
        let (op, body) = obj.iter().next().unwrap();
        let stage = match op.as_str() {
            "$match" => {
                let filter = parse_filter(body)?;
                validate_regexes(&filter).map_err(|e| format!("pipeline[{i}] $match: {e}"))?;
                Stage::Match(filter)
            }
            "$group" => parse_group(body).map_err(|e| format!("pipeline[{i}] $group: {e}"))?,
            "$sort" => {
                Stage::Sort(parse_sort(body).map_err(|e| format!("pipeline[{i}] $sort: {e}"))?)
            }
            "$skip" => Stage::Skip(
                as_u64(body).ok_or_else(|| format!("pipeline[{i}] $skip must be a u64"))?,
            ),
            "$limit" => Stage::Limit(
                as_u64(body).ok_or_else(|| format!("pipeline[{i}] $limit must be a u64"))?,
            ),
            "$project" => {
                let (fields, include, keep_id) =
                    parse_project(body).map_err(|e| format!("pipeline[{i}] $project: {e}"))?;
                Stage::Project {
                    fields,
                    include,
                    keep_id,
                }
            }
            other => return Err(format!("pipeline[{i}]: unsupported stage '{other}'")),
        };
        stages.push(stage);
    }
    Ok(stages)
}

/// Reject a `$match` whose regexes will not compile, at pipeline-parse time.
///
/// Goes through `build_regex` — the same builder the executor uses — so
/// validation and execution agree on what is legal. They did not: this used a
/// bare `regex::Regex::new`, whose default size ceiling is 10 MiB against the
/// executor's 1 MiB, so a pattern between the two passed validation here and
/// then failed when the stage actually ran.
fn validate_regexes(filter: &Filter) -> Result<(), String> {
    match filter {
        Filter::Regex(_, pattern) => crate::query::filter::build_regex(pattern)
            .map(|_| ())
            .map_err(|e| format!("invalid regex: {e}")),
        Filter::And(filters) | Filter::Or(filters) => filters.iter().try_for_each(validate_regexes),
        Filter::Not(inner) => validate_regexes(inner),
        _ => Ok(()),
    }
}

fn as_u64(v: &Json) -> Option<u64> {
    v.as_u64()
}

/// `"$field"` → field name; a bare field name is also accepted.
fn field_ref(v: &Json) -> Option<String> {
    let s = v.as_str()?;
    Some(s.strip_prefix('$').unwrap_or(s).to_string())
}

fn parse_group(body: &Json) -> Result<Stage, String> {
    let obj = body.as_object().ok_or("$group must be an object")?;
    let id = obj.get("_id").ok_or("$group requires an _id")?;
    let key = match id {
        Json::Null => GroupKey::Null,
        Json::String(_) => GroupKey::Field(field_ref(id).ok_or("invalid _id field reference")?),
        _ => return Err("_id must be a \"$field\" reference or null".into()),
    };
    let mut accumulators = Vec::new();
    for (out, spec) in obj {
        if out == "_id" {
            continue;
        }
        accumulators.push((out.clone(), parse_accumulator(spec)?));
    }
    Ok(Stage::Group { key, accumulators })
}

fn parse_accumulator(spec: &Json) -> Result<Accumulator, String> {
    let obj = spec
        .as_object()
        .ok_or("accumulator must be an object like { \"$sum\": \"$field\" }")?;
    if obj.len() != 1 {
        return Err("accumulator must have exactly one operator".into());
    }
    let (op, arg) = obj.iter().next().unwrap();
    match op.as_str() {
        // `{ $sum: 1 }` counts; `{ $sum: "$field" }` sums the field.
        "$sum" => match arg {
            Json::Number(n) if n.as_i64() == Some(1) => Ok(Accumulator::Count),
            _ => Ok(Accumulator::Sum(
                field_ref(arg).ok_or("$sum expects \"$field\" or 1")?,
            )),
        },
        "$avg" => Ok(Accumulator::Avg(
            field_ref(arg).ok_or("$avg expects \"$field\"")?,
        )),
        "$min" => Ok(Accumulator::Min(
            field_ref(arg).ok_or("$min expects \"$field\"")?,
        )),
        "$max" => Ok(Accumulator::Max(
            field_ref(arg).ok_or("$max expects \"$field\"")?,
        )),
        "$count" => Ok(Accumulator::Count),
        "$push" => Ok(Accumulator::Push(
            field_ref(arg).ok_or("$push expects \"$field\"")?,
        )),
        "$addToSet" => Ok(Accumulator::AddToSet(
            field_ref(arg).ok_or("$addToSet expects \"$field\"")?,
        )),
        "$first" => Ok(Accumulator::First(
            field_ref(arg).ok_or("$first expects \"$field\"")?,
        )),
        "$last" => Ok(Accumulator::Last(
            field_ref(arg).ok_or("$last expects \"$field\"")?,
        )),
        other => Err(format!("unsupported accumulator '{other}'")),
    }
}

/// `{ field: 1 | -1 }`. Note: with multiple keys, ordering follows JSON object
/// key order; most sorts use a single key.
fn parse_sort(body: &Json) -> Result<Vec<SortSpec>, String> {
    let obj = body.as_object().ok_or("$sort must be an object")?;
    let mut specs = Vec::with_capacity(obj.len());
    for (field, dir) in obj {
        match dir.as_i64() {
            Some(1) => specs.push(SortSpec::asc(field.clone())),
            Some(-1) => specs.push(SortSpec::desc(field.clone())),
            _ => return Err(format!("$sort direction for '{field}' must be 1 or -1")),
        }
    }
    Ok(specs)
}

/// Parse a `$project` body into `(fields, include, keep_id)`.
///
/// Inclusion (`{a: 1}`) keeps only the listed fields; exclusion (`{a: 0}`) drops
/// them and keeps the rest. Mixing the two is rejected rather than silently
/// resolved — previously an all-zero body parsed to an empty *inclusion* list,
/// which returned documents stripped of every field but `_id` with no error.
/// `_id: 0` is the one exclusion allowed to accompany an inclusion.
///
/// Values are booleans or numbers, read for truthiness as MongoDB reads them:
/// zero excludes, every other number includes. Truthiness is evaluated on the
/// number itself, *not* on a cast to `i64` — a cast silently floors `0.5` to `0`
/// and flips it from inclusion to exclusion, which then trips the mixing check
/// in `{a: 0.5, b: 1}` and errors out on a body that names no exclusion at all.
fn parse_project(body: &Json) -> Result<(Vec<String>, bool, bool), String> {
    let obj = body.as_object().ok_or("$project must be an object")?;
    if obj.is_empty() {
        return Err("$project must name at least one field".into());
    }

    let mut included = Vec::new();
    let mut excluded = Vec::new();
    let mut keep_id = true;

    for (field, spec) in obj {
        let keep = match spec {
            Json::Bool(b) => *b,
            Json::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
            _ => {
                return Err(format!(
                    "$project value for '{field}' must be a number or boolean"
                ));
            }
        };
        if field == "_id" {
            keep_id = keep;
            // `_id` participates in neither list: it is controlled by `keep_id`
            // alone, so `{a: 1, _id: 0}` stays a pure inclusion.
            continue;
        }
        if keep {
            included.push(field.clone());
        } else {
            excluded.push(field.clone());
        }
    }

    match (included.is_empty(), excluded.is_empty()) {
        (false, false) => Err(format!(
            "$project cannot mix inclusion ({}) and exclusion ({}) — \
             use one or the other (`_id: 0` is the sole exception)",
            included.join(", "),
            excluded.join(", "),
        )),
        // Pure exclusion, e.g. `{description: 0}`.
        (true, false) => Ok((excluded, false, keep_id)),
        // Pure inclusion, e.g. `{name: 1}`.
        (false, true) => Ok((included, true, keep_id)),
        // Only `_id` was named. `{_id: 0}` = drop `_id`, keep everything else;
        // `{_id: 1}` = keep only `_id`.
        (true, true) => Ok((Vec::new(), keep_id, keep_id)),
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// How many of a sorted stream's leading documents can actually be reached by
/// the `$skip`/`$limit` stages that immediately follow a `$sort`.
///
/// Tracks the surviving window `[start, end)` over the sorted list. `$skip(s)`
/// shifts the window forward; `$limit(l)` caps it. `end` is the answer: nothing
/// past it can ever be observed, so the sort never needs to order it.
///
/// Returns `None` when the reachable set is unbounded (no `$limit`), or when a
/// stage that changes the document set (`$match`, `$group`) intervenes — in
/// which case we must sort everything, as before.
pub(crate) fn reachable_after_sort(rest: &[Stage]) -> Option<usize> {
    let mut start: usize = 0;
    let mut end: Option<usize> = None; // None = unbounded

    for stage in rest {
        match stage {
            Stage::Skip(n) => {
                let n = clamp_to_usize(*n);
                start = start.saturating_add(n);
                if let Some(e) = end {
                    end = Some(e.saturating_add(n));
                }
            }
            Stage::Limit(n) => {
                let cap = start.saturating_add(clamp_to_usize(*n));
                end = Some(end.map_or(cap, |e| e.min(cap)));
            }
            // `$project` reshapes documents but preserves both their count and
            // their order, so it cannot bring more of the sorted list into view.
            Stage::Project { .. } => continue,
            // Anything else changes the document set; stop reasoning here.
            _ => break,
        }
    }
    end
}

/// Execute a pipeline starting from `input` documents.
///
/// `input` is the full collection scan (or index-filtered results from the
/// first `$match` stage — handled by `Collection::aggregate`).
///
/// A `$sort` followed by a bounded `$skip`/`$limit` run is executed as a
/// **bounded** sort: only the `skip + limit` documents that can actually be
/// returned are ordered (O(n + k log k)), instead of ordering the whole matched
/// set and then throwing nearly all of it away (O(n log n)). Paging a 10k
/// catalog for 24 rows no longer pays to sort 10k.
pub fn execute_pipeline(
    mut docs: Vec<Document>,
    pipeline: &[Stage],
) -> Result<Vec<Document>, TalaDbError> {
    // Compile every regex in every `$match` stage once, up front — the same
    // thing the query executor does via `Matcher::new`.
    //
    // Going through `Filter::matches` instead rebuilt the automaton for every
    // document, and swallowed compile failures into `unwrap_or(false)`. That
    // second half is the real bug: an invalid or over-size-limit pattern
    // silently matched nothing here while the identical filter through `find()`
    // returned `InvalidFilter`. Same query, two answers, one of them a silent
    // empty result.
    let mut regex_cache = HashMap::new();
    for stage in pipeline {
        if let Stage::Match(filter) = stage {
            regex_cache.extend(filter.compile_regex_cache()?);
        }
    }

    let mut i = 0;
    while i < pipeline.len() {
        if let Stage::Sort(specs) = &pipeline[i]
            && let Some(keep) = reachable_after_sort(&pipeline[i + 1..])
        {
            // The trailing $skip/$limit stages still run below; this only
            // discards the tail that provably cannot be reached.
            partial_sort_documents(&mut docs, specs, keep);
            i += 1;
            continue;
        }
        docs = apply_stage(docs, &pipeline[i], &regex_cache)?;
        i += 1;
    }
    Ok(docs)
}

fn apply_stage(
    docs: Vec<Document>,
    stage: &Stage,
    regex_cache: &HashMap<String, regex::Regex>,
) -> Result<Vec<Document>, TalaDbError> {
    match stage {
        // `try_fold` rather than `filter`: the predicate can now fail, and a
        // closure passed to `filter` has nowhere to send an error.
        Stage::Match(filter) => docs.into_iter().try_fold(Vec::new(), |mut kept, d| {
            if filter.matches_with_cache(&d, regex_cache)? {
                kept.push(d);
            }
            Ok(kept)
        }),

        Stage::Group { key, accumulators } => apply_group(docs, key, accumulators),

        Stage::Sort(specs) => {
            let mut out = docs;
            sort_documents(&mut out, specs);
            Ok(out)
        }

        Stage::Skip(n) => {
            let n = clamp_to_usize(*n);
            if n >= docs.len() {
                Ok(vec![])
            } else {
                Ok(docs.into_iter().skip(n).collect())
            }
        }

        Stage::Limit(n) => Ok(docs.into_iter().take(clamp_to_usize(*n)).collect()),

        Stage::Project {
            fields,
            include,
            keep_id,
        } => Ok(docs
            .into_iter()
            .map(|mut d| {
                d.fields.retain(|(k, _)| {
                    if k == "_id" {
                        // `_id` is governed solely by `keep_id`, in both modes.
                        return *keep_id;
                    }
                    let named = fields.iter().any(|f| f == k);
                    // Inclusion keeps what is named; exclusion keeps what is not.
                    if *include { named } else { !named }
                });
                d
            })
            .collect()),
    }
}

// ---------------------------------------------------------------------------
// $group implementation
// ---------------------------------------------------------------------------

/// Mutable per-group accumulator state.
enum AccState {
    Sum { int: i128, float: Option<f64> },
    AvgState { sum: f64, count: u64 },
    Min(Option<Value>),
    Max(Option<Value>),
    Count(u64),
    Push(Vec<Value>),
    AddToSet(Vec<Value>),
    First(Option<Value>),
    Last(Option<Value>),
}

fn make_state(acc: &Accumulator) -> AccState {
    match acc {
        Accumulator::Sum(_) => AccState::Sum {
            int: 0,
            float: None,
        },
        Accumulator::Avg(_) => AccState::AvgState { sum: 0.0, count: 0 },
        Accumulator::Min(_) => AccState::Min(None),
        Accumulator::Max(_) => AccState::Max(None),
        Accumulator::Count => AccState::Count(0),
        Accumulator::Push(_) => AccState::Push(vec![]),
        Accumulator::AddToSet(_) => AccState::AddToSet(vec![]),
        Accumulator::First(_) => AccState::First(None),
        Accumulator::Last(_) => AccState::Last(None),
    }
}

fn update_state(state: &mut AccState, acc: &Accumulator, doc: &Document) {
    match (state, acc) {
        (AccState::Sum { int, float }, Accumulator::Sum(f)) => {
            if let Some(v) = doc.get(f) {
                match v {
                    Value::Int(n) if float.is_none() => *int += i128::from(*n),
                    Value::Int(n) => *float = Some(float.unwrap() + *n as f64),
                    Value::Float(n) => *float = Some(float.unwrap_or(*int as f64) + n),
                    _ => {}
                }
            }
        }
        (AccState::AvgState { sum, count }, Accumulator::Avg(f)) => {
            if let Some(v) = doc.get(f)
                && let Some(n) = numeric_to_f64(v)
            {
                *sum += n;
                *count += 1;
            }
        }
        (AccState::Min(cur), Accumulator::Min(f)) => {
            if let Some(v) = doc.get(f) {
                *cur = Some(match cur.take() {
                    None => v.clone(),
                    Some(existing) => {
                        if value_lt(v, &existing) {
                            v.clone()
                        } else {
                            existing
                        }
                    }
                });
            }
        }
        (AccState::Max(cur), Accumulator::Max(f)) => {
            if let Some(v) = doc.get(f) {
                *cur = Some(match cur.take() {
                    None => v.clone(),
                    Some(existing) => {
                        if value_lt(&existing, v) {
                            v.clone()
                        } else {
                            existing
                        }
                    }
                });
            }
        }
        (AccState::Count(n), Accumulator::Count) => *n += 1,
        (AccState::Push(arr), Accumulator::Push(f)) => {
            if let Some(v) = doc.get(f) {
                arr.push(v.clone());
            }
        }
        (AccState::AddToSet(arr), Accumulator::AddToSet(f)) => {
            if let Some(v) = doc.get(f)
                && !arr.contains(v)
            {
                arr.push(v.clone());
            }
        }
        (AccState::First(cur), Accumulator::First(f)) if cur.is_none() => {
            *cur = doc.get(f).cloned();
        }
        (AccState::Last(cur), Accumulator::Last(f)) => {
            *cur = doc.get(f).cloned();
        }
        _ => {}
    }
}

fn finalize_state(state: AccState) -> Value {
    match state {
        AccState::Sum { int, float } => match float {
            Some(n) => float_or_int(n),
            None if int <= i128::from(i64::MAX) && int >= i128::from(i64::MIN) => {
                Value::Int(int as i64)
            }
            None => Value::Float(int as f64),
        },
        AccState::AvgState { sum, count } => {
            if count == 0 {
                Value::Null
            } else {
                Value::Float(sum / count as f64)
            }
        }
        AccState::Min(v) | AccState::Max(v) | AccState::First(v) | AccState::Last(v) => {
            v.unwrap_or(Value::Null)
        }
        AccState::Count(n) => Value::Int(n as i64),
        AccState::Push(arr) | AccState::AddToSet(arr) => Value::Array(arr),
    }
}

fn apply_group(
    docs: Vec<Document>,
    key: &GroupKey,
    accumulators: &[(String, Accumulator)],
) -> Result<Vec<Document>, TalaDbError> {
    // Map from group-key string → (group_key_value, Vec<AccState>)
    let mut groups: HashMap<String, (Value, Vec<AccState>)> = HashMap::new();

    for doc in &docs {
        let group_val = match key {
            GroupKey::Field(f) => doc.get(f).cloned().unwrap_or(Value::Null),
            GroupKey::Null => Value::Null,
        };
        let group_str = value_to_key_string(&group_val);

        let entry = groups.entry(group_str).or_insert_with(|| {
            let states = accumulators.iter().map(|(_, a)| make_state(a)).collect();
            (group_val.clone(), states)
        });

        for (i, (_, acc)) in accumulators.iter().enumerate() {
            update_state(&mut entry.1[i], acc, doc);
        }
    }

    // Build output documents: _id = group key, + one field per accumulator
    let mut out: Vec<Document> = groups
        .into_values()
        .map(|(group_val, states)| {
            let mut fields: Vec<(String, Value)> = vec![("_id".into(), group_val)];
            for ((name, _), state) in accumulators.iter().zip(states) {
                fields.push((name.clone(), finalize_state(state)));
            }
            // Use a nil ULID for group result rows — they are not real documents
            Document::with_id(ulid::Ulid::nil(), fields)
        })
        .collect();

    out.sort_by_key(|d| value_to_key_string(d.get("_id").unwrap_or(&Value::Null)));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn numeric_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn float_or_int(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < i64::MAX as f64 {
        Value::Int(f as i64)
    } else {
        Value::Float(f)
    }
}

fn value_lt(a: &Value, b: &Value) -> bool {
    crate::query::options::cmp_values(a, b) == std::cmp::Ordering::Less
}

/// Produce a stable string key for HashMap grouping.
fn value_to_key_string(v: &Value) -> String {
    match v {
        Value::Null => "\x00null".into(),
        Value::Bool(b) => format!("\x01{b}"),
        Value::Int(n) => format!("\x02{:020}", i128::from(*n) - i128::from(i64::MIN)),
        Value::Float(f) => format!("\x03{f:e}"),
        Value::Str(s) => format!("\x04{s}"),
        Value::Bytes(b) => format!("\x05{b:?}"),
        Value::Array(_) | Value::Object(_) => format!("\x06{v:?}"),
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;
    use crate::query::Filter;

    // Trivial filter parser: {"status":"active"} → Eq. Enough to prove $match
    // routes through the closure; real filter parsing is the bindings' job.
    fn tiny_filter(v: &Json) -> Result<Filter, String> {
        let obj = v.as_object().ok_or("bad filter")?;
        let (k, val) = obj.iter().next().ok_or("empty filter")?;
        Ok(Filter::Eq(
            k.clone(),
            Value::Str(val.as_str().unwrap_or("").to_string()),
        ))
    }

    fn parse(json: &str) -> Result<Pipeline, String> {
        parse_pipeline(&serde_json::from_str(json).unwrap(), &tiny_filter)
    }

    #[test]
    fn rejects_non_one_numeric_sum() {
        assert!(parse(r#"[{"$group":{"_id":null,"n":{"$sum":2}}}]"#).is_err());
    }

    #[test]
    fn negative_integer_group_key_is_safe() {
        assert!(!value_to_key_string(&Value::Int(-1)).is_empty());
        assert!(!value_to_key_string(&Value::Int(i64::MIN)).is_empty());
    }

    #[test]
    fn project_can_explicitly_exclude_group_id() {
        let pl = parse(r#"[{"$project":{"_id":0,"total":1}}]"#).unwrap();
        assert!(
            matches!(&pl[0], Stage::Project { fields, include: true, keep_id: false } if fields == &vec!["total".to_string()])
        );
    }

    #[test]
    fn sort_preserves_caller_key_priority() {
        let pl = parse(r#"[{"$sort":{"z":1,"a":-1}}]"#).unwrap();
        assert!(matches!(&pl[0], Stage::Sort(s) if s[0].field == "z" && s[1].field == "a"));
    }

    #[test]
    fn parses_full_group_pipeline() {
        let pl = parse(
            r#"[
                {"$match": {"status": "active"}},
                {"$group": {"_id": "$dept", "total": {"$sum": "$salary"}, "n": {"$sum": 1}, "avg": {"$avg": "$salary"}}},
                {"$sort": {"total": -1}},
                {"$limit": 10}
            ]"#,
        )
        .unwrap();
        assert_eq!(pl.len(), 4);
        assert!(matches!(pl[0], Stage::Match(_)));
        match &pl[1] {
            Stage::Group { key, accumulators } => {
                assert!(matches!(key, GroupKey::Field(f) if f == "dept"));
                assert_eq!(accumulators.len(), 3);
                // Look up by output name — JSON object key order isn't guaranteed.
                let acc = |name: &str| accumulators.iter().find(|(n, _)| n == name).map(|(_, a)| a);
                assert!(matches!(acc("total"), Some(Accumulator::Sum(f)) if f == "salary"));
                assert!(matches!(acc("n"), Some(Accumulator::Count))); // $sum: 1
                assert!(matches!(acc("avg"), Some(Accumulator::Avg(f)) if f == "salary"));
            }
            _ => panic!("expected group"),
        }
        assert!(
            matches!(&pl[2], Stage::Sort(s) if s[0].direction == crate::query::options::SortDirection::Desc)
        );
        assert!(matches!(pl[3], Stage::Limit(10)));
    }

    #[test]
    fn group_by_null_and_count_and_project() {
        let pl = parse(
            r#"[{"$group": {"_id": null, "total": {"$count": {}}}}, {"$project": {"total": 1}}]"#,
        )
        .unwrap();
        assert!(matches!(
            &pl[0],
            Stage::Group {
                key: GroupKey::Null,
                ..
            }
        ));
        assert!(
            matches!(&pl[1], Stage::Project { fields, include: true, keep_id: true } if fields == &vec!["total".to_string()])
        );
    }

    #[test]
    fn rejects_bad_shapes() {
        assert!(parse(r#"{"$match": {}}"#).is_err()); // not an array
        assert!(parse(r#"[{"$match": {}, "$limit": 1}]"#).is_err()); // two ops in one stage
        assert!(parse(r#"[{"$frobnicate": 1}]"#).is_err()); // unknown stage
        assert!(parse(r#"[{"$group": {"total": {"$sum": "$x"}}}]"#).is_err()); // no _id
        assert!(parse(r#"[{"$sort": {"f": 2}}]"#).is_err()); // bad direction
    }
}
