//! Sorted pages served from a secondary index without materialising it.
//!
//! An index key is `encode_value_prefix(value) ++ ulid`, so a forward scan
//! yields `(value asc, id asc)`. Once [`index_order_is_sort_order`] has shown
//! that the encoding agrees with `cmp_values` on every key of the index, the
//! rows of a sort on that field are the index entries, ranked as follows:
//!
//! * runs of equal values (equal value prefixes) are ordered by value, in the
//!   sort's direction;
//! * inside a run, a single-key sort orders by `_id` ascending in **both**
//!   directions (`cmp_rows` breaks ties on the id without reversing it), which
//!   is the run's key order;
//! * inside a run, a multi-key sort orders by the later keys, which only the
//!   documents can tell.
//!
//! One encoding quirk breaks "key order is `(value, id)` order": a Str/Bytes
//! key whose id starts with 0xFF can sort after a longer value that extends it
//! with an escaped NUL (see `crate::index`). [`ids_can_reorder_strings`] rules
//! that out with two probes for nearly every collection; otherwise the walks
//! give up at the first key whose value contains NUL.
//!
//! An ascending page therefore stops the walk at rank `keep`. A descending page
//! still walks every key — the internal read API iterates forward only — but
//! keeps only the trailing runs that can reach the page. Either way the memory
//! held is bounded by `keep`, never by the collection or by a run of ties.

use std::collections::VecDeque;
use std::ops::Bound;

use ulid::Ulid;

use crate::document::Document;
use crate::engine::{ReadTxn, ScanFlow};
use crate::error::TalaDbError;
use crate::index::{
    TAG_BYTES, TAG_FLOAT, TAG_INT, TAG_STR, docs_table_name, has_escaped_nul, index_table_name,
    ulid_from_index_key,
};
use crate::query::executor::fetch_by_ulids;
use crate::query::options::{SortDirection, SortSpec, partial_sort_documents};

/// Documents decoded per step when a run must be ordered by later sort keys.
const RUN_CHUNK: usize = 256;

/// The candidates for a sorted page.
pub(crate) struct SortedPage {
    /// Every row of rank `skipped..keep` in the sort's order, plus possibly a
    /// few more of the same runs, in no particular order: the caller still
    /// orders them on the full sort specification.
    pub docs: Vec<Document>,
    /// Rows of rank below this were not decoded. Always at most the requested
    /// `skip`.
    pub skipped: usize,
}

