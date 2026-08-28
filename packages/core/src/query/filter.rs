use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::document::{Document, Value};
use crate::error::TalaDbError;

/// MongoDB-inspired filter AST.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Filter {
    /// Matches all documents.
    All,
    /// field == value
    Eq(String, Value),
    /// field != value
    Ne(String, Value),
    /// field > value
    Gt(String, Value),
    /// field >= value
    Gte(String, Value),
    /// field < value
    Lt(String, Value),
    /// field <= value
    Lte(String, Value),
    /// field in [values]
    In(String, Vec<Value>),
    /// field not in [values]
    Nin(String, Vec<Value>),
    /// field exists / does not exist
    Exists(String, bool),
    /// $and
    And(Vec<Self>),
    /// $or
    Or(Vec<Self>),
    /// $not
    Not(Box<Self>),
    /// Full-text search: field contains all tokens from the query string.
    /// Requires a full-text index created with `Collection::create_fts_index`.
    Contains(String, String),
    /// Regex match on a string field. Pattern is compiled on each call; consider
    /// `Filter::Contains` for repeated token searches.
    /// Example: `Filter::Regex("email".into(), r"@example\.com$".into())`
    Regex(String, String),
}

/// Apply `pred` to a field value, and to each element when it is an array.
///
/// This is what gives array fields query semantics. `{ tags: ['rust', 'db'] }`
/// matches `{ tags: 'rust' }` because one element does, exactly as it does in
/// MongoDB — and the whole-array comparison is still tried first, so an exact
/// `{ tags: ['rust', 'db'] }` filter keeps working.
///
/// Documents could previously *hold* arrays (`$push` and `$pull` maintain them)
/// while no query could reach into one: the comparison was against the array as
/// a single value, so a field holding a list matched nothing but an identical
/// list. The failure was silent — an empty result set, never an error.
///
/// Nested arrays are not flattened. One level is what the write path indexes
/// (`encode_index_keys`), and matching deeper than the index reaches would make
/// the same query answer differently depending on whether an index existed.
fn any_element<F: Fn(&Value) -> bool>(value: &Value, pred: F) -> bool {
    if pred(value) {
        return true;
    }
    match value {
        Value::Array(items) => items.iter().any(pred),
        _ => false,
    }
}

/// Ceiling on a compiled user-supplied pattern, applied to both the NFA and the
/// lazy DFA cache.
///
/// A pattern arrives from the caller, so its *compiled* size is caller-controlled
/// too: `(?:[a-z0-9]{64}){64}` is a few dozen bytes of source and megabytes of
/// automaton. The `regex` crate has no catastrophic backtracking, so this is not
/// about exponential matching — it is about memory.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// Compile a user-supplied pattern under [`REGEX_SIZE_LIMIT`].
///
/// Every regex in the engine goes through here so the limits cannot drift
/// between the place a pattern is validated and the place it is executed. They
/// had drifted: `aggregate::validate_regexes` used a bare `Regex::new`, whose
/// default ceiling is 10 MiB, so a pattern between 1 and 10 MiB passed `$match`
/// validation and then failed — or silently matched nothing — at execution.
pub(crate) fn build_regex(pattern: &str) -> Result<regex::Regex, TalaDbError> {
    regex::RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| TalaDbError::InvalidFilter(format!("regex: {e}")))
}

/// How a [`Filter`] evaluation resolves a `Regex` pattern to a compiled automaton.
enum RegexSource<'a> {
    /// Compile on demand. Correct anywhere, but recompiles per document, so it
    /// belongs to one-off checks rather than a scan.
    Compile,
    /// Look the pattern up in a cache built by [`Filter::compile_regex_cache`].
    Cached(&'a HashMap<String, regex::Regex>),
}

