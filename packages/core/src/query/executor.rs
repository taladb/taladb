use std::collections::HashSet;
use std::ops::Bound;
use std::time::Instant;

use ulid::Ulid;

use crate::document::{Document, Value};
use crate::engine::{ReadTxn, ScanFlow};
use crate::error::TalaDbError;
use crate::fts::{fts_table_name, fts_token_range, tokenize, ulid_from_fts_key};
use crate::index::{
    CompoundIndexDef, IndexDef, compound_table_name, docs_table_name, index_table_name,
    ulid_from_index_key,
};
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

/// Resolve a filter into IDs without retaining decoded document bodies.
pub(crate) fn matching_ids(
    plan: &QueryPlan,
    filter: &Filter,
    txn: &dyn ReadTxn,
    collection: &str,
    indexes: &[IndexDef],
    compounds: &[CompoundIndexDef],
) -> Result<HashSet<[u8; 16]>, TalaDbError> {
    Ok(matching_ids_limited(
        plan,
        filter,
        txn,
        collection,
        indexes,
        compounds,
        usize::MAX,
    )?
    .expect("an unlimited ID collection cannot request streaming"))
}

// None requests streaming. The limit applies to intermediate index branches,
// unions and residual matches as well as the final eligible set.
pub(crate) fn matching_ids_limited(
    plan: &QueryPlan,
    filter: &Filter,
    txn: &dyn ReadTxn,
    collection: &str,
    indexes: &[IndexDef],
    compounds: &[CompoundIndexDef],
    limit: usize,
) -> Result<Option<HashSet<[u8; 16]>>, TalaDbError> {
    let matcher = Matcher::new(filter)?;
    let covered =
        match super::index_filter::resolve(filter, indexes, compounds, txn, collection, limit) {
            Ok(Some(covered)) if covered.exact => return Ok(Some(covered.ids)),
            Ok(covered) => covered,
            Err(super::index_filter::ResolveError::Limit) => return Ok(None),
            Err(super::index_filter::ResolveError::Database(error)) => return Err(error),
        };
    let mut ids = HashSet::new();
    let fields = super::filter_document::fields(filter);
    let table = docs_table_name(collection);
    let mut overflow = false;
    let mut accept = |bytes: &[u8]| -> Result<bool, TalaDbError> {
        let doc = super::filter_document::decode(bytes, &fields)?;
        if matcher.matches(&doc)? {
            if ids.len() >= limit && !ids.contains(&doc.id.to_bytes()) {
                overflow = true;
                return Ok(false);
            }
            ids.insert(doc.id.to_bytes());
        }
        Ok(true)
    };
    if let Some(covered) = covered {
        for id in covered.ids {
            if let Some(bytes) = txn.get(&table, &id)?
                && !accept(&bytes)?
            {
                break;
            }
        }
    } else if let QueryPlan::ById { ids } = plan {
        for id in ids {
            if let Some(bytes) = txn.get(&table, &id.to_bytes())?
                && !accept(&bytes)?
            {
                break;
            }
        }
    } else if limit != usize::MAX {
        visit_plan_documents(plan, txn, collection, &mut accept)?;
    } else if matches!(plan, QueryPlan::FullScan) {
        txn.scan(
            &table,
            Bound::Unbounded,
            Bound::Unbounded,
            &mut |_, bytes| {
                Ok(if accept(bytes)? {
                    ScanFlow::Continue
                } else {
                    ScanFlow::Stop
                })
            },
        )?;
    } else {
        for id in collect_ulids(plan, txn, collection, None)? {
            if let Some(bytes) = txn.get(&table, &id)? {
                accept(&bytes)?;
            }
        }
    }
    Ok((!overflow).then_some(ids))
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
    // Keep ordinary full scans on the direct document collector. The ID-only
    // mutation/count collector below uses projection; routing normal scans
    // through that sink adds dispatch to an otherwise tight decode/filter loop.
    if matches!(plan, QueryPlan::FullScan) {
        check_deadline(deadline)?;
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        let matcher = Matcher::new(filter)?;
        let mut docs = Vec::new();
        let more = |docs: &Vec<Document>| {
            if limit.is_some_and(|n| docs.len() >= n) {
                ScanFlow::Stop
            } else {
                ScanFlow::Continue
            }
        };
        let table = docs_table_name(collection);
        if matches!(filter, Filter::All) {
            txn.scan(
                &table,
                Bound::Unbounded,
                Bound::Unbounded,
                &mut |_, bytes| {
                    check_deadline(deadline)?;
                    let doc = postcard::from_bytes(bytes)?;
                    if matcher.matches(&doc)? {
                        docs.push(doc);
                    }
                    Ok(more(&docs))
                },
            )?;
        } else {
            let mut decoder = ScanDecoder::new(filter);
            txn.scan(
                &table,
                Bound::Unbounded,
                Bound::Unbounded,
                &mut |_, bytes| {
                    check_deadline(deadline)?;
                    // The whole-document branch is the loop above, unchanged.
                    let matched = if decoder.project {
                        decoder
                            .projected(bytes, &matcher)?
                            .map(|doc| docs.push(doc))
                    } else {
                        let doc = postcard::from_bytes(bytes)?;
                        matcher.matches(&doc)?.then(|| docs.push(doc))
                    };
                    decoder.tick(matched.is_some());
                    Ok(more(&docs))
                },
            )?;
        }
        return Ok(docs);
    }
    let mut docs = Vec::new();
    execute_into(
        plan,
        filter,
        txn,
        collection,
        deadline,
        limit,
        false,
        &mut |doc| docs.push(doc),
    )?;
    Ok(docs)
}

