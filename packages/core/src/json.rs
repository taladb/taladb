//! TalaDB's JSON query language: documents, filters and updates as JSON.
//!
//! This is the same language every binding speaks — `{"age": {"$gte": 18}}`,
//! `{"$set": {"done": true}}` — so a filter written for the browser, Node.js,
//! React Native, Kotlin or Swift means the same thing here. The bindings call
//! into this module rather than keeping parsers of their own.
//!
//! ```
//! use serde_json::json;
//! use taladb::json::{filter_from_json, update_from_json};
//!
//! let filter = filter_from_json(&json!({ "age": { "$gte": 18 }, "role": "admin" })).unwrap();
//! let update = update_from_json(&json!({ "$set": { "active": true } })).unwrap();
//! # let _ = (filter, update);
//! ```

use serde_json::Value as Json;

use crate::collection::Update;
use crate::document::{Document, Value};
use crate::error::TalaDbError;
use crate::json_depth::check_json_depth;
use crate::query::Filter;

/// Convert a JSON value to a TalaDB [`Value`]. Integers that fit in `i64`
/// become [`Value::Int`]; every other number becomes [`Value::Float`].
pub fn to_value(j: &Json) -> Value {
    match j {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        Json::String(s) => Value::Str(s.clone()),
        Json::Array(arr) => Value::Array(arr.iter().map(to_value).collect()),
        Json::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), to_value(v))).collect())
        }
    }
}

/// Convert a TalaDB [`Value`] to JSON. Non-finite floats become `null`, and
/// [`Value::Bytes`] — which JSON cannot carry — becomes a `"<bytes:N>"` marker.
pub fn from_value(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Int(n) => Json::Number((*n).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Value::Str(s) => Json::String(s.clone()),
        Value::Bytes(b) => Json::String(format!("<bytes:{}>", b.len())),
        Value::Array(arr) => Json::Array(arr.iter().map(from_value).collect()),
        Value::Object(obj) => Json::Object(
            obj.iter()
                .map(|(k, v)| (k.clone(), from_value(v)))
                .collect(),
        ),
    }
}

/// A stored document as a JSON object, with its id under `"_id"`.
pub fn document_to_json(doc: &Document) -> Json {
    let mut map = serde_json::Map::new();
    map.insert("_id".to_string(), Json::String(doc.id.to_string()));
    for (k, v) in &doc.fields {
        map.insert(k.clone(), from_value(v));
    }
    Json::Object(map)
}

/// The fields of a JSON object, ready for [`crate::Collection::insert`].
///
/// An `"_id"` key is kept: the engine validates it as a ULID and refuses to
/// overwrite an existing document.
pub fn fields_from_json(v: &Json) -> Result<Vec<(String, Value)>, TalaDbError> {
    // `serde_json` already caps its parser at 128 levels, so this is about
    // agreement rather than safety: the engine's ceiling is 64, and the same
    // document should be accepted or rejected identically whichever binding it
    // arrives through.
    check_json_depth(v)?;
    let obj = v
        .as_object()
        .ok_or_else(|| TalaDbError::Serialization("a document must be a JSON object".into()))?;
    Ok(obj.iter().map(|(k, v)| (k.clone(), to_value(v))).collect())
}

/// Parse a filter.
///
/// `null` and `{}` match every document. Anything unparseable is an
/// **error**, never a silent match-all: degrading a malformed filter to
/// [`Filter::All`] would make a typo'd operator in a `delete_many` or
/// `update_many` hit every document in the collection.
pub fn filter_from_json(v: &Json) -> Result<Filter, TalaDbError> {
    check_json_depth(v)?;
    if v.is_null() || v.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(Filter::All);
    }
    filter_from_json_object(v).ok_or_else(|| TalaDbError::InvalidFilter(v.to_string()))
}

