use std::collections::HashSet;
use std::ops::Bound;
use std::time::Instant;

use ulid::Ulid;

use crate::document::{Document, Value};
use crate::engine::{ReadTxn, ScanFlow};
use crate::error::TalaDbError;
use crate::fts::{fts_table_name, fts_token_range, tokenize, ulid_from_fts_key};
use crate::index::{compound_table_name, docs_table_name, index_table_name, ulid_from_index_key};
use crate::query::filter::Filter;
use crate::query::planner::QueryPlan;

/// How many documents to decode per `get_many` round when a limit could cut the
/// walk short. Large enough that the per-batch table open amortises away, small
/// enough that `find_one` over a million candidates decodes ~256, not a million.
const FETCH_CHUNK: usize = 256;

/// Execute a query plan and return matching documents.
/// The `filter` is always applied as a post-filter to eliminate false positives.
///
/// `deadline` — if `Some`, the executor checks elapsed time in the document
/// filter loop and returns [`TalaDbError::QueryTimeout`] if the deadline is
/// exceeded.
pub fn execute(
    plan: &QueryPlan,
    filter: &Filter,
    txn: &dyn ReadTxn,
    collection: &str,
    deadline: Option<Instant>,
) -> Result<Vec<Document>, TalaDbError> {
    execute_limited(plan, filter, txn, collection, deadline, None)
}

