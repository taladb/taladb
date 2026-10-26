//! Retain the globally best page window subject to per-group quotas. Groups
//! outside that window need no state: the admission score only increases.
use crate::vector::Candidate;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};

pub(super) enum Window {
    Plain {
        limit: usize,
        best: BinaryHeap<Reverse<Candidate>>,
    },
    Grouped(GroupWindow),
}
pub(super) struct GroupWindow {
    limit: usize,
    group_size: usize,
    best: BTreeMap<Candidate, Vec<u8>>,
    groups: HashMap<Vec<u8>, BTreeSet<Candidate>>,
}
impl Window {
    pub(super) fn new(limit: usize, group_size: Option<usize>) -> Self {
        if let Some(group_size) = group_size {
            Self::Grouped(GroupWindow {
                limit,
                group_size,
                best: BTreeMap::new(),
                groups: HashMap::new(),
            })
        } else {
            Self::Plain {
                limit,
                best: BinaryHeap::new(),
            }
        }
    }
    pub(super) fn competitive(&self, candidate: Candidate) -> bool {
        match self {
            Self::Plain { limit, best } => {
                best.len() < *limit || best.peek().is_some_and(|worst| candidate > worst.0)
            }
            Self::Grouped(g) => {
                g.best.len() < g.limit
                    || g.best
                        .first_key_value()
                        .is_some_and(|(worst, _)| candidate > *worst)
            }
        }
    }
    pub(super) fn offer(&mut self, candidate: Candidate, key: Vec<u8>) {
        if !self.competitive(candidate) {
            return;
        }
        match self {
            Self::Plain { limit, best } => {
                if best.len() == *limit {
                    best.pop();
                }
                best.push(Reverse(candidate));
            }
            Self::Grouped(g) => {
                if g.best.contains_key(&candidate) {
                    return;
                }
                if let Some(group) = g.groups.get(&key)
                    && group.len() == g.group_size
                {
                    let worst = *group.first().unwrap();
                    if candidate <= worst {
                        return;
                    }
                    g.remove(worst);
                }
                if g.best.len() == g.limit {
                    let worst = *g.best.first_key_value().unwrap().0;
                    g.remove(worst);
                }
                g.groups.entry(key.clone()).or_default().insert(candidate);
                g.best.insert(candidate, key);
            }
        }
    }
    pub(super) fn ranked(self) -> Vec<(ulid::Ulid, f32)> {
        let mut candidates: Vec<_> = match self {
            Self::Plain { best, .. } => best.into_iter().map(|r| r.0).collect(),
            Self::Grouped(g) => g.best.into_keys().collect(),
        };
        candidates.sort_unstable_by(|a, b| b.cmp(a));
        candidates.into_iter().map(|c| (c.0, c.1)).collect()
    }
}
impl GroupWindow {
    fn remove(&mut self, candidate: Candidate) {
        if let Some(key) = self.best.remove(&candidate) {
            let group = self.groups.get_mut(&key).unwrap();
            group.remove(&candidate);
            if group.is_empty() {
                self.groups.remove(&key);
            }
        }
    }
}

pub(super) struct Ranker<'a> {
    window: Window,
    group_by: Option<&'a str>,
    fields: crate::query::filter_document::Fields<'a>,
    threshold: Option<f32>,
}
impl<'a> Ranker<'a> {
    pub(super) fn new(limit: usize, options: &'a super::VectorQueryOptions) -> Self {
        let group_by = options.group_by.as_deref();
        let fields = crate::query::filter_document::Fields::of(
            group_by.map(|f| f.split('.').next().unwrap_or(f)),
        );
        Self {
            window: Window::new(limit, group_by.map(|_| options.group_size.unwrap_or(1))),
            group_by,
            fields,
            threshold: options.score_threshold,
        }
    }
    pub(super) fn offer(
        &mut self,
        collection: &super::Collection,
        txn: &dyn crate::engine::ReadTxn,
        id: ulid::Ulid,
        score: f32,
    ) -> Result<(), crate::TalaDbError> {
        let candidate = Candidate(id, score);
        if !score.is_finite()
            || self.threshold.is_some_and(|t| score < t)
            || !self.window.competitive(candidate)
        {
            return Ok(());
        }
        let key = if let Some(field) = self.group_by {
            let Some(bytes) = txn.get(
                &crate::index::docs_table_name(&collection.name),
                &id.to_bytes(),
            )?
            else {
                return Ok(());
            };
            let mut doc = crate::query::filter_document::decode(&bytes, &self.fields)?;
            collection.decrypt_doc(&mut doc)?;
            postcard::to_allocvec(doc.get(field).unwrap_or(&crate::Value::Null))?
        } else {
            Vec::new()
        };
        self.window.offer(candidate, key);
        Ok(())
    }
    pub(super) fn ranked(self) -> Vec<(ulid::Ulid, f32)> {
        self.window.ranked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn online_quotas_match_full_sort_for_every_order_and_do_not_retain_displaced_groups() {
        for group_size in [1, 2, 7] {
            for limit in [1, 5, 17, 100] {
                for seed in 0..8u64 {
                    let mut rows: Vec<_> = (0..200u128)
                        .map(|i| {
                            (
                                Candidate(ulid::Ulid::from(i), ((i * 31) % 23) as f32),
                                vec![(i % 37) as u8],
                            )
                        })
                        .collect();
                    let mut state = seed + 1;
                    for i in (1..rows.len()).rev() {
                        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                        rows.swap(i, (state as usize) % (i + 1));
                    }
                    let mut expected = rows.clone();
                    expected.sort_unstable_by_key(|a| Reverse(a.0));
                    let mut counts = HashMap::<Vec<u8>, usize>::new();
                    expected.retain(|(_, key)| {
                        let n = counts.entry(key.clone()).or_default();
                        *n += 1;
                        *n <= group_size
                    });
                    expected.truncate(limit);
                    let mut window = Window::new(limit, Some(group_size));
                    for (candidate, key) in rows {
                        window.offer(candidate, key);
                        let Window::Grouped(g) = &window else {
                            unreachable!()
                        };
                        assert!(g.best.len() <= limit && g.groups.len() <= limit);
                        assert_eq!(
                            g.groups.values().map(BTreeSet::len).sum::<usize>(),
                            g.best.len()
                        );
                        assert!(g.groups.values().all(|v| v.len() <= group_size));
                    }
                    assert_eq!(
                        window.ranked(),
                        expected
                            .into_iter()
                            .map(|(c, _)| (c.0, c.1))
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
    }
}