/// Keep the plan's natural order while retaining only IDs for write-side
/// revalidation. Each candidate body is released immediately.
pub(crate) fn candidate_ids(
    plan: &QueryPlan,
    filter: &Filter,
    txn: &dyn ReadTxn,
    collection: &str,
) -> Result<Vec<Ulid>, TalaDbError> {
    let mut ids = Vec::new();
    execute_into(
        plan,
        filter,
        txn,
        collection,
        None,
        None,
        true,
        &mut |doc| ids.push(doc.id),
    )?;
    Ok(ids)
}

struct DocumentSink<'a> {
    emit: &'a mut dyn FnMut(Document),
    count: usize,
    fields: Option<super::filter_document::Fields<'a>>,
}

impl DocumentSink<'_> {
    fn decode(&self, bytes: &[u8]) -> Result<Document, postcard::Error> {
        match &self.fields {
            Some(fields) => super::filter_document::decode(bytes, fields),
            None => postcard::from_bytes(bytes),
        }
    }
    fn push(&mut self, doc: Document) {
        (self.emit)(doc);
        self.count += 1;
    }
    fn len(&self) -> usize {
        self.count
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_into(
    plan: &QueryPlan,
    filter: &Filter,
    txn: &dyn ReadTxn,
    collection: &str,
    deadline: Option<Instant>,
    limit: Option<usize>,
    ids_only: bool,
    emit: &mut dyn FnMut(Document),
) -> Result<(), TalaDbError> {
    // Check deadline up-front so callers that pass an already-expired deadline
    // don't touch storage at all.
    check_deadline(deadline)?;
    if limit == Some(0) {
        return Ok(());
    }

    // Pre-compile every regex in the filter tree once, and pre-tokenize a
    // `Contains` query once, instead of once per candidate document.
    let matcher = Matcher::new(filter)?;
    let mut results = DocumentSink {
        emit,
        count: 0,
        fields: ids_only.then(|| super::filter_document::fields(filter)),
    };

    // `true` while more documents are still wanted.
    let wants_more = |results: &DocumentSink<'_>| limit.is_none_or(|n| results.len() < n);

    match plan {
        QueryPlan::FullScan => {
            // Stream the docs table: decode, test, and drop non-matches as we
            // go rather than materialising every document first.
            let table = docs_table_name(collection);
            // Choose the decoder once per scan. Keeping that branch inside
            // the callback adds measurable overhead to ordinary full scans.
            let fields = results.fields.take();
            let mut accept = |doc: Document| {
                if matcher.matches(&doc)? {
                    results.push(doc);
                }
                Ok(if wants_more(&results) {
                    ScanFlow::Continue
                } else {
                    ScanFlow::Stop
                })
            };
            match fields {
                Some(fields) => {
                    txn.scan(&table, Bound::Unbounded, Bound::Unbounded, &mut |_, v| {
                        check_deadline(deadline)?;
                        accept(super::filter_document::decode(v, &fields)?)
                    })?
                }
                None => txn.scan(&table, Bound::Unbounded, Bound::Unbounded, &mut |_, v| {
                    check_deadline(deadline)?;
                    accept(postcard::from_bytes(v)?)
                })?,
            }
        }

        // Primary-key point lookups; the post-filter still applies (covers
        // `_id` filters nested inside And/Or expressions).
        QueryPlan::ById { ids } => {
            fetch_filtered(
                txn,
                collection,
                ids,
                &matcher,
                deadline,
                limit,
                &mut results,
            )?;
        }

        QueryPlan::IndexEq { field, start, end } => {
            fetch_index_range(
                txn,
                &index_table_name(collection, field),
                Bound::Included(start),
                Bound::Included(end),
                collection,
                &matcher,
                deadline,
                limit,
                &mut results,
                None,
            )?;
        }
        QueryPlan::IndexRange { field, start, end } => {
            let mut seen = HashSet::new();
            fetch_index_range(
                txn,
                &index_table_name(collection, field),
                bound_as_ref(start),
                bound_as_ref(end),
                collection,
                &matcher,
                deadline,
                limit,
                &mut results,
                Some(&mut seen),
            )?;
        }
        QueryPlan::IndexIn { field, ranges } => {
            let table = index_table_name(collection, field);
            let mut seen = HashSet::new();
            for (start, end) in ranges {
                fetch_index_range(
                    txn,
                    &table,
                    Bound::Included(start),
                    Bound::Included(end),
                    collection,
                    &matcher,
                    deadline,
                    limit,
                    &mut results,
                    Some(&mut seen),
                )?;
                if !wants_more(&results) {
                    break;
                }
            }
        }

        // Full-text search: intersect ULID sets across all query tokens (AND semantics).
        QueryPlan::FtsSearch { field, tokens } => {
            if tokens.is_empty() {
                return Ok(());
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
                    return Ok(());
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
            let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
            fetch_index_range(
                txn,
                &compound_table_name(collection, &fields),
                Bound::Included(start),
                Bound::Included(end),
                collection,
                &matcher,
                deadline,
                limit,
                &mut results,
                None,
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
            // A two-sided range on a scalar field: walk the shared key ranges in
            // key order instead of materialising both halves' ID sets. Disjoint
            // ranges over one key per document need no dedup set, and the walk
            // stops at the limit.
            if let Some(QueryPlan::IndexOr { plans: ranges }) =
                super::index_filter::scalar_range_plan(plan, txn, collection)?
            {
                for range in &ranges {
                    let QueryPlan::IndexRange { field, start, end } = range else {
                        unreachable!("scalar_range_plan yields key ranges only");
                    };
                    fetch_index_range(
                        txn,
                        &index_table_name(collection, field),
                        bound_as_ref(start),
                        bound_as_ref(end),
                        collection,
                        &matcher,
                        deadline,
                        limit,
                        &mut results,
                        None,
                    )?;
                }
                return Ok(());
            }
            let mut sets: Vec<HashSet<[u8; 16]>> = Vec::with_capacity(plans.len());
            for sub_plan in plans {
                check_deadline(deadline)?;
                let set: HashSet<[u8; 16]> = collect_ulids(sub_plan, txn, collection, deadline)?
                    .into_iter()
                    .collect();
                // Empty intersection — no later branch can add anything back.
                if set.is_empty() {
                    return Ok(());
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

    Ok(())
}

/// A filter with its per-query work (regex compilation, query tokenization)
/// hoisted out of the per-document loop.
pub(crate) enum Matcher<'a> {
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
    pub(crate) fn new(filter: &'a Filter) -> Result<Self, TalaDbError> {
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

    /// # Errors
    ///
    /// Propagates a filter-evaluation failure. In practice `Matcher::new` has
    /// already compiled every pattern, so this cannot fail for a matcher built
    /// from the same filter — but swallowing it here is what turned a bad regex
    /// under `$not` into "match everything".
    pub(crate) fn matches(&self, doc: &Document) -> Result<bool, TalaDbError> {
        match self {
            Matcher::Contains { field, tokens } => {
                if tokens.is_empty() {
                    return Ok(true);
                }
                Ok(match doc.get(field) {
                    Some(Value::Str(text)) => {
                        let doc_tokens: HashSet<String> = tokenize(text).into_iter().collect();
                        tokens.iter().all(|t| doc_tokens.contains(t))
                    }
                    _ => false,
                })
            }
            Matcher::General { filter, regexes } => filter.matches_with_cache(doc, regexes),
        }
    }
}

/// Documents per [`ScanDecoder`] decision.
const DECODE_WINDOW: u64 = 32;

/// Decodes the documents of a filtered full scan, choosing per window of
/// [`DECODE_WINDOW`] documents between two strategies:
///
/// * **projected** — decode only the top-level fields the filter reads, test
///   that, and decode the whole document again only when it matches;
/// * **whole** — decode every document whole and test it, as a scan always did.
///
/// Projection saves what a miss would have allocated and copied, and costs a
/// second decode per match. It pays while the share of matches stays below a
/// third of the share of bytes the projection skips. After each window the
/// rule is applied to the match rate of the recent windows (decayed by 1/8 per
/// window, so a run of matches in periodic data does not flip the choice back
/// and forth) and to the bytes measured in the last projected window. A
/// selective filter over wide documents keeps projecting; a filter that most
/// documents pass, or that reads most of each document, goes back to whole
/// decoding within a few windows, and returns to projecting if matches become
/// rare again.
///
/// Either way the returned document comes from the full decoder, and the
/// projection holds every field the filter can read (see
/// [`super::filter_document`]), so the result is the same document in the same
/// order.
struct ScanDecoder<'a> {
    fields: super::filter_document::Fields<'a>,
    project: bool,
    seen: u64,
    matched: u64,
    /// Decayed `(matched, seen)` counts of the windows so far.
    rate: (u64, u64),
    /// Encoded bytes and decoded (not skipped) bytes in this projected window.
    bytes: u64,
    kept: u64,
    /// `(skipped, total)` bytes of the last projected window.
    share: (u64, u64),
}

impl<'a> ScanDecoder<'a> {
    fn new(filter: &'a Filter) -> Self {
        Self {
            fields: super::filter_document::fields(filter),
            project: true,
            seen: 0,
            matched: 0,
            rate: (0, 0),
            bytes: 0,
            kept: 0,
            share: (0, 0),
        }
    }

    /// The document, decoded whole, if its projection matches. While
    /// `project` is false the caller decodes and tests documents itself.
    fn projected(
        &mut self,
        bytes: &[u8],
        matcher: &Matcher<'_>,
    ) -> Result<Option<Document>, TalaDbError> {
        match super::filter_document::decode(bytes, &self.fields) {
            Ok(projected) => {
                self.bytes += bytes.len() as u64;
                self.kept += super::filter_document::decoded_len(&projected);
                Ok(if matcher.matches(&projected)? {
                    Some(postcard::from_bytes(bytes)?)
                } else {
                    None
                })
            }
            // The full decoder is the reference: return its error, or its
            // document should it accept what the projection did not.
            Err(_) => {
                let doc: Document = postcard::from_bytes(bytes)?;
                Ok(matcher.matches(&doc)?.then_some(doc))
            }
        }
    }

    /// Count one scanned document, and choose the strategy for the next
    /// window when this one is complete.
    fn tick(&mut self, matched: bool) {
        self.seen += 1;
        self.matched += u64::from(matched);
        if self.seen == DECODE_WINDOW {
            if self.project {
                self.share = (self.bytes.saturating_sub(self.kept), self.bytes);
            }
            let (matched, seen) = self.rate;
            self.rate = (
                matched - matched / 8 + self.matched,
                seen - seen / 8 + self.seen,
            );
            let (skipped, total) = self.share;
            self.project = self.rate.0 * 3 * total < skipped * self.rate.1;
            self.seen = 0;
            self.matched = 0;
            self.bytes = 0;
            self.kept = 0;
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
    out: &mut DocumentSink<'_>,
) -> Result<(), TalaDbError> {
    let table = docs_table_name(collection);
    for chunk in ulids.chunks(FETCH_CHUNK) {
        check_deadline(deadline)?;
        let key_store: Vec<[u8; 16]> = chunk.iter().map(Ulid::to_bytes).collect();
        let keys: Vec<&[u8]> = key_store.iter().map(<[u8; 16]>::as_slice).collect();
        for bytes in txn.get_many(&table, &keys)?.into_iter().flatten() {
            let doc = out.decode(&bytes)?;
            if matcher.matches(&doc)? {
                out.push(doc);
                if limit.is_some_and(|n| out.len() >= n) {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// Keep only a bounded page of index IDs ahead of the document fetch. Resume
/// after the last raw key, preserving range order and deduplicating array and
/// overlapping `$in` entries before decoding their bodies.
#[allow(clippy::too_many_arguments)]
fn fetch_index_range(
    txn: &dyn ReadTxn,
    table: &str,
    start: Bound<&[u8]>,
    end: Bound<&[u8]>,
    collection: &str,
    matcher: &Matcher<'_>,
    deadline: Option<Instant>,
    limit: Option<usize>,
    out: &mut DocumentSink<'_>,
    mut seen: Option<&mut HashSet<Ulid>>,
) -> Result<(), TalaDbError> {
    let mut last: Option<Vec<u8>> = None;
    loop {
        check_deadline(deadline)?;
        let cap = limit.map_or(FETCH_CHUNK, |n| {
            n.saturating_sub(out.len()).min(FETCH_CHUNK)
        });
        if cap == 0 {
            return Ok(());
        }
        let mut ids = Vec::with_capacity(cap);
        let mut next = None;
        let mut scanned = 0;
        let lower = last.as_deref().map_or(start, Bound::Excluded);
        txn.scan(table, lower, end, &mut |key, _| {
            check_deadline(deadline)?;
            scanned += 1;
            let cursor = next.get_or_insert_with(Vec::new);
            cursor.clear();
            cursor.extend_from_slice(key);
            if let Some(id) = ulid_from_index_key(key)
                && seen.as_mut().is_none_or(|set| set.insert(id))
            {
                ids.push(id);
            }
            Ok(if scanned >= cap {
                ScanFlow::Stop
            } else {
                ScanFlow::Continue
            })
        })?;
        fetch_filtered(txn, collection, &ids, matcher, deadline, limit, out)?;
        if scanned < cap {
            return Ok(());
        }
        last = next;
    }
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
        QueryPlan::IndexOr { plans } => {
            let mut ids = HashSet::new();
            for plan in plans {
                ids.extend(collect_ulids(plan, txn, collection, deadline)?);
            }
            Ok(ids.into_iter().collect())
        }
        QueryPlan::IndexAnd { plans } => {
            if let Some(narrowed) = super::index_filter::scalar_range_plan(plan, txn, collection)? {
                return collect_ulids(&narrowed, txn, collection, deadline);
            }
            let mut ids: Option<HashSet<[u8; 16]>> = None;
            for plan in plans {
                let next: HashSet<_> = collect_ulids(plan, txn, collection, deadline)?
                    .into_iter()
                    .collect();
                if let Some(current) = &mut ids {
                    current.retain(|id| next.contains(id));
                } else {
                    ids = Some(next);
                }
            }
            Ok(ids.unwrap_or_default().into_iter().collect())
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
    let mut count = 0;
    execute_into(plan, filter, txn, collection, None, None, true, &mut |_| {
        count += 1
    })?;
    Ok(count)
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
                // the range's keys of the value's own length are exactly the
                // documents `Eq` matches (`count_plan_entries` skips the rest).
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
            // A Str/Bytes value with an escaped NUL extends a shorter value's
            // prefix (`"a\0b"` is `"a"`'s prefix ++ 0xFF ++ …), so its keys fall
            // inside this range. Only keys of exactly the bound's length — the
            // value prefix plus an id — hold the value itself.
            txn.scan(
                &table,
                Bound::Included(start.as_slice()),
                Bound::Included(end.as_slice()),
                &mut |key, _| {
                    if key.len() == start.len() {
                        n += 1;
                    }
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
    let mut docs = Vec::with_capacity(ulids.len());
    for chunk in ulids.chunks(FETCH_CHUNK) {
        let key_store: Vec<[u8; 16]> = chunk.iter().map(Ulid::to_bytes).collect();
        let keys: Vec<&[u8]> = key_store.iter().map(<[u8; 16]>::as_slice).collect();
        for bytes in txn.get_many(&table, &keys)?.into_iter().flatten() {
            docs.push(postcard::from_bytes(&bytes)?);
        }
    }
    Ok(docs)
}

/// Emit unique matches without collecting their IDs. Covered scalar plans use
/// index keys; arrays, partial coverage and negation stream one projected body
/// at a time, preserving full document predicate semantics and deduplication.
pub(crate) fn visit_matching_ids(
    plan: &QueryPlan,
    filter: &Filter,
    txn: &dyn ReadTxn,
    collection: &str,
    indexes: &[IndexDef],
    compounds: &[CompoundIndexDef],
    accept: &mut dyn FnMut([u8; 16]) -> Result<(), TalaDbError>,
) -> Result<(), TalaDbError> {
    let matcher = Matcher::new(filter)?;
    if super::index_filter::visit_unique(filter, indexes, compounds, txn, collection, accept)? {
        return Ok(());
    }
    let fields = super::filter_document::fields(filter);
    let mut accept_doc = |bytes: &[u8]| -> Result<bool, TalaDbError> {
        let doc = super::filter_document::decode(bytes, &fields)?;
        if matcher.matches(&doc)? {
            accept(doc.id.to_bytes())?;
        }
        Ok(true)
    };
    if let QueryPlan::FtsSearch { field, tokens } = plan {
        return visit_fts_documents(txn, collection, field, tokens, &mut accept_doc).map(|_| ());
    }
    let table = docs_table_name(collection);
    // A positive AND child is a superset of the final matches. Preserve its
    // index narrowing while checking residual fields, without an unbounded set.
    let mut accept_id = |id: [u8; 16]| -> Result<(), TalaDbError> {
        if let Some(bytes) = txn.get(&table, &id)? {
            accept_doc(&bytes)?;
        }
        Ok(())
    };
    if visit_index_seed(filter, indexes, compounds, txn, collection, &mut accept_id)? {
        return Ok(());
    }
    txn.scan(
        &table,
        Bound::Unbounded,
        Bound::Unbounded,
        &mut |_, bytes| {
            accept_doc(bytes)?;
            Ok(ScanFlow::Continue)
        },
    )
}

fn visit_index_seed(
    filter: &Filter,
    indexes: &[IndexDef],
    compounds: &[CompoundIndexDef],
    txn: &dyn ReadTxn,
    collection: &str,
    accept: &mut dyn FnMut([u8; 16]) -> Result<(), TalaDbError>,
) -> Result<bool, TalaDbError> {
    if let Filter::And(children) = filter {
        for child in children {
            if super::index_filter::visit_unique(
                child, indexes, compounds, txn, collection, accept,
            )? || visit_index_seed(child, indexes, compounds, txn, collection, accept)?
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// Preserve indexed source plans for capped collection, including mixed OR
// branches. The capped collector deduplicates IDs and checks the whole filter.
fn visit_plan_documents(
    plan: &QueryPlan,
    txn: &dyn ReadTxn,
    collection: &str,
    accept: &mut impl FnMut(&[u8]) -> Result<bool, TalaDbError>,
) -> Result<bool, TalaDbError> {
    match plan {
        QueryPlan::FtsSearch { field, tokens } => {
            visit_fts_documents(txn, collection, field, tokens, accept)
        }
        QueryPlan::ById { ids } => {
            let table = docs_table_name(collection);
            for id in ids {
                if let Some(bytes) = txn.get(&table, &id.to_bytes())?
                    && !accept(&bytes)?
                {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        QueryPlan::IndexOr { plans } => {
            for plan in plans {
                if !visit_plan_documents(plan, txn, collection, accept)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        QueryPlan::IndexAnd { plans } if !plans.is_empty() => {
            match super::index_filter::scalar_range_plan(plan, txn, collection)? {
                Some(narrowed) => visit_plan_documents(&narrowed, txn, collection, accept),
                None => visit_plan_documents(&plans[0], txn, collection, accept),
            }
        }
        _ => {
            let docs = docs_table_name(collection);
            let mut scan = |table: &str, start: Bound<&[u8]>, end: Bound<&[u8]>, indexed: bool| {
                let mut more = true;
                txn.scan(table, start, end, &mut |key, value| {
                    let bytes = if indexed {
                        ulid_from_index_key(key)
                            .map(|id| txn.get(&docs, &id.to_bytes()))
                            .transpose()?
                            .flatten()
                    } else {
                        None
                    };
                    if !indexed {
                        more = accept(value)?;
                    } else if let Some(bytes) = bytes {
                        more = accept(&bytes)?;
                    }
                    Ok(if more {
                        ScanFlow::Continue
                    } else {
                        ScanFlow::Stop
                    })
                })?;
                Ok(more)
            };
            match plan {
                QueryPlan::IndexEq { field, start, end } => scan(
                    &index_table_name(collection, field),
                    Bound::Included(start),
                    Bound::Included(end),
                    true,
                ),
                QueryPlan::IndexRange { field, start, end } => scan(
                    &index_table_name(collection, field),
                    start.as_ref().map(Vec::as_slice),
                    end.as_ref().map(Vec::as_slice),
                    true,
                ),
                QueryPlan::IndexIn { field, ranges } => {
                    let table = index_table_name(collection, field);
                    for (start, end) in ranges {
                        if !scan(&table, Bound::Included(start), Bound::Included(end), true)? {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                }
                QueryPlan::CompoundIndexEq { fields, start, end } => {
                    let table = crate::index::compound_table_name(
                        collection,
                        &fields.iter().map(String::as_str).collect::<Vec<_>>(),
                    );
                    scan(&table, Bound::Included(start), Bound::Included(end), true)
                }
                _ => scan(&docs, Bound::Unbounded, Bound::Unbounded, false),
            }
        }
    }
}

// Posting lists are unique per token and document. Bounded previews choose a
// short token list; projected document checks enforce every remaining predicate.
fn visit_fts_documents(
    txn: &dyn ReadTxn,
    collection: &str,
    field: &str,
    tokens: &[String],
    accept: &mut impl FnMut(&[u8]) -> Result<bool, TalaDbError>,
) -> Result<bool, TalaDbError> {
    let Some(mut seed) = tokens.first() else {
        return Ok(true);
    };
    let table = fts_table_name(collection, field);
    if tokens.len() > 1 {
        let mut smallest = usize::MAX;
        for token in tokens {
            let (start, end) = fts_token_range(token);
            let mut count = 0;
            txn.scan(
                &table,
                Bound::Included(&start),
                Bound::Included(&end),
                &mut |_, _| {
                    count += 1;
                    Ok(if count >= 64 {
                        ScanFlow::Stop
                    } else {
                        ScanFlow::Continue
                    })
                },
            )?;
            if count == 0 {
                return Ok(true);
            }
            if count < smallest {
                smallest = count;
                seed = token;
            }
        }
    }
    let (start, end) = fts_token_range(seed);
    let docs = docs_table_name(collection);
    let mut more = true;
    txn.scan(
        &table,
        Bound::Included(&start),
        Bound::Included(&end),
        &mut |key, _| {
            if let Some(id) = ulid_from_fts_key(key)
                && let Some(bytes) = txn.get(&docs, &id.to_bytes())?
                && !accept(&bytes)?
            {
                more = false;
                return Ok(ScanFlow::Stop);
            }
            Ok(ScanFlow::Continue)
        },
    )?;
    Ok(more)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Value {
        Value::Str(v.into())
    }

    /// Documents covering every value variant, nesting, arrays, missing
    /// versus null, mixed numbers, NaN, signed zero, bytes and large payloads.
    fn corpus() -> Vec<Document> {
        (0..120u32)
            .map(|i| {
                let mut fields = vec![("i".to_string(), Value::Int(i64::from(i)))];
                let n = match i % 9 {
                    0 => Some(Value::Int(3)),
                    1 => Some(Value::Float(3.0)),
                    2 => Some(Value::Float(3.5)),
                    3 => Some(Value::Float(f64::NAN)),
                    4 => Some(Value::Float(-0.0)),
                    5 => Some(Value::Int(0)),
                    6 => Some(s("3")),
                    7 => Some(Value::Null),
                    _ => None,
                };
                if let Some(n) = n {
                    fields.push(("n".into(), n));
                }
                if i % 5 != 0 {
                    fields.push((
                        "s".into(),
                        s(["a", "a\0b", "ünï", "", "zz"][i as usize % 5]),
                    ));
                }
                if i % 4 == 0 {
                    fields.push(("b".into(), Value::Bytes(vec![i as u8 % 3, 0xFF])));
                }
                fields.push((
                    "tags".into(),
                    match i % 6 {
                        0 => Value::Array(vec![s("rust"), s("db")]),
                        1 => s("rust"),
                        2 => Value::Array(vec![Value::Array(vec![s("rust")]), Value::Int(1)]),
                        3 => Value::Array(vec![]),
                        4 => Value::Null,
                        _ => Value::Array(vec![Value::Int(3), Value::Float(0.0)]),
                    },
                ));
                if i % 7 != 3 {
                    fields.push((
                        "meta".into(),
                        if i % 7 == 6 {
                            Value::Int(1)
                        } else {
                            Value::Object(vec![
                                ("tag".into(), s(if i % 2 == 0 { "yes" } else { "no" })),
                                (
                                    "depth".into(),
                                    Value::Object(vec![("x".into(), Value::Int(i64::from(i % 4)))]),
                                ),
                            ])
                        },
                    ));
                }
                fields.push((
                    "body".into(),
                    s(&format!("rust database storage token{}", i % 5)),
                ));
                fields.push(("flag".into(), Value::Bool(i % 2 == 0)));
                fields.push((
                    "payload".into(),
                    s(&"x".repeat(if i % 40 == 0 { 70_000 } else { 1_024 })),
                ));
                fields.push((
                    "vec".into(),
                    Value::Array((0..64).map(|j| Value::Float(f64::from(j))).collect()),
                ));
                Document::with_id(Ulid::from(u128::from(i) + 1), fields)
            })
            .collect()
    }

    fn filters() -> Vec<Filter> {
        let f = |v: f64| Value::Float(v);
        let mut out = vec![Filter::All];
        for v in [
            Value::Int(3),
            f(3.0),
            f(3.5),
            f(f64::NAN),
            f(0.0),
            f(-0.0),
            s("3"),
            Value::Null,
        ] {
            for make in [
                Filter::Eq as fn(String, Value) -> Filter,
                Filter::Ne,
                Filter::Gt,
                Filter::Gte,
                Filter::Lt,
                Filter::Lte,
            ] {
                out.push(make("n".into(), v.clone()));
            }
        }
        out.extend([
            Filter::Eq("s".into(), s("a")),
            Filter::Gt("s".into(), s("a")),
            Filter::Lte("s".into(), s("a\0b")),
            Filter::In("s".into(), vec![s(""), s("zz"), Value::Null]),
            Filter::Nin("n".into(), vec![Value::Int(3), f(f64::NAN)]),
            Filter::Exists("n".into(), true),
            Filter::Exists("n".into(), false),
            Filter::Eq("n".into(), Value::Null),
            Filter::Exists("zz".into(), false),
            Filter::Eq("tags".into(), s("rust")),
            Filter::Eq("tags".into(), Value::Array(vec![s("rust"), s("db")])),
            Filter::Ne("tags".into(), s("rust")),
            Filter::In("tags".into(), vec![s("db"), Value::Int(3)]),
            Filter::Eq("tags".into(), f(-0.0)),
            Filter::Eq("meta.tag".into(), s("yes")),
            Filter::Gte("meta.depth.x".into(), Value::Int(2)),
            Filter::Exists("meta.tag".into(), false),
            Filter::Exists("meta.depth.y".into(), false),
            Filter::Eq("b".into(), Value::Bytes(vec![1, 0xFF])),
            Filter::Gt("b".into(), Value::Bytes(vec![0])),
            Filter::Eq("flag".into(), Value::Bool(true)),
            Filter::And(vec![
                Filter::Eq("flag".into(), Value::Bool(true)),
                Filter::Lt("i".into(), Value::Int(60)),
            ]),
            Filter::Or(vec![
                Filter::Eq("meta.tag".into(), s("no")),
                Filter::Exists("b".into(), true),
            ]),
            Filter::Not(Box::new(Filter::Exists("s".into(), true))),
            Filter::Not(Box::new(Filter::Or(vec![
                Filter::Eq("n".into(), Value::Int(3)),
                Filter::Eq("zz".into(), Value::Null),
            ]))),
            Filter::Regex("s".into(), "^a".into()),
            Filter::Regex("tags".into(), "^ru".into()),
            Filter::Not(Box::new(Filter::Regex("meta.tag".into(), "y".into()))),
            Filter::Contains("body".into(), "rust token3".into()),
            Filter::Contains("body".into(), String::new()),
            Filter::Contains("zz".into(), "rust".into()),
            Filter::Eq("_id".into(), s(&Ulid::from(6u128).to_string())),
            Filter::Ne("_id".into(), s(&Ulid::from(6u128).to_string())),
            Filter::In("_id".into(), vec![s(&Ulid::from(9u128).to_string())]),
            Filter::Exists("_id".into(), false),
            Filter::Eq("payload".into(), s(&"x".repeat(1_024))),
            Filter::Lt("i".into(), Value::Int(1)),
        ]);
        out
    }

    /// The full scan before projection: decode whole, then test.
    fn oracle(bytes: &[u8], matcher: &Matcher<'_>) -> Result<Option<Vec<u8>>, String> {
        let doc: Document =
            postcard::from_bytes(bytes).map_err(|e| TalaDbError::from(e).to_string())?;
        Ok(matcher
            .matches(&doc)
            .map_err(|e| e.to_string())?
            .then(|| postcard::to_allocvec(&doc).unwrap()))
    }

    fn projected(
        bytes: &[u8],
        filter: &Filter,
        matcher: &Matcher<'_>,
    ) -> Result<Option<Vec<u8>>, String> {
        ScanDecoder::new(filter)
            .projected(bytes, matcher)
            .map(|doc| doc.map(|d| postcard::to_allocvec(&d).unwrap()))
            .map_err(|e| e.to_string())
    }

    #[test]
    fn projected_decision_and_document_match_the_full_decode_for_every_filter() {
        let docs: Vec<Vec<u8>> = corpus()
            .iter()
            .map(|d| postcard::to_allocvec(d).unwrap())
            .collect();
        let filters = filters();
        let mut split = 0;
        for filter in &filters {
            let matcher = Matcher::new(filter).unwrap();
            let mut matched = 0;
            for bytes in &docs {
                let expected = oracle(bytes, &matcher).unwrap();
                matched += usize::from(expected.is_some());
                assert_eq!(
                    projected(bytes, filter, &matcher).unwrap(),
                    expected,
                    "{filter:?}"
                );
            }
            split += usize::from(matched > 0 && matched < docs.len());
        }
        // A filter that matches all or nothing proves little about a
        // projection; most of these split the corpus.
        assert!(
            split * 3 > filters.len() * 2,
            "{split} of {}",
            filters.len()
        );
    }

    #[test]
    fn corrupt_encodings_fail_or_succeed_exactly_as_the_full_decode_does() {
        let doc = &corpus()[12];
        let mut small = doc.clone();
        small.fields.retain(|(k, _)| k != "payload" && k != "vec");
        small.fields.push(("payload".into(), s("xyz")));
        small
            .fields
            .push(("vec".into(), Value::Array(vec![Value::Float(1.0)])));
        let good = postcard::to_allocvec(&small).unwrap();
        let mut variants: Vec<Vec<u8>> = (0..good.len()).map(|n| good[..n].to_vec()).collect();
        for at in 0..good.len() {
            for byte in [0x00, 0x01, 0x02, 0x08, 0x09, 0x7F, 0x80, 0xC3, 0xFF] {
                let mut v = good.clone();
                v[at] = byte;
                variants.push(v);
            }
        }
        let mut trailing = good;
        trailing.extend_from_slice(&[0xFF, 0x00]);
        variants.push(trailing);

        let mut errors = 0;
        for filter in filters() {
            let fields = super::super::filter_document::fields(&filter);
            let matcher = Matcher::new(&filter).unwrap();
            for v in &variants {
                let expected = oracle(v, &matcher);
                errors += usize::from(expected.is_err());
                assert_eq!(
                    projected(v, &filter, &matcher),
                    expected,
                    "{filter:?} {v:?}"
                );
                // `count` and the ID collectors decode the projection only.
                assert_eq!(
                    super::super::filter_document::decode(v, &fields).is_ok(),
                    expected.is_ok(),
                    "{filter:?} {v:?}"
                );
            }
        }
        assert!(errors > 0);
    }

    #[test]
    fn decoder_projects_selective_scans_and_decodes_whole_when_matches_are_dense() {
        let docs: Vec<Vec<u8>> = corpus()
            .iter()
            .map(|d| postcard::to_allocvec(d).unwrap())
            .collect();
        let run = |filter: &Filter, windows: usize, matching: &dyn Fn(usize) -> bool| {
            let matcher = Matcher::new(filter).unwrap();
            let mut decoder = ScanDecoder::new(filter);
            let mut modes = Vec::new();
            for k in 0..windows * DECODE_WINDOW as usize {
                let bytes = &docs[k % docs.len()];
                let hit = if decoder.project {
                    decoder.projected(bytes, &matcher).unwrap().is_some()
                } else {
                    let doc: Document = postcard::from_bytes(bytes).unwrap();
                    matcher.matches(&doc).unwrap()
                };
                assert_eq!(hit, matching(k), "{filter:?} at {k}");
                decoder.tick(hit);
                if (k + 1) % DECODE_WINDOW as usize == 0 {
                    modes.push(decoder.project);
                }
            }
            modes
        };
        // No match: wide documents keep projecting.
        let none = Filter::Lt("i".into(), Value::Int(-1));
        assert!(run(&none, 20, &|_| false).iter().all(|p| *p));
        // Every document matches: whole decoding from the first decision on.
        let all = Filter::Gte("i".into(), Value::Int(0));
        assert!(run(&all, 20, &|_| true).iter().all(|p| !*p));
        // A filter that reads every field skips nothing, so projection never
        // pays, whatever the matches.
        let every = Filter::Or(
            [
                "i", "n", "s", "b", "tags", "meta", "body", "flag", "payload", "vec",
            ]
            .map(|f| Filter::Eq(f.into(), s("never")))
            .to_vec(),
        );
        assert!(run(&every, 20, &|_| false).iter().all(|p| !*p));
        // Matches arrive, then stop: whole decoding while they last, then
        // projection again.
        let late = Filter::Gte("i".into(), Value::Int(0));
        let modes = run(&late, 4, &|_| true);
        assert_eq!(modes, [false; 4]);
        let mut decoder = ScanDecoder::new(&late);
        let matcher = Matcher::new(&late).unwrap();
        for _ in 0..DECODE_WINDOW {
            decoder.projected(&docs[1], &matcher).unwrap();
            decoder.tick(true);
        }
        assert!(!decoder.project);
        let mut back = None;
        for w in 0..64 {
            for _ in 0..DECODE_WINDOW {
                decoder.tick(false);
            }
            if decoder.project {
                back = Some(w);
                break;
            }
        }
        assert!(back.is_some_and(|w| w < 32), "{back:?}");
    }
}