/// [`execute`] that stops as soon as `limit` **matching** documents have been
/// found.
///
/// The limit is threaded all the way down to the storage walk rather than being
/// applied to a finished result set, which is the whole point: `find_one` over
/// an unindexed predicate used to decode every document in the collection to
/// return one. Pass `None` for the unbounded behaviour.
///
/// Only sound when the caller does not reorder afterwards — a `$sort` needs the
/// full candidate set, so `find_with_options` only passes a limit when there is
/// no sort spec.
pub fn execute_limited(
    plan: &QueryPlan,
    filter: &Filter,
    txn: &dyn ReadTxn,
    collection: &str,
    deadline: Option<Instant>,
    limit: Option<usize>,
) -> Result<Vec<Document>, TalaDbError> {
    // Check deadline up-front so callers that pass an already-expired deadline
    // don't touch storage at all.
    check_deadline(deadline)?;
    if limit == Some(0) {
        return Ok(vec![]);
    }

    // Pre-compile every regex in the filter tree once, and pre-tokenize a
    // `Contains` query once, instead of once per candidate document.
    let matcher = Matcher::new(filter)?;
    let mut results: Vec<Document> = Vec::new();

    // `true` while more documents are still wanted.
    let wants_more = |results: &Vec<Document>| limit.is_none_or(|n| results.len() < n);

    match plan {
        QueryPlan::FullScan => {
            // Stream the docs table: decode, test, and drop non-matches as we
            // go rather than materialising every document first.
            let table = docs_table_name(collection);
            let mut err: Option<TalaDbError> = None;
            txn.scan(&table, Bound::Unbounded, Bound::Unbounded, &mut |_, v| {
                if let Some(dl) = deadline
                    && Instant::now() >= dl
                {
                    err = Some(TalaDbError::QueryTimeout);
                    return Ok(ScanFlow::Stop);
                }
                let doc: Document = match postcard::from_bytes(v) {
                    Ok(d) => d,
                    Err(e) => {
                        err = Some(e.into());
                        return Ok(ScanFlow::Stop);
                    }
                };
                if matcher.matches(&doc) {
                    results.push(doc);
                }
                Ok(if wants_more(&results) {
                    ScanFlow::Continue
                } else {
                    ScanFlow::Stop
                })
            })?;
            if let Some(e) = err {
                return Err(e);
            }
        }

        // Primary-key point lookups; the post-filter still applies (covers
        // `_id` filters nested inside And/Or expressions).
        QueryPlan::ById { ids } => {
            fetch_filtered(txn, collection, ids, &matcher, deadline, limit, &mut results)?;
        }

        QueryPlan::IndexEq { field, start, end } => {
            // When the range holds only matches, the post-filter cannot reject
            // anything — so a bounded query can stop walking the index at
            // `limit` entries instead of collecting every one and then keeping
            // the first few. This is what makes `find_one` on an indexed
            // equality O(limit) rather than O(matches).
            let scan_cap = if plan_is_exact_for(plan, filter) {
                limit
            } else {
                None
            };
            let ulids = index_range_scan(
                txn,
                collection,
                field,
                Bound::Included(start.as_slice()),
                Bound::Included(end.as_slice()),
                scan_cap,
            )?;
            check_deadline(deadline)?;
            fetch_filtered(
                txn,
                collection,
                &ulids,
                &matcher,
                deadline,
                limit,
                &mut results,
            )?;
        }

        QueryPlan::IndexRange { field, start, end } => {
            let start_ref = bound_as_ref(start);
            let end_ref = bound_as_ref(end);
            let mut ulids = index_range_scan(txn, collection, field, start_ref, end_ref, None)?;
            // An array field is indexed once per element, so a document whose
            // list has several values inside the range appears once per value.
            // Without this the same document comes back two or three times from
            // an ordinary `$gte`.
            let mut seen: HashSet<Ulid> = HashSet::with_capacity(ulids.len());
            ulids.retain(|u| seen.insert(*u));
            check_deadline(deadline)?;
            fetch_filtered(
                txn,
                collection,
                &ulids,
                &matcher,
                deadline,
                limit,
                &mut results,
            )?;
        }

        QueryPlan::IndexIn { field, ranges } => {
            let mut ulids: Vec<Ulid> = Vec::new();
            for (start, end) in ranges {
                check_deadline(deadline)?;
                let mut batch = index_range_scan(
                    txn,
                    collection,
                    field,
                    Bound::Included(start.as_slice()),
                    Bound::Included(end.as_slice()),
                    None,
                )?;
                ulids.append(&mut batch);
            }
            // Ranges from duplicate $in values (or overlapping cross-type
            // numeric ranges) interleave, so adjacent-only Vec::dedup would
            // leave duplicates and the same document would be returned twice.
            let mut seen: HashSet<Ulid> = HashSet::with_capacity(ulids.len());
            ulids.retain(|u| seen.insert(*u));
            fetch_filtered(
                txn,
                collection,
                &ulids,
                &matcher,
                deadline,
                limit,
                &mut results,
            )?;
        }

        // Full-text search: intersect ULID sets across all query tokens (AND semantics).
        QueryPlan::FtsSearch { field, tokens } => {
            if tokens.is_empty() {
                return Ok(vec![]);
            }
            let table = fts_table_name(collection, field);
            // Collect ULID sets per token, then intersect
            let mut ulid_sets: Vec<HashSet<[u8; 16]>> = Vec::with_capacity(tokens.len());
            for token in tokens {
                check_deadline(deadline)?;
                let (start, end) = fts_token_range(token);
                let mut set: HashSet<[u8; 16]> = HashSet::new();
                txn.scan(
                    &table,
                    Bound::Included(start.as_slice()),
                    Bound::Included(end.as_slice()),
                    &mut |k, _| {
                        if let Some(u) = ulid_from_fts_key(k) {
                            set.insert(u.to_bytes());
                        }
                        Ok(ScanFlow::Continue)
                    },
                )?;
                // An absent token means an empty intersection — stop reading.
                if set.is_empty() {
                    return Ok(vec![]);
                }
                ulid_sets.push(set);
            }
            // Intersect all sets — documents must contain every token. Start
            // from the smallest so the probes run against the fewest elements.
            ulid_sets.sort_by_key(std::collections::HashSet::len);
            let intersection = ulid_sets
                .into_iter()
                .reduce(|a, b| a.into_iter().filter(|u| b.contains(u)).collect())
                .unwrap_or_default();
            let ulids: Vec<Ulid> = intersection.into_iter().map(Ulid::from_bytes).collect();
            fetch_filtered(
                txn,
                collection,
                &ulids,
                &matcher,
                deadline,
                limit,
                &mut results,
            )?;
        }

        QueryPlan::CompoundIndexEq { fields, start, end } => {
            let field_refs: Vec<&str> = fields.iter().map(std::string::String::as_str).collect();
            let table = compound_table_name(collection, &field_refs);
            let ulids = table_range_scan(
                txn,
                &table,
                Bound::Included(start.as_slice()),
                Bound::Included(end.as_slice()),
            )?;
            fetch_filtered(
                txn,
                collection,
                &ulids,
                &matcher,
                deadline,
                limit,
                &mut results,
            )?;
        }

        // Union the results of multiple index-backed sub-plans, deduplicating by ULID
        // before loading documents to avoid fetching the same document multiple times.
        QueryPlan::IndexOr { plans } => {
            let mut seen: HashSet<[u8; 16]> = HashSet::new();
            for sub_plan in plans {
                check_deadline(deadline)?;
                let ulids = collect_ulids(sub_plan, txn, collection, deadline)?;
                for id_bytes in ulids {
                    seen.insert(id_bytes);
                }
            }
            let ulids: Vec<Ulid> = seen.into_iter().map(Ulid::from_bytes).collect();
            fetch_filtered(
                txn,
                collection,
                &ulids,
                &matcher,
                deadline,
                limit,
                &mut results,
            )?;
        }

        // Intersect several index-backed sub-plans ($and where more than one
        // conjunct is indexed). Whichever branch is most selective bounds the
        // work: the smallest id set is intersected against the rest, so only
        // documents satisfying *every* indexed conjunct are ever decoded.
        QueryPlan::IndexAnd { plans } => {
            let mut sets: Vec<HashSet<[u8; 16]>> = Vec::with_capacity(plans.len());
            for sub_plan in plans {
                check_deadline(deadline)?;
                let set: HashSet<[u8; 16]> = collect_ulids(sub_plan, txn, collection, deadline)?
                    .into_iter()
                    .collect();
                // Empty intersection — no later branch can add anything back.
                if set.is_empty() {
                    return Ok(vec![]);
                }
                sets.push(set);
            }
            sets.sort_by_key(std::collections::HashSet::len);
            let intersection = sets
                .into_iter()
                .reduce(|a, b| a.into_iter().filter(|u| b.contains(u)).collect())
                .unwrap_or_default();
            let ulids: Vec<Ulid> = intersection.into_iter().map(Ulid::from_bytes).collect();
            fetch_filtered(
                txn,
                collection,
                &ulids,
                &matcher,
                deadline,
                limit,
                &mut results,
            )?;
        }
    }

    Ok(results)
}