/// The filter grammar itself, without [`filter_from_json`]'s top-level rules:
/// `null` is rejected and nesting depth is not checked. For bindings that
/// parse filters nested inside other requests (aggregation `$match`, vector
/// commands), where those rules are applied to the enclosing request instead.
#[doc(hidden)]
pub fn filter_from_json_object(v: &Json) -> Option<Filter> {
    let obj = v.as_object()?;
    let mut filters: Vec<Filter> = Vec::new();
    for (field, expr) in obj {
        if field.starts_with('$') {
            let logical = match field.as_str() {
                "$and" => Filter::And(
                    expr.as_array()?
                        .iter()
                        .map(filter_from_json_object)
                        .collect::<Option<_>>()?,
                ),
                "$or" => Filter::Or(
                    expr.as_array()?
                        .iter()
                        .map(filter_from_json_object)
                        .collect::<Option<_>>()?,
                ),
                "$not" => Filter::Not(Box::new(filter_from_json_object(expr)?)),
                _ => return None,
            };
            filters.push(logical);
            continue;
        }
        if !expr.is_object() {
            filters.push(Filter::Eq(field.clone(), to_value(expr)));
            continue;
        }
        let ops = expr.as_object()?;
        if ops.is_empty() {
            // `{field: {}}` is ambiguous (error on node, match-all historically
            // on web/RN) — rejected everywhere as of 0.8.1.
            return None;
        }
        for (op, val) in ops {
            let v = to_value(val);
            let f = match op.as_str() {
                "$eq" => Filter::Eq(field.clone(), v),
                "$ne" => Filter::Ne(field.clone(), v),
                "$gt" => Filter::Gt(field.clone(), v),
                "$gte" => Filter::Gte(field.clone(), v),
                "$lt" => Filter::Lt(field.clone(), v),
                "$lte" => Filter::Lte(field.clone(), v),
                "$exists" => Filter::Exists(field.clone(), val.as_bool()?),
                "$in" => Filter::In(
                    field.clone(),
                    val.as_array()?.iter().map(to_value).collect(),
                ),
                "$nin" => Filter::Nin(
                    field.clone(),
                    val.as_array()?.iter().map(to_value).collect(),
                ),
                "$contains" => Filter::Contains(field.clone(), val.as_str()?.to_string()),
                "$regex" => Filter::Regex(field.clone(), val.as_str()?.to_string()),
                _ => return None,
            };
            filters.push(f);
        }
    }
    match filters.len() {
        0 => Some(Filter::All),
        1 => Some(filters.remove(0)),
        _ => Some(Filter::And(filters)),
    }
}

/// Parse an update: any combination of `$set`, `$unset`, `$inc`, `$push` and
/// `$pull`, applied in that order. An empty update or an unknown operator is
/// an error.
pub fn update_from_json(v: &Json) -> Result<Update, TalaDbError> {
    check_json_depth(v)?;
    update_from_json_object(v)
        .ok_or_else(|| TalaDbError::InvalidOperation(format!("invalid update: {v}")))
}

fn update_from_json_object(v: &Json) -> Option<Update> {
    let obj = v.as_object()?;
    let mut updates = Vec::new();
    if let Some(set) = obj.get("$set") {
        let pairs = set
            .as_object()?
            .iter()
            .map(|(k, v)| (k.clone(), to_value(v)))
            .collect();
        updates.push(Update::Set(pairs));
    }
    if let Some(unset) = obj.get("$unset") {
        let keys = unset.as_object()?.keys().cloned().collect();
        updates.push(Update::Unset(keys));
    }
    if let Some(inc) = obj.get("$inc") {
        let pairs = inc
            .as_object()?
            .iter()
            .map(|(k, v)| (k.clone(), to_value(v)))
            .collect();
        updates.push(Update::Inc(pairs));
    }
    if let Some(push) = obj.get("$push") {
        updates.extend(
            push.as_object()?
                .iter()
                .map(|(k, v)| Update::Push(k.clone(), to_value(v))),
        );
    }
    if let Some(pull) = obj.get("$pull") {
        updates.extend(
            pull.as_object()?
                .iter()
                .map(|(k, v)| Update::Pull(k.clone(), to_value(v))),
        );
    }
    if obj
        .keys()
        .any(|k| !matches!(k.as_str(), "$set" | "$unset" | "$inc" | "$push" | "$pull"))
    {
        return None;
    }
    match updates.len() {
        0 => None,
        1 => Some(updates.remove(0)),
        _ => Some(Update::Many(updates)),
    }
}