impl Filter {
    /// Evaluate the arms that cannot fail — every predicate that does not involve
    /// a regex or a sub-filter. The composite and `Regex` arms are handled by
    /// [`Filter::eval`] before it delegates here, so they are unreachable; each
    /// returns `false` rather than panicking, since a panic in a query path is a
    /// denial of service and a wrong `false` here is already impossible.
    fn matches_leaf(&self, doc: &Document) -> bool {
        match self {
            Self::All => true,

            Self::Eq(field, val) => {
                if field == "_id" {
                    return matches!(val, Value::Str(s) if s == &doc.id.to_string());
                }
                doc.get(field).is_some_and(|v| any_element(v, |e| e == val))
            }

            // Negation is over the *document*, not the element: `$ne` excludes a
            // document any of whose elements equals the value, which is what
            // makes `$ne` the complement of `$eq` on an array field too.
            Self::Ne(field, val) => {
                if field == "_id" {
                    return !matches!(val, Value::Str(s) if s == &doc.id.to_string());
                }
                doc.get(field).is_none_or(|v| !any_element(v, |e| e == val))
            }

            Self::Gt(field, val) => doc.get(field).is_some_and(|v| {
                any_element(v, |e| {
                    e.partial_cmp_numeric(val) == Some(std::cmp::Ordering::Greater)
                })
            }),

            Self::Gte(field, val) => doc.get(field).is_some_and(|v| {
                any_element(v, |e| {
                    matches!(
                        e.partial_cmp_numeric(val),
                        Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                    )
                })
            }),

            Self::Lt(field, val) => doc.get(field).is_some_and(|v| {
                any_element(v, |e| {
                    e.partial_cmp_numeric(val) == Some(std::cmp::Ordering::Less)
                })
            }),

            Self::Lte(field, val) => doc.get(field).is_some_and(|v| {
                any_element(v, |e| {
                    matches!(
                        e.partial_cmp_numeric(val),
                        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                    )
                })
            }),

            Self::In(field, vals) => {
                if field == "_id" {
                    let id_str = doc.id.to_string();
                    return vals
                        .iter()
                        .any(|v| matches!(v, Value::Str(s) if *s == id_str));
                }
                doc.get(field)
                    .is_some_and(|v| any_element(v, |e| vals.contains(e)))
            }

            Self::Nin(field, vals) => {
                if field == "_id" {
                    let id_str = doc.id.to_string();
                    return !vals
                        .iter()
                        .any(|v| matches!(v, Value::Str(s) if *s == id_str));
                }
                doc.get(field)
                    .is_none_or(|v| !any_element(v, |e| vals.contains(e)))
            }

            Self::Exists(field, should_exist) => {
                if field == "_id" {
                    return *should_exist;
                }
                doc.contains_key(field) == *should_exist
            }

            // Handled by `eval` before it delegates here, so these are
            // unreachable. `false` rather than `unreachable!` because a panic in
            // a query path is a denial of service, and this match is exhaustive
            // with no catch-all — a new `Filter` variant fails to compile here
            // until someone gives it an arm.
            //
            // What that does *not* catch: `eval` ends in a `leaf =>` catch-all,
            // so a new *composite* variant would route here silently and get
            // whatever arm was written for it. A variant holding sub-filters
            // needs an arm in `eval` too, or its children never get evaluated.
            Self::And(_) | Self::Or(_) | Self::Not(_) | Self::Regex(..) => false,

            // Post-filter: check all tokens appear in the field value
            Self::Contains(field, query) => {
                use crate::fts::tokenize;
                let query_tokens = tokenize(query);
                if query_tokens.is_empty() {
                    return true;
                }
                if let Some(Value::Str(text)) = doc.get(field) {
                    let doc_tokens = tokenize(text);
                    query_tokens
                        .iter()
                        .all(|qt| doc_tokens.iter().any(|dt| dt == qt))
                } else {
                    false
                }
            }
        }
    }