/// A filter with its per-query work (regex compilation, query tokenization)
/// hoisted out of the per-document loop.
enum Matcher<'a> {
    /// `Contains` with the query tokenized once, as a set for O(1) probes.
    Contains {
        field: &'a str,
        tokens: HashSet<String>,
    },
    General {
        filter: &'a Filter,
        regexes: std::collections::HashMap<String, regex::Regex>,
    },
}

impl<'a> Matcher<'a> {
    fn new(filter: &'a Filter) -> Result<Self, TalaDbError> {
        if let Filter::Contains(field, query) = filter {
            return Ok(Matcher::Contains {
                field,
                tokens: tokenize(query).into_iter().collect(),
            });
        }
        // A malformed pattern fails fast here rather than silently matching
        // nothing for every document.
        Ok(Matcher::General {
            filter,
            regexes: filter.compile_regex_cache()?,
        })
    }

    fn matches(&self, doc: &Document) -> bool {
        match self {
            Matcher::Contains { field, tokens } => {
                if tokens.is_empty() {
                    return true;
                }
                match doc.get(field) {
                    Some(Value::Str(text)) => {
                        let doc_tokens: HashSet<String> = tokenize(text).into_iter().collect();
                        tokens.iter().all(|t| doc_tokens.contains(t))
                    }
                    _ => false,
                }
            }
            Matcher::General { filter, regexes } => filter.matches_with_cache(doc, regexes),
        }
    }
}

