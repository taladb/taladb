//! Transactional, portable HNSW. Nodes are separate storage records, not a
//! process-wide graph blob. Document writes and link changes share a transaction.
//! Algorithm: Malkov/Yashunin, https://arxiv.org/abs/1603.09320 (algorithms 1–4).
//! Deleted nodes remain traversable; rebuilding compacts tombstones.
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::engine::{ReadTxn, WriteTxn, WriteView};
use crate::error::TalaDbError;
use crate::vector::{VectorMetric, l2_norm, score_with_query_norm};

pub(crate) const META: &str = "meta::vector_graphs";
pub(crate) const BUILDS: &str = "meta::vector_builds";
const FORMAT: u32 = 1;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Quantization {
    #[default]
    None,
    Scalar,
    Binary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct GraphOptions {
    pub m: u32,
    #[serde(alias = "ef_construction")]
    pub ef_construction: u32,
    pub quantization: Quantization,
}
impl Default for GraphOptions {
    fn default() -> Self {
        Self {
            m: 32,
            ef_construction: 200,
            quantization: Quantization::None,
        }
    }
}
impl GraphOptions {
    pub fn validate(&self, metric: VectorMetric) -> Result<(), TalaDbError> {
        if !(2..=128).contains(&self.m)
            || self.ef_construction < self.m
            || self.ef_construction > 100_000
        {
            return Err(invalid(
                "HNSW requires 2 <= m <= 128 and m <= efConstruction <= 100000",
            ));
        }
        if metric == VectorMetric::Dot {
            return Err(invalid(
                "HNSW requires cosine or euclidean; use exact search for dot product",
            ));
        }
        if self.quantization == Quantization::Binary && metric != VectorMetric::Cosine {
            return Err(invalid("binary quantization requires cosine similarity"));
        }
        Ok(())
    }
}
pub(crate) fn invalid(msg: &str) -> TalaDbError {
    TalaDbError::InvalidOperation(msg.into())
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Header {
    pub format: u32,
    pub table: String,
    pub revision: u64,
    pub options: GraphOptions,
    pub dimensions: usize,
    pub metric: VectorMetric,
    pub entry: Option<u64>,
    pub level: usize,
    pub next: u64,
    pub live: u64,
    pub deleted: u64,
}
impl Header {
    pub fn new(
        table: String,
        revision: u64,
        options: GraphOptions,
        dimensions: usize,
        metric: VectorMetric,
    ) -> Self {
        Self {
            format: FORMAT,
            table,
            revision,
            options,
            dimensions,
            metric,
            entry: None,
            level: 0,
            next: 0,
            live: 0,
            deleted: 0,
        }
    }
}
pub(crate) fn header(txn: &dyn ReadTxn, key: &str) -> Result<Option<Header>, TalaDbError> {
    let h: Option<Header> = txn
        .get(META, key.as_bytes())?
        .map(|b| postcard::from_bytes(&b))
        .transpose()?;
    if let Some(h) = &h {
        if h.format != FORMAT {
            return Err(invalid("unsupported HNSW format; rebuild the vector index"));
        }
        h.options.validate(h.metric)?;
        if h.level > 16 || h.dimensions == 0 {
            return Err(invalid("invalid HNSW header"));
        }
    }
    Ok(h)
}
pub(crate) fn save_header(
    txn: &mut dyn WriteTxn,
    key: &str,
    h: &Header,
) -> Result<(), TalaDbError> {
    txn.put(META, key.as_bytes(), &postcard::to_allocvec(h)?)
}

#[derive(Clone, Serialize, Deserialize)]
enum Code {
    Float(Vec<f32>),
    Scalar {
        values: Vec<u8>,
        min: f32,
        step: f32,
    },
    Binary(Vec<u8>),
}
impl Code {
    fn encode(v: &[f32], mode: Quantization) -> Self {
        match mode {
            Quantization::None => Self::Float(v.to_vec()),
            Quantization::Scalar => {
                let min = v.iter().copied().fold(f32::INFINITY, f32::min);
                let max = v.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                // f64 avoids overflow on finite, large f32 inputs.
                let step = ((f64::from(max) - f64::from(min)) / 255.0) as f32;
                let values = v
                    .iter()
                    .map(|x| {
                        if step == 0.0 {
                            0
                        } else {
                            ((f64::from(*x) - f64::from(min)) / f64::from(step))
                                .round()
                                .clamp(0.0, 255.0) as u8
                        }
                    })
                    .collect();
                Self::Scalar { values, min, step }
            }
            Quantization::Binary => {
                let mut bits = vec![0; v.len().div_ceil(8)];
                for (i, x) in v.iter().enumerate() {
                    if *x >= 0.0 {
                        bits[i / 8] |= 1 << (i % 8);
                    }
                }
                Self::Binary(bits)
            }
        }
    }
    fn decode(&self, dimensions: usize) -> Vec<f32> {
        match self {
            Self::Float(v) => v.clone(),
            Self::Scalar { values, min, step } => values
                .iter()
                .map(|x| (f64::from(*min) + f64::from(*step) * f64::from(*x)) as f32)
                .collect(),
            Self::Binary(v) => (0..dimensions)
                .map(|i| {
                    if v[i / 8] & (1 << (i % 8)) != 0 {
                        1.0
                    } else {
                        -1.0
                    }
                })
                .collect(),
        }
    }
    fn valid(&self, d: usize) -> bool {
        match self {
            Self::Float(v) => v.len() == d && v.iter().all(|x| x.is_finite()),
            Self::Scalar { values, min, step } => {
                values.len() == d && min.is_finite() && step.is_finite() && *step >= 0.0
            }
            Self::Binary(v) => v.len() == d.div_ceil(8),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
struct Node {
    doc: [u8; 16],
    code: Code,
    links: Vec<Vec<u64>>,
    deleted: bool,
}
fn node_key(id: u64) -> [u8; 9] {
    let mut key = [1; 9];
    key[1..].copy_from_slice(&id.to_be_bytes());
    key
}
fn map_key(id: &[u8; 16]) -> [u8; 17] {
    let mut key = [2; 17];
    key[1..].copy_from_slice(id);
    key
}

#[derive(Clone, Copy, PartialEq)]
struct Hit(f32, u64);
impl Eq for Hit {}
impl PartialOrd for Hit {
    fn partial_cmp(&self, rhs: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(rhs))
    }
}
impl Ord for Hit {
    fn cmp(&self, rhs: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&rhs.0).then(self.1.cmp(&rhs.1))
    }
}

struct Reader<'a> {
    txn: &'a dyn ReadTxn,
    h: &'a Header,
    nodes: HashMap<u64, Node>,
    distances: usize,
    cache_bytes: usize,
}
impl<'a> Reader<'a> {
    fn new(txn: &'a dyn ReadTxn, h: &'a Header) -> Self {
        Self {
            txn,
            h,
            nodes: HashMap::new(),
            distances: 0,
            cache_bytes: 0,
        }
    }
    fn node(&mut self, id: u64) -> Result<&Node, TalaDbError> {
        if !self.nodes.contains_key(&id) {
            let bytes = self
                .txn
                .get(&self.h.table, &node_key(id))?
                .ok_or_else(|| invalid("missing HNSW node; rebuild the vector index"))?;
            let node: Node = postcard::from_bytes(&bytes)?;
            if node.links.is_empty()
                || node.links.len() > 17
                || !node.code.valid(self.h.dimensions)
                || node
                    .links
                    .iter()
                    .any(|l| l.len() > self.h.options.m as usize * 2)
            {
                return Err(invalid("invalid HNSW node; rebuild the vector index"));
            }
            // Bound retained node records on phones and in WASM. The queue and
            // visited IDs remain lightweight; evicted nodes can be read again.
            if self.cache_bytes.saturating_add(bytes.len()) > 8 * 1024 * 1024 {
                self.nodes.clear();
                self.cache_bytes = 0;
            }
            self.cache_bytes = self.cache_bytes.saturating_add(bytes.len());
            self.nodes.insert(id, node);
        }
        Ok(self.nodes.get(&id).unwrap())
    }
    fn distance_with_norm(
        &mut self,
        query: &[f32],
        query_norm: f32,
        id: u64,
    ) -> Result<Hit, TalaDbError> {
        let dimensions = self.h.dimensions;
        let metric = self.h.metric;
        let node = self.node(id)?;
        let v = node.code.decode(dimensions);
        let score = score_with_query_norm(&metric, query, query_norm, &v);
        self.distances += 1;
        if !score.is_finite() {
            return Err(invalid(
                "vector magnitude overflows HNSW distance; normalize the embedding",
            ));
        }
        Ok(Hit(-score, id))
    }
    fn distance(&mut self, query: &[f32], id: u64) -> Result<Hit, TalaDbError> {
        self.distance_with_norm(query, l2_norm(query), id)
    }
    fn greedy(&mut self, q: &[f32], entry: u64, layer: usize) -> Result<u64, TalaDbError> {
        let query_norm = l2_norm(q);
        let mut best = self.distance_with_norm(q, query_norm, entry)?;
        loop {
            let before = best;
            let links = self
                .node(best.1)?
                .links
                .get(layer)
                .cloned()
                .unwrap_or_default();
            for id in links {
                let h = self.distance_with_norm(q, query_norm, id)?;
                if h < best {
                    best = h;
                }
            }
            if best == before {
                return Ok(best.1);
            }
        }
    }
    // Disallowed/tombstoned nodes are routing bridges, never returned hits.
    // Only eligible hits tighten the stopping bound, so selective filters do
    // not strand traversal at a rejected node. ANN remains approximate.
    fn layer(
        &mut self,
        q: &[f32],
        entries: &[u64],
        layer: usize,
        ef: usize,
        allowed: Option<&HashSet<[u8; 16]>>,
        live_only: bool,
    ) -> Result<Vec<Hit>, TalaDbError> {
        let query_norm = l2_norm(q);
        let mut visited = HashSet::new();
        let mut queue = BinaryHeap::new();
        let mut best = BinaryHeap::new();
        for &id in entries {
            let hit = self.distance_with_norm(q, query_norm, id)?;
            visited.insert(id);
            queue.push(Reverse(hit));
            let n = self.node(id)?;
            if (!live_only || !n.deleted) && allowed.is_none_or(|a| a.contains(&n.doc)) {
                best.push(hit);
            }
        }
        while let Some(Reverse(hit)) = queue.pop() {
            if best.len() >= ef && best.peek().is_some_and(|worst| hit > *worst) {
                break;
            }
            let links = self
                .node(hit.1)?
                .links
                .get(layer)
                .cloned()
                .unwrap_or_default();
            for id in links {
                if !visited.insert(id) {
                    continue;
                }
                let next = self.distance_with_norm(q, query_norm, id)?;
                if best.len() < ef || best.peek().is_some_and(|worst| next < *worst) {
                    queue.push(Reverse(next));
                    let n = self.node(id)?;
                    if (!live_only || !n.deleted) && allowed.is_none_or(|a| a.contains(&n.doc)) {
                        best.push(next);
                        if best.len() > ef {
                            best.pop();
                        }
                    }
                }
            }
        }
        Ok(best.into_sorted_vec())
    }
    fn select(&mut self, candidates: Vec<Hit>, limit: usize) -> Result<Vec<u64>, TalaDbError> {
        let mut selected = Vec::new();
        let mut rejected = Vec::new();
        for hit in candidates {
            let dimensions = self.h.dimensions;
            let point = self.node(hit.1)?.code.decode(dimensions);
            let mut diverse = true;
            for &other in &selected {
                if self.distance(&point, other)?.0 < hit.0 {
                    diverse = false;
                    break;
                }
            }
            if diverse {
                selected.push(hit.1);
            } else {
                rejected.push(hit.1);
            }
            if selected.len() == limit {
                return Ok(selected);
            }
        }
        selected.extend(
            rejected
                .into_iter()
                .take(limit.saturating_sub(selected.len())),
        );
        Ok(selected)
    }
}

/// Tombstone the current version of a document. Old links remain valid.
pub(crate) fn remove(
    txn: &mut dyn WriteTxn,
    h: &mut Header,
    doc: &[u8; 16],
) -> Result<(), TalaDbError> {
    if let Some(bytes) = txn.get(&h.table, &map_key(doc))? {
        txn.delete(&h.table, &map_key(doc))?;
        let id = u64::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| invalid("invalid HNSW mapping"))?,
        );
        let mut n = Reader::new(&WriteView(txn), h).node(id)?.clone();
        if !n.deleted {
            n.deleted = true;
            h.live = h
                .live
                .checked_sub(1)
                .ok_or_else(|| invalid("invalid HNSW live-node count; rebuild the vector index"))?;
            h.deleted = h
                .deleted
                .checked_add(1)
                .ok_or_else(|| invalid("HNSW deleted-node count overflow"))?;
        }
        txn.put(&h.table, &node_key(id), &postcard::to_allocvec(&n)?)?;
    }
    Ok(())
}
pub(crate) fn insert(
    txn: &mut dyn WriteTxn,
    h: &mut Header,
    doc: [u8; 16],
    values: &[f32],
) -> Result<(), TalaDbError> {
    if values.len() != h.dimensions || !values.iter().all(|x| x.is_finite()) {
        return Err(invalid("invalid HNSW vector"));
    }
    remove(txn, h, &doc)?;
    let id = h.next;
    h.next = h
        .next
        .checked_add(1)
        .ok_or_else(|| invalid("HNSW node ID exhausted"))?;
    // SplitMix64 gives a reproducible level distribution without native RNG or threads.
    let mut rng = id.wrapping_add(0x9e3779b97f4a7c15);
    rng = (rng ^ (rng >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    rng = (rng ^ (rng >> 27)).wrapping_mul(0x94d049bb133111eb);
    rng ^= rng >> 31;
    let mut level = 0;
    while level < 16 && rng.is_multiple_of(u64::from(h.options.m)) {
        level += 1;
        rng /= u64::from(h.options.m);
    }
    let code = Code::encode(values, h.options.quantization);
    let query = code.decode(h.dimensions);
    let mut node = Node {
        doc,
        code,
        links: vec![vec![]; level + 1],
        deleted: false,
    };
    if let Some(mut entry) = h.entry {
        let view = WriteView(txn);
        let mut reader = Reader::new(&view, h);
        for layer in ((level + 1)..=h.level).rev() {
            entry = reader.greedy(&query, entry, layer)?;
        }
        for layer in (0..=level.min(h.level)).rev() {
            let candidates = reader.layer(
                &query,
                &[entry],
                layer,
                h.options.ef_construction as usize,
                None,
                false,
            )?;
            if let Some(best) = candidates.first() {
                entry = best.1;
            }
            node.links[layer] = reader.select(candidates, h.options.m as usize)?;
        }
        drop(reader);
        txn.put(&h.table, &node_key(id), &postcard::to_allocvec(&node)?)?;
        // Connect both directions; prune using the diversity heuristic.
        for (layer, neighbors) in node.links.iter().enumerate() {
            for &neighbor in neighbors {
                let view = WriteView(txn);
                let mut reader = Reader::new(&view, h);
                let mut n = reader.node(neighbor)?.clone();
                n.links[layer].push(id);
                let limit = h.options.m as usize * if layer == 0 { 2 } else { 1 };
                if n.links[layer].len() > limit {
                    let q = n.code.decode(h.dimensions);
                    let mut candidates = n.links[layer]
                        .iter()
                        .map(|&v| reader.distance(&q, v))
                        .collect::<Result<Vec<_>, _>>()?;
                    candidates.sort_unstable();
                    n.links[layer] = reader.select(candidates, limit)?;
                }
                txn.put(&h.table, &node_key(neighbor), &postcard::to_allocvec(&n)?)?;
            }
        }
    } else {
        txn.put(&h.table, &node_key(id), &postcard::to_allocvec(&node)?)?;
    }
    if h.entry.is_none() || level > h.level {
        h.entry = Some(id);
        h.level = level;
    }
    h.live = h
        .live
        .checked_add(1)
        .ok_or_else(|| invalid("HNSW live-node count overflow"))?;
    txn.put(&h.table, &map_key(&doc), &id.to_le_bytes())?;
    Ok(())
}

pub(crate) fn search(
    txn: &dyn ReadTxn,
    h: &Header,
    query: &[f32],
    ef: usize,
    allowed: Option<&HashSet<[u8; 16]>>,
) -> Result<(Vec<[u8; 16]>, usize), TalaDbError> {
    let Some(mut entry) = h.entry else {
        return Ok((vec![], 0));
    };
    if h.live == 0 {
        return Ok((vec![], 0));
    }
    let q = if h.options.quantization == Quantization::Binary {
        Code::encode(query, Quantization::Binary).decode(h.dimensions)
    } else {
        query.to_vec()
    };
    let mut reader = Reader::new(txn, h);
    for layer in (1..=h.level).rev() {
        entry = reader.greedy(&q, entry, layer)?;
    }
    let hits = reader.layer(&q, &[entry], 0, ef.max(1), allowed, true)?;
    let ids = hits
        .iter()
        .map(|h| reader.node(h.1).map(|n| n.doc))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((ids, reader.distances))
}