    /// The single evaluator behind both public entry points.
    ///
    /// `matches` and `matches_with_cache` used to be two full implementations of
    /// this logic, and they had already drifted: on a pattern that exceeded the
    /// size limit one returned `false` and the other returned `false` for a
    /// different reason, and *neither* said so. Under `Not` a silent `false`
    /// inverts to `true` — "matched nothing" becomes "matched everything" — so an
    /// exclusion filter returned the whole collection. One implementation, one
    /// failure mode, propagated.
    fn eval(&self, doc: &Document, regexes: &RegexSource<'_>) -> Result<bool, TalaDbError> {
        match self {
            Self::And(filters) => {
                for f in filters {
                    if !f.eval(doc, regexes)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }

            Self::Or(filters) => {
                for f in filters {
                    if f.eval(doc, regexes)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }

            Self::Not(inner) => Ok(!inner.eval(doc, regexes)?),

            Self::Regex(field, pattern) => {
                // Resolve the pattern before looking at the document: a filter
                // whose regex cannot be honoured is an error about the *query*,
                // and must not depend on whether this particular document
                // happens to carry the field.
                let compiled;
                let re = match regexes {
                    RegexSource::Compile => {
                        compiled = build_regex(pattern)?;
                        &compiled
                    }
                    RegexSource::Cached(cache) => cache.get(pattern).ok_or_else(|| {
                        // `compile_regex_cache` collects from this same tree, so
                        // a miss means the cache belongs to a different filter.
                        TalaDbError::InvalidFilter(format!(
                            "internal: no compiled regex for pattern {pattern:?} —                              the cache was built from a different filter"
                        ))
                    })?,
                };
                let Some(value) = doc.get(field) else {
                    return Ok(false);
                };
                Ok(any_element(
                    value,
                    |e| matches!(e, Value::Str(t) if re.is_match(t)),
                ))
            }

            leaf => Ok(leaf.matches_leaf(doc)),
        }
    }

    /// Evaluate this filter against a document, compiling any regex on the spot.
    ///
    /// # Errors
    ///
    /// Returns [`TalaDbError::InvalidFilter`] if a `Regex` pattern is malformed or
    /// compiles to an automaton larger than 1 MiB.
    ///
    /// Prefer [`Filter::compile_regex_cache`] + [`Filter::matches_with_cache`]
    /// when testing more than one document: this recompiles every pattern per
    /// call, which on a scan rebuilds the same automaton once per document.
    pub fn matches(&self, doc: &Document) -> Result<bool, TalaDbError> {
        self.eval(doc, &RegexSource::Compile)
    }

    /// Recursively collect all unique regex patterns in this filter tree.
    ///
    /// Used by the query executor to pre-compile regexes once per query rather
    /// than once per document.
    pub fn collect_regex_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();
        self.collect_regex_patterns_into(&mut patterns);
        patterns.sort();
        patterns.dedup();
        patterns
    }

    fn collect_regex_patterns_into(&self, out: &mut Vec<String>) {
        match self {
            Self::Regex(_, pattern) => out.push(pattern.clone()),
            Self::And(filters) | Self::Or(filters) => {
                for f in filters {
                    f.collect_regex_patterns_into(out);
                }
            }
            Self::Not(inner) => inner.collect_regex_patterns_into(out),
            _ => {}
        }
    }

    /// Pre-compile every regex pattern in this filter tree under
    /// [`REGEX_SIZE_LIMIT`]. Pass the result to [`Self::matches_with_cache`].
    ///
    /// This is the validating step: a malformed or oversized pattern fails here,
    /// once per query, instead of per document — which is why every query path
    /// calls it before scanning.
    ///
    /// # Errors
    ///
    /// Returns [`TalaDbError::InvalidFilter`] for a pattern that will not compile.
    pub(crate) fn compile_regex_cache(&self) -> Result<HashMap<String, regex::Regex>, TalaDbError> {
        let patterns = self.collect_regex_patterns();
        let mut map = HashMap::with_capacity(patterns.len());
        for pat in patterns {
            let re = build_regex(&pat)?;
            map.insert(pat, re);
        }
        Ok(map)
    }

    /// Evaluate this filter using regexes pre-compiled by
    /// [`Self::compile_regex_cache`].
    ///
    /// # Errors
    ///
    /// Returns [`TalaDbError::InvalidFilter`] if `regex_cache` is missing a
    /// pattern this filter uses, which means it was built from a different
    /// filter. Build it from the same tree and this cannot happen.
    pub fn matches_with_cache(
        &self,
        doc: &Document,
        regex_cache: &HashMap<String, regex::Regex>,
    ) -> Result<bool, TalaDbError> {
        self.eval(doc, &RegexSource::Cached(regex_cache))
    }

    /// Return the single indexed field this filter constrains, if any.
    /// Used by the query planner to select an index.
    pub fn primary_field(&self) -> Option<&str> {
        match self {
            Self::Eq(f, _)
            | Self::Ne(f, _)
            | Self::Gt(f, _)
            | Self::Gte(f, _)
            | Self::Lt(f, _)
            | Self::Lte(f, _)
            | Self::In(f, _)
            | Self::Nin(f, _)
            | Self::Exists(f, _) => Some(f.as_str()),
            Self::And(filters) => filters.iter().find_map(|f| f.primary_field()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ulid::Ulid;

    fn doc(fields: Vec<(&str, Value)>) -> Document {
        Document::with_id(
            Ulid::nil(),
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    #[test]
    fn basic_eq() {
        let d = doc(vec![("age", Value::Int(30))]);
        assert!(
            Filter::Eq("age".into(), Value::Int(30))
                .matches(&d)
                .expect("regex-free filter")
        );
        assert!(
            !Filter::Eq("age".into(), Value::Int(31))
                .matches(&d)
                .expect("regex-free filter")
        );
    }

    #[test]
    fn range_ops() {
        let d = doc(vec![("score", Value::Float(7.5))]);
        assert!(
            Filter::Gt("score".into(), Value::Float(7.0))
                .matches(&d)
                .expect("regex-free filter")
        );
        assert!(
            !Filter::Gt("score".into(), Value::Float(8.0))
                .matches(&d)
                .expect("regex-free filter")
        );
        assert!(
            Filter::Lte("score".into(), Value::Float(7.5))
                .matches(&d)
                .expect("regex-free filter")
        );
    }

    #[test]
    fn and_or() {
        let d = doc(vec![("a", Value::Int(1)), ("b", Value::Int(2))]);
        let f = Filter::And(vec![
            Filter::Eq("a".into(), Value::Int(1)),
            Filter::Eq("b".into(), Value::Int(2)),
        ]);
        assert!(f.matches(&d).expect("regex-free filter"));
        let f2 = Filter::Or(vec![
            Filter::Eq("a".into(), Value::Int(99)),
            Filter::Eq("b".into(), Value::Int(2)),
        ]);
        assert!(f2.matches(&d).expect("regex-free filter"));
    }

    #[test]
    fn exists() {
        let d = doc(vec![("x", Value::Null)]);
        assert!(
            Filter::Exists("x".into(), true)
                .matches(&d)
                .expect("regex-free filter")
        );
        assert!(
            !Filter::Exists("y".into(), true)
                .matches(&d)
                .expect("regex-free filter")
        );
        assert!(
            Filter::Exists("y".into(), false)
                .matches(&d)
                .expect("regex-free filter")
        );
    }
}
