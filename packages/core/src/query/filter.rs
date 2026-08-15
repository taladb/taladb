use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::document::{Document, Value};

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

impl Filter {
    /// Evaluate this filter against a document. Used as a post-filter after index scans.
    pub fn matches(&self, doc: &Document) -> bool {
        match self {
            Self::All => true,

            Self::Eq(field, val) => {
                if field == "_id" {
                    return matches!(val, Value::Str(s) if s == &doc.id.to_string());
                }
                doc.get(field)
                    .is_some_and(|v| any_element(v, |e| e == val))
            }

            // Negation is over the *document*, not the element: `$ne` excludes a
            // document any of whose elements equals the value, which is what
            // makes `$ne` the complement of `$eq` on an array field too.
            Self::Ne(field, val) => {
                if field == "_id" {
                    return !matches!(val, Value::Str(s) if s == &doc.id.to_string());
                }
                doc.get(field)
                    .is_none_or(|v| !any_element(v, |e| e == val))
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

            Self::And(filters) => filters.iter().all(|f| f.matches(doc)),

            Self::Or(filters) => filters.iter().any(|f| f.matches(doc)),

            Self::Not(inner) => !inner.matches(doc),

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

            // Regex post-filter — compiled on each invocation, always a full scan.
            // Size limits guard against ReDoS via catastrophic backtracking or
            // excessively large compiled automata from user-supplied patterns.
            Self::Regex(field, pattern) => {
                let Some(value) = doc.get(field) else {
                    return false;
                };
                let Ok(re) = regex::RegexBuilder::new(pattern)
                    // Limit the compiled NFA/DFA size to 1 MiB each.
                    .size_limit(1 << 20)
                    .dfa_size_limit(1 << 20)
                    .build()
                else {
                    return false;
                };
                any_element(value, |e| matches!(e, Value::Str(t) if re.is_match(t)))
            }
        }
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

    /// Pre-compile every regex pattern in this filter tree, with size limits
    /// guarding against ReDoS from user-supplied patterns. Returns an error
    /// for malformed patterns. Pass the result to [`Self::matches_with_cache`].
    pub(crate) fn compile_regex_cache(
        &self,
    ) -> Result<HashMap<String, regex::Regex>, crate::error::TalaDbError> {
        let patterns = self.collect_regex_patterns();
        let mut map = HashMap::with_capacity(patterns.len());
        for pat in patterns {
            let re = regex::RegexBuilder::new(&pat)
                .size_limit(1 << 20)
                .dfa_size_limit(1 << 20)
                .build()
                .map_err(|e| crate::error::TalaDbError::InvalidFilter(format!("regex: {e}")))?;
            map.insert(pat, re);
        }
        Ok(map)
    }

    /// Evaluate this filter using pre-compiled regexes from `regex_cache`.
    ///
    /// For Regex variants the cached `Regex` object is reused instead of
    /// recompiling the pattern.  All other variants delegate to `matches`.
    pub fn matches_with_cache(
        &self,
        doc: &Document,
        regex_cache: &HashMap<String, regex::Regex>,
    ) -> bool {
        match self {
            Self::Regex(field, pattern) => {
                let Some(re) = regex_cache.get(pattern) else {
                    return false;
                };
                doc.get(field).is_some_and(|v| {
                    any_element(v, |e| matches!(e, Value::Str(t) if re.is_match(t)))
                })
            }
            Self::And(filters) => filters
                .iter()
                .all(|f| f.matches_with_cache(doc, regex_cache)),
            Self::Or(filters) => filters
                .iter()
                .any(|f| f.matches_with_cache(doc, regex_cache)),
            Self::Not(inner) => !inner.matches_with_cache(doc, regex_cache),
            other => other.matches(doc),
        }
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
        assert!(Filter::Eq("age".into(), Value::Int(30)).matches(&d));
        assert!(!Filter::Eq("age".into(), Value::Int(31)).matches(&d));
    }

    #[test]
    fn range_ops() {
        let d = doc(vec![("score", Value::Float(7.5))]);
        assert!(Filter::Gt("score".into(), Value::Float(7.0)).matches(&d));
        assert!(!Filter::Gt("score".into(), Value::Float(8.0)).matches(&d));
        assert!(Filter::Lte("score".into(), Value::Float(7.5)).matches(&d));
    }

    #[test]
    fn and_or() {
        let d = doc(vec![("a", Value::Int(1)), ("b", Value::Int(2))]);
        let f = Filter::And(vec![
            Filter::Eq("a".into(), Value::Int(1)),
            Filter::Eq("b".into(), Value::Int(2)),
        ]);
        assert!(f.matches(&d));
        let f2 = Filter::Or(vec![
            Filter::Eq("a".into(), Value::Int(99)),
            Filter::Eq("b".into(), Value::Int(2)),
        ]);
        assert!(f2.matches(&d));
    }

    #[test]
    fn exists() {
        let d = doc(vec![("x", Value::Null)]);
        assert!(Filter::Exists("x".into(), true).matches(&d));
        assert!(!Filter::Exists("y".into(), true).matches(&d));
        assert!(Filter::Exists("y".into(), false).matches(&d));
    }
}