/// Fetch the named documents in batches, testing each against `matcher` and
/// stopping once `limit` matches have been collected.
///
/// Batching is what makes this cheap: one `open_table` per `FETCH_CHUNK` keys
/// instead of one per key (~2.3 µs each on redb, which otherwise dominates).
#[allow(clippy::too_many_arguments)]
fn fetch_filtered(
    txn: &dyn ReadTxn,
    collection: &str,
    ulids: &[Ulid],
    matcher: &Matcher<'_>,
    deadline: Option<Instant>,
    limit: Option<usize>,
    out: &mut Vec<Document>,
) -> Result<(), TalaDbError> {
    let table = docs_table_name(collection);
    let key_store: Vec<[u8; 16]> = ulids.iter().map(ulid::Ulid::to_bytes).collect();
    for chunk in key_store.chunks(FETCH_CHUNK) {
        check_deadline(deadline)?;
        let keys: Vec<&[u8]> = chunk.iter().map(<[u8; 16]>::as_slice).collect();
        for bytes in txn.get_many(&table, &keys)?.into_iter().flatten() {
            let doc: Document = postcard::from_bytes(&bytes)?;
            if matcher.matches(&doc) {
                out.push(doc);
                if limit.is_some_and(|n| out.len() >= n) {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// Collect raw ULID bytes from an index-backed plan without loading documents.
/// Used by `IndexOr` to deduplicate before fetching.
fn collect_ulids(
    plan: &QueryPlan,
    txn: &dyn ReadTxn,
    collection: &str,
    deadline: Option<Instant>,
) -> Result<Vec<[u8; 16]>, TalaDbError> {
    match plan {
        QueryPlan::ById { ids } => Ok(ids.iter().map(ulid::Ulid::to_bytes).collect()),

        QueryPlan::IndexEq { field, start, end } => {
            let ulids = index_range_scan(
                txn,
                collection,
                field,
                Bound::Included(start.as_slice()),
                Bound::Included(end.as_slice()),
                None,
            )?;
            Ok(ulids.into_iter().map(|u| u.to_bytes()).collect())
        }
        QueryPlan::IndexRange { field, start, end } => {
            let start_ref = bound_as_ref(start);
            let end_ref = bound_as_ref(end);
            let ulids = index_range_scan(txn, collection, field, start_ref, end_ref, None)?;
            Ok(ulids.into_iter().map(|u| u.to_bytes()).collect())
        }
        QueryPlan::IndexIn { field, ranges } => {
            let mut result: Vec<[u8; 16]> = Vec::new();
            for (start, end) in ranges {
                let batch = index_range_scan(
                    txn,
                    collection,
                    field,
                    Bound::Included(start.as_slice()),
                    Bound::Included(end.as_slice()),
                    None,
                )?;
                result.extend(batch.into_iter().map(|u| u.to_bytes()));
            }
            Ok(result)
        }
        QueryPlan::CompoundIndexEq { fields, start, end } => {
            let field_refs: Vec<&str> = fields.iter().map(std::string::String::as_str).collect();
            let table = compound_table_name(collection, &field_refs);
            let ulids = table_range_scan(
                txn,
                &table,
                Bound::Included(start.as_slice()),
                Bound::Included(end.as_slice()),
            )?;
            Ok(ulids.into_iter().map(|u| u.to_bytes()).collect())
        }
        _ => {
            // For non-index plans, fall back to executing and extracting ids
            let docs = execute(plan, &Filter::All, txn, collection, deadline)?;
            Ok(docs.into_iter().map(|d| d.id.to_bytes()).collect())
        }
    }
}

/// Count documents matching `filter` without keeping any of them.
///
/// Same walk as [`execute`], minus the `Vec<Document>` that a caller only ever
/// calls `.len()` on. When the plan is index-backed and the filter is fully
/// covered by it, the index entries alone answer the question and no document
/// is decoded at all.
pub fn count_matching(
    plan: &QueryPlan,
    filter: &Filter,
    txn: &dyn ReadTxn,
    collection: &str,
) -> Result<u64, TalaDbError> {
    // An index scan whose plan exactly reproduces the filter needs no documents:
    // every entry in the range is a match by construction.
    if plan_is_exact_for(plan, filter) {
        return count_plan_entries(plan, txn, collection);
    }
    Ok(execute(plan, filter, txn, collection, None)?.len() as u64)
}

/// Whether every id the plan yields is guaranteed to satisfy `filter`, so the
/// post-filter can be skipped.
///
/// Deliberately conservative — only the single-predicate shapes where the index
/// range and the predicate are the same thing. Anything else falls back to
/// decoding and testing.
fn plan_is_exact_for(plan: &QueryPlan, filter: &Filter) -> bool {
    match (plan, filter) {
        (QueryPlan::IndexEq { field, .. }, Filter::Eq(f, v)) => {
            if field != f {
                return false;
            }
            match v {
                // Not indexable, so the index cannot speak for them at all.
                Value::Array(_) | Value::Object(_) => false,
                // NaN encodes to a real key and the index range would find a
                // stored NaN — but `Eq` compares with `==`, and NaN equals
                // nothing, so the filter matches zero documents. Counting the
                // range would disagree with `find`.
                //
                // (±0.0 is not a concern here: the planner emits `IndexIn`
                // covering both encodings for a zero float, never `IndexEq`.)
                Value::Float(f) if f.is_nan() => false,
                // Every other value encodes injectively within its type tag, so
                // the range holds exactly the documents `Eq` matches.
                _ => true,
            }
        }
        (QueryPlan::CompoundIndexEq { .. }, Filter::And(_)) => false,
        _ => false,
    }
}

/// Count the entries a plan's index range covers, without touching documents.
fn count_plan_entries(
    plan: &QueryPlan,
    txn: &dyn ReadTxn,
    collection: &str,
) -> Result<u64, TalaDbError> {
    match plan {
        QueryPlan::IndexEq { field, start, end } => {
            let table = index_table_name(collection, field);
            let mut n = 0u64;
            txn.scan(
                &table,
                Bound::Included(start.as_slice()),
                Bound::Included(end.as_slice()),
                &mut |_, _| {
                    n += 1;
                    Ok(ScanFlow::Continue)
                },
            )?;
            Ok(n)
        }
        // `plan_is_exact_for` admits nothing else; kept total for safety.
        _ => Ok(execute(plan, &Filter::All, txn, collection, None)?.len() as u64),
    }
}

#[inline]
fn check_deadline(deadline: Option<Instant>) -> Result<(), TalaDbError> {
    if let Some(dl) = deadline
        && Instant::now() >= dl
    {
        return Err(TalaDbError::QueryTimeout);
    }
    Ok(())
}

fn bound_as_ref(b: &Bound<Vec<u8>>) -> Bound<&[u8]> {
    match b {
        Bound::Included(v) => Bound::Included(v.as_slice()),
        Bound::Excluded(v) => Bound::Excluded(v.as_slice()),
        Bound::Unbounded => Bound::Unbounded,
    }
}

/// Collect the ULIDs in an index range.
///
/// `cap` stops the walk after that many entries. Pass it **only** when every
/// entry in the range is known to satisfy the query's filter, otherwise a
/// post-filter rejection would leave the result short.
fn index_range_scan(
    txn: &dyn ReadTxn,
    collection: &str,
    field: &str,
    start: Bound<&[u8]>,
    end: Bound<&[u8]>,
    cap: Option<usize>,
) -> Result<Vec<Ulid>, TalaDbError> {
    let table = index_table_name(collection, field);
    table_range_scan_capped(txn, &table, start, end, cap)
}

/// One entry of a secondary index, in index order.
pub(crate) struct IndexEntry {
    /// The order-preserving encoding of the indexed value (the key without its
    /// trailing ULID). Equal prefixes ⇒ equal values, so this is all we need to
    /// spot the boundary of a run of ties.
    pub value_prefix: Vec<u8>,
    pub id: Ulid,
}

/// Walk a secondary index end to end, in **key order**, without decoding a
/// single document.
///
/// Index keys are `encode_value_prefix(value) ++ ulid`, and the encoding is
/// order-preserving — so this yields entries ordered by `(value asc, id asc)`,
/// which is exactly the total order `cmp_rows` defines for an ascending sort.
/// A descending sort is the same sequence reversed.
///
/// This is what lets a sorted page be served by decoding only the documents on
/// that page, instead of materialising the whole collection to sort it.
pub(crate) fn index_ordered_entries(
    txn: &dyn ReadTxn,
    collection: &str,
    field: &str,
) -> Result<Vec<IndexEntry>, TalaDbError> {
    let table = index_table_name(collection, field);
    let mut out = Vec::new();
    txn.scan(&table, Bound::Unbounded, Bound::Unbounded, &mut |k, _| {
        // The ULID is the last 16 bytes; everything before it is the value.
        if let Some(id) = ulid_from_index_key(k)
            && let Some(split) = k.len().checked_sub(16)
        {
            out.push(IndexEntry {
                value_prefix: k[..split].to_vec(),
                id,
            });
        }
        Ok(ScanFlow::Continue)
    })?;
    Ok(out)
}

/// Decode just the named documents. Public to the crate so `aggregate` can
/// materialise only the page it is about to return.
pub(crate) fn fetch_documents(
    txn: &dyn ReadTxn,
    collection: &str,
    ulids: Vec<Ulid>,
) -> Result<Vec<Document>, TalaDbError> {
    fetch_by_ulids(txn, collection, &ulids)
}

fn table_range_scan(
    txn: &dyn ReadTxn,
    table: &str,
    start: Bound<&[u8]>,
    end: Bound<&[u8]>,
) -> Result<Vec<Ulid>, TalaDbError> {
    table_range_scan_capped(txn, table, start, end, None)
}

fn table_range_scan_capped(
    txn: &dyn ReadTxn,
    table: &str,
    start: Bound<&[u8]>,
    end: Bound<&[u8]>,
    cap: Option<usize>,
) -> Result<Vec<Ulid>, TalaDbError> {
    if cap == Some(0) {
        return Ok(vec![]);
    }
    let mut ulids = Vec::new();
    txn.scan(table, start, end, &mut |k, _| {
        if let Some(u) = ulid_from_index_key(k) {
            ulids.push(u);
        }
        Ok(match cap {
            Some(n) if ulids.len() >= n => ScanFlow::Stop,
            _ => ScanFlow::Continue,
        })
    })?;
    Ok(ulids)
}

/// Decode every named document, in `ulids` order. One `open_table` per
/// [`FETCH_CHUNK`] keys rather than one per key.
pub(crate) fn fetch_by_ulids(
    txn: &dyn ReadTxn,
    collection: &str,
    ulids: &[Ulid],
) -> Result<Vec<Document>, TalaDbError> {
    let table = docs_table_name(collection);
    let key_store: Vec<[u8; 16]> = ulids.iter().map(ulid::Ulid::to_bytes).collect();
    let mut docs = Vec::with_capacity(ulids.len());
    for chunk in key_store.chunks(FETCH_CHUNK) {
        let keys: Vec<&[u8]> = chunk.iter().map(<[u8; 16]>::as_slice).collect();
        for bytes in txn.get_many(&table, &keys)?.into_iter().flatten() {
            docs.push(postcard::from_bytes(&bytes)?);
        }
    }
    Ok(docs)
}