/// Whether ordering a field's index keys gives the sort comparator's order for
/// every document of the collection. Reads a constant number of keys.
///
/// The caller has already checked the field's array counter is zero, so each
/// document has at most one key. Then:
///
/// * the index has an entry for every document exactly when the two tables
///   have the same length (redb keeps the length in the table header);
/// * `TAG_INT` sorts before `TAG_FLOAT`, while `cmp_values` compares the two
///   numerically, so a field holding both is out;
/// * `cmp_values` treats NaN as equal to everything and `-0.0` as equal to
///   `0.0`, while their keys differ, so any float key in those ranges is out.
pub(crate) fn index_order_is_sort_order(
    txn: &dyn ReadTxn,
    collection: &str,
    field: &str,
) -> Result<bool, TalaDbError> {
    let table = index_table_name(collection, field);
    if txn.count_entries(&table)? != txn.count_entries(&docs_table_name(collection))? {
        return Ok(false);
    }
    let has_int = any_key(
        txn,
        &table,
        Bound::Included(&[TAG_INT]),
        Bound::Excluded(&[TAG_INT + 1]),
    )?;
    let has_float = any_key(
        txn,
        &table,
        Bound::Included(&[TAG_FLOAT]),
        Bound::Excluded(&[TAG_FLOAT + 1]),
    )?;
    if has_int && has_float {
        return Ok(false);
    }
    if has_float {
        for (start, end) in float_ranges_out_of_order() {
            if any_key(
                txn,
                &table,
                Bound::Included(&start),
                end.as_ref().map(Vec::as_slice),
            )? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Key ranges holding every float key whose value `cmp_values` does not order
/// like its key: negative NaNs (below `-inf`), `-0.0` and `0.0` (adjacent), and
/// positive NaNs (above `+inf`). Bounds are value prefixes, so each range
/// covers every id.
fn float_ranges_out_of_order() -> [(Vec<u8>, Bound<Vec<u8>>); 3] {
    let key = |sortable: u64| {
        let mut k = vec![TAG_FLOAT];
        k.extend_from_slice(&sortable.to_be_bytes());
        k
    };
    let neg_inf = !f64::NEG_INFINITY.to_bits();
    let neg_zero = !(-0.0f64).to_bits();
    let pos_zero = 0.0f64.to_bits() ^ (1 << 63);
    let pos_inf = f64::INFINITY.to_bits() ^ (1 << 63);
    [
        (key(0), Bound::Excluded(key(neg_inf))),
        (key(neg_zero), Bound::Excluded(key(pos_zero + 1))),
        (key(pos_inf + 1), Bound::Excluded(vec![TAG_FLOAT + 1])),
    ]
}

/// Whether some key of the index may sort out of value order.
///
/// A Str/Bytes key followed by an id starting 0xFF sorts *after* the keys of a
/// longer value that extends it with an escaped NUL, when that value's encoding
/// is smaller than the id: `"a"` + `FF 70 …` comes after `"a\0b"`. That needs an
/// id starting 0xFF — generated ids reach one only after the year 10889, but
/// caller-supplied and `derive_doc_id` ones can — and a Str/Bytes key. A probe
/// for each rules it out for nearly every collection.
///
/// When neither can be ruled out, a walk gives up (and the query scans) at the
/// first key whose value contains NUL. Every out-of-order pair has such a key,
/// and it is the one that comes first in key order, so a walk that has met
/// none has seen exactly the keys, in exactly the order, that value order
/// gives it — even one that stopped early.
fn ids_can_reorder_strings(
    txn: &dyn ReadTxn,
    collection: &str,
    table: &str,
) -> Result<bool, TalaDbError> {
    Ok(any_key(
        txn,
        &docs_table_name(collection),
        Bound::Included(&[0xFF]),
        Bound::Unbounded,
    )? && any_key(
        txn,
        table,
        Bound::Included(&[TAG_STR]),
        Bound::Excluded(&[TAG_BYTES + 1]),
    )?)
}

fn any_key(
    txn: &dyn ReadTxn,
    table: &str,
    start: Bound<&[u8]>,
    end: Bound<&[u8]>,
) -> Result<bool, TalaDbError> {
    let mut found = false;
    txn.scan(table, start, end, &mut |_, _| {
        found = true;
        Ok(ScanFlow::Stop)
    })?;
    Ok(found)
}

/// One run of equal values, in index order.
#[derive(Default)]
struct Run {
    prefix: Vec<u8>,
    /// The run's first ids, at most `keep` of them.
    ids: Vec<Ulid>,
    /// Entries seen in the run. More than `ids.len()` when the cap was reached
    /// or (ascending) when the walk stopped inside the run.
    size: usize,
    /// Whether every entry of the run was seen.
    complete: bool,
}

/// The rows of rank `0..keep` for `sort` (whose first key is the indexed field),
/// leaving out whole runs, or single-key rows, of rank below `skip` without
/// decoding them. The index must satisfy [`index_order_is_sort_order`].
///
/// Returns `None` if an index key is malformed, or may be out of order (see
/// [`ids_can_reorder_strings`]); the caller then scans and sorts.
pub(crate) fn sorted_page(
    txn: &dyn ReadTxn,
    collection: &str,
    sort: &[SortSpec],
    skip: usize,
    keep: usize,
) -> Result<Option<SortedPage>, TalaDbError> {
    let Some(first) = sort.first() else {
        return Ok(None);
    };
    if skip >= keep {
        return Ok(Some(SortedPage {
            docs: Vec::new(),
            skipped: 0,
        }));
    }
    let table = index_table_name(collection, &first.field);
    let strict = ids_can_reorder_strings(txn, collection, &table)?;
    if sort.len() == 1 && first.direction == SortDirection::Asc {
        // Key order is the sort order: the page is the keys of rank
        // `skip..keep`, and the walk ends there.
        let Some((ids, seen)) = ascending_ids(txn, &table, skip, keep, strict)? else {
            return Ok(None);
        };
        return Ok(Some(SortedPage {
            docs: fetch_by_ulids(txn, collection, &ids)?,
            skipped: skip.min(seen),
        }));
    }
    let multi_key = sort.len() > 1;
    let runs = match first.direction {
        SortDirection::Asc => ascending_runs(txn, &table, keep, strict)?,
        SortDirection::Desc => {
            let Some(window) = DescendingWindow::walk(txn, &table, keep, multi_key, strict)? else {
                return Ok(None);
            };
            if !multi_key {
                // Rows are the window's ids in order; the boundary run holds
                // its lowest ids, which the `_id` tie-break ranks first.
                let ids = window.ranked_ids(keep);
                let skipped = skip.min(ids.len());
                return Ok(Some(SortedPage {
                    docs: fetch_by_ulids(txn, collection, &ids[skipped..])?,
                    skipped,
                }));
            }
            Some(window.into_runs())
        }
    };
    let Some(runs) = runs else {
        return Ok(None);
    };

    // Multi-key: later keys order rows only inside a run. Runs wholly below
    // `skip` are dropped; the rest are decoded, a run that may exceed what the
    // page needs through a bounded selection over its key range.
    let mut docs = Vec::new();
    let mut skipped = 0;
    let mut start = 0;
    for run in runs {
        if start >= keep {
            break;
        }
        let end = start + run.size;
        if run.complete && end <= skip {
            skipped = end;
        } else if run.complete && run.size == run.ids.len() {
            docs.extend(fetch_by_ulids(txn, collection, &run.ids)?);
        } else {
            let Some(best) = best_of_run(txn, &table, collection, sort, &run.prefix, keep - start)?
            else {
                return Ok(None);
            };
            docs.extend(best);
        }
        start = end;
    }
    Ok(Some(SortedPage { docs, skipped }))
}

/// Ids of rank `skip..keep` in key order, and how many keys were walked.
fn ascending_ids(
    txn: &dyn ReadTxn,
    table: &str,
    skip: usize,
    keep: usize,
    strict: bool,
) -> Result<Option<(Vec<Ulid>, usize)>, TalaDbError> {
    let mut ids = Vec::new();
    let mut seen = 0;
    let mut malformed = false;
    txn.scan(table, Bound::Unbounded, Bound::Unbounded, &mut |k, _| {
        let Some((_, id)) = split_key(k, strict) else {
            malformed = true;
            return Ok(ScanFlow::Stop);
        };
        if seen >= skip {
            ids.push(id);
        }
        seen += 1;
        Ok(if seen == keep {
            ScanFlow::Stop
        } else {
            ScanFlow::Continue
        })
    })?;
    Ok((!malformed).then_some((ids, seen)))
}

/// Forward walk that stops at rank `keep`. The last run may be incomplete.
fn ascending_runs(
    txn: &dyn ReadTxn,
    table: &str,
    keep: usize,
    strict: bool,
) -> Result<Option<Vec<Run>>, TalaDbError> {
    let mut runs: Vec<Run> = Vec::new();
    let mut seen = 0;
    let mut malformed = false;
    txn.scan(table, Bound::Unbounded, Bound::Unbounded, &mut |k, _| {
        let Some((prefix, id)) = split_key(k, strict) else {
            malformed = true;
            return Ok(ScanFlow::Stop);
        };
        if runs.last().is_none_or(|r| r.prefix != prefix) {
            if let Some(r) = runs.last_mut() {
                r.complete = true;
            }
            runs.push(Run {
                prefix: prefix.to_vec(),
                ..Run::default()
            });
        }
        let run = runs.last_mut().expect("pushed above");
        run.ids.push(id);
        run.size += 1;
        seen += 1;
        Ok(if seen == keep {
            ScanFlow::Stop
        } else {
            ScanFlow::Continue
        })
    })?;
    if malformed {
        return Ok(None);
    }
    // The walk ended with the index, so every run is whole.
    if seen < keep
        && let Some(r) = runs.last_mut()
    {
        r.complete = true;
    }
    Ok(Some(runs))
}

/// One run held by [`DescendingWindow`].
struct HeldRun {
    /// Ids of the run held in the window's `ids`: its first ones, at most `keep`.
    held: usize,
    /// Keys in the run.
    size: usize,
    /// Bytes of its value prefix in the window's `prefixes` (zero when they
    /// are not kept).
    prefix_len: usize,
}

/// The trailing runs of an index able to reach rank `keep` of a descending
/// order, lowest value first.
///
/// Invariant: either one run is held, or the runs after the first (lowest) hold
/// fewer than `keep` rows between them, so the first run is the page's boundary
/// and every later run lies wholly on the page. Runs, ids and prefixes are flat
/// queues, so a key that starts a run and retires another costs a few pushes
/// and pops, not an allocation.
#[derive(Default)]
struct DescendingWindow {
    runs: VecDeque<HeldRun>,
    ids: VecDeque<Ulid>,
    prefixes: VecDeque<u8>,
}

impl DescendingWindow {
    /// Walk the whole index forward. `with_prefixes` keeps each held run's value
    /// prefix, which only a multi-key sort needs afterwards.
    fn walk(
        txn: &dyn ReadTxn,
        table: &str,
        keep: usize,
        with_prefixes: bool,
        strict: bool,
    ) -> Result<Option<Self>, TalaDbError> {
        let mut w = Self::default();
        // The newest run's prefix, to spot where it ends.
        let mut last: Vec<u8> = Vec::new();
        // Rows held by every run but the first.
        let mut tail = 0usize;
        let mut malformed = false;
        txn.scan(table, Bound::Unbounded, Bound::Unbounded, &mut |k, _| {
            let Some((prefix, id)) = split_key(k, strict) else {
                malformed = true;
                return Ok(ScanFlow::Stop);
            };
            if w.runs.is_empty() || last.as_slice() != prefix {
                last.clear();
                last.extend_from_slice(prefix);
                if with_prefixes {
                    w.prefixes.extend(prefix.iter().copied());
                }
                w.runs.push_back(HeldRun {
                    held: 0,
                    size: 0,
                    prefix_len: if with_prefixes { prefix.len() } else { 0 },
                });
            }
            let held_runs = w.runs.len();
            let run = w.runs.back_mut().expect("pushed above");
            run.size += 1;
            if run.held < keep {
                run.held += 1;
                w.ids.push_back(id);
                if held_runs > 1 {
                    tail += 1;
                }
            }
            while w.runs.len() > 1 && tail >= keep {
                let dropped = w.runs.pop_front().expect("more than one run");
                w.ids.drain(..dropped.held);
                w.prefixes.drain(..dropped.prefix_len);
                tail -= w.runs.front().expect("more than one run").held;
            }
            Ok(ScanFlow::Continue)
        })?;
        Ok((!malformed).then_some(w))
    }

    /// Ids in the sort's order — highest run first, ids ascending inside a run
    /// — up to `keep` of them.
    fn ranked_ids(mut self, keep: usize) -> Vec<Ulid> {
        let ids = self.ids.make_contiguous();
        let mut out = Vec::with_capacity(ids.len().min(keep));
        let mut end = ids.len();
        for run in self.runs.iter().rev() {
            let start = end - run.held;
            out.extend_from_slice(&ids[start..end]);
            end = start;
        }
        out.truncate(keep);
        out
    }

    /// The held runs, highest value first.
    fn into_runs(self) -> Vec<Run> {
        let mut ids = self.ids.into_iter();
        let mut prefixes = self.prefixes.into_iter();
        let mut runs: Vec<Run> = self
            .runs
            .into_iter()
            .map(|r| Run {
                prefix: prefixes.by_ref().take(r.prefix_len).collect(),
                ids: ids.by_ref().take(r.held).collect(),
                size: r.size,
                complete: true,
            })
            .collect();
        runs.reverse();
        runs
    }
}

/// The `need` best documents of one run under `sort`, reading the run's keys in
/// steps of [`RUN_CHUNK`] and holding at most `need + RUN_CHUNK` documents.
/// Every document of the run is decoded once.
fn best_of_run(
    txn: &dyn ReadTxn,
    table: &str,
    collection: &str,
    sort: &[SortSpec],
    prefix: &[u8],
    need: usize,
) -> Result<Option<Vec<Document>>, TalaDbError> {
    let mut end = prefix.to_vec();
    end.extend_from_slice(&[0xFF; 16]);
    let mut from = prefix.to_vec();
    from.extend_from_slice(&[0x00; 16]);
    let mut first = true;
    let mut best: Vec<Document> = Vec::new();
    let mut ids = Vec::with_capacity(RUN_CHUNK);
    loop {
        ids.clear();
        let mut malformed = false;
        let start = if first {
            Bound::Included(from.as_slice())
        } else {
            Bound::Excluded(from.as_slice())
        };
        txn.scan(table, start, Bound::Included(&end), &mut |k, _| {
            match split_key(k, false) {
                Some((p, id)) if p == prefix => ids.push(id),
                // Another value whose encoding extends this prefix (a string
                // with an escaped NUL) can sort inside the range: not ours.
                Some(_) => {}
                None => {
                    malformed = true;
                    return Ok(ScanFlow::Stop);
                }
            }
            Ok(if ids.len() == RUN_CHUNK {
                ScanFlow::Stop
            } else {
                ScanFlow::Continue
            })
        })?;
        if malformed {
            return Ok(None);
        }
        best.extend(fetch_by_ulids(txn, collection, &ids)?);
        if best.len() > need {
            partial_sort_documents(&mut best, sort, need);
        }
        if ids.len() < RUN_CHUNK {
            return Ok(Some(best));
        }
        first = false;
        from.truncate(prefix.len());
        from.extend_from_slice(&ids[ids.len() - 1].to_bytes());
    }
}

/// `(value prefix, id)` of an index key; `None` if it is malformed or, when
/// `strict`, holds a value containing NUL (see [`ids_can_reorder_strings`]).
fn split_key(key: &[u8], strict: bool) -> Option<(&[u8], Ulid)> {
    let split = key.len().checked_sub(16)?;
    let prefix = &key[..split];
    if strict && has_escaped_nul(prefix) {
        return None;
    }
    Some((prefix, ulid_from_index_key(key)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Value;
    use crate::index::encode_index_key;

    fn in_range(key: &[u8], start: &[u8], end: &Bound<Vec<u8>>) -> bool {
        key >= start
            && match end {
                Bound::Excluded(e) => key < e.as_slice(),
                Bound::Included(e) => key <= e.as_slice(),
                Bound::Unbounded => true,
            }
    }

    /// The probe ranges hold exactly the floats `cmp_values` cannot order by
    /// key: every NaN payload of either sign, and both zeros.
    #[test]
    fn probe_ranges_hold_exactly_nan_and_signed_zero() {
        let ranges = float_ranges_out_of_order();
        let id = Ulid::from(u128::MAX);
        let low = Ulid::from(0u128);
        let mut cases: Vec<(f64, bool)> = vec![
            (f64::NAN, true),
            (-f64::NAN, true),
            (f64::from_bits(0x7FF0_0000_0000_0001), true),
            (f64::from_bits(0x7FFF_FFFF_FFFF_FFFF), true),
            (f64::from_bits(0xFFF0_0000_0000_0001), true),
            (f64::from_bits(0xFFFF_FFFF_FFFF_FFFF), true),
            (0.0, true),
            (-0.0, true),
            (f64::INFINITY, false),
            (f64::NEG_INFINITY, false),
            (f64::MAX, false),
            (f64::MIN, false),
            (f64::MIN_POSITIVE, false),
            (-f64::MIN_POSITIVE, false),
            (f64::from_bits(1), false),
            (-f64::from_bits(1), false),
            (1.0, false),
            (-1.0, false),
        ];
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..10_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let f = f64::from_bits(state);
            cases.push((f, f.is_nan() || f == 0.0));
        }
        for (f, out_of_order) in cases {
            for id in [id, low] {
                let key = encode_index_key(&Value::Float(f), id).unwrap();
                let hit = ranges.iter().any(|(s, e)| in_range(&key, s, e));
                assert_eq!(hit, out_of_order, "{f:?} ({:#018x})", f.to_bits());
            }
        }
    }
}
