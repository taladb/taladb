//! Vector index support for TalaDB — Phase 1 (flat / brute-force search).
//!
//! Storage layout
//! ──────────────
//! Meta table  : `meta::vector_indexes`
//!   key       : `<collection>::<field>`   (UTF-8)
//!   value     : postcard-serialised `VectorDef`
//!
//! Vector table: `vec::<collection>::<field>`
//!   key       : ULID bytes (16 B, big-endian)
//!   value     : raw f32 LE bytes  (dimensions × 4 B)
//!
//! Search algorithm
//! ────────────────
//! Phase 1 uses a flat (brute-force) linear scan — O(n·d).
//! Every vector stored in the vec table is scored against the query vector,
//! results are sorted descending, and the top-k documents are loaded.
//! Phase 2 will replace this with an HNSW index for sub-linear search.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::document::{Document, Value};

// ---------------------------------------------------------------------------
// Decoded-vector cache (flat search)
// ---------------------------------------------------------------------------
//
// The flat search path reads the entire `vec::` table from storage and decodes
// every f32 vector on *each* query. This cache keeps the decoded vectors in
// memory so repeated queries are memory-bound rather than storage-bound — the
// common case for retrieval workloads (many searches, occasional writes).
//
// Each entry is tagged with the collection's write generation (see
// `crate::watch`). A read serves the cache only when the stored generation
// still matches the collection's current one; any committed write bumps the
// generation via `watch::notify`, so the flat path stays exact. Unlike the
// HNSW graph cache, this must never be served stale — flat search is the
// always-current, exact path.

/// Decoded vectors for one `collection::field`, tagged with the write
/// generation they were built at.
pub struct CachedVectors {
    pub generation: u64,
    pub vectors: Arc<Vec<(Ulid, Vec<f32>)>>,
}

pub type VectorCacheMap = HashMap<String, CachedVectors>;
/// Shared across every `Collection` handle from the same `Database`, so a
/// write through one handle invalidates reads through another.
pub type SharedVectorCache = Arc<Mutex<VectorCacheMap>>;

pub fn new_shared_vector_cache() -> SharedVectorCache {
    Arc::new(Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const META_VECTOR_TABLE: &str = "meta::vector_indexes";
/// Stores HNSW options: key = `"{collection}::{field}"`, value = postcard `HnswOptions`.
pub const META_HNSW_TABLE: &str = "meta::hnsw_indexes";
/// HNSW graph blob table: `hnsw::{collection}::{field}`, single key = `b"graph"`.
pub fn hnsw_table_name(collection: &str, field: &str) -> String {
    format!("hnsw::{}::{}", collection, field)
}

// ---------------------------------------------------------------------------
// VectorMetric
// ---------------------------------------------------------------------------

/// Similarity metric used when searching a vector index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum VectorMetric {
    /// Cosine similarity — angle between vectors, range [-1, 1].
    /// Best for text embeddings where magnitude is not meaningful.
    #[default]
    Cosine,
    /// Raw dot product — magnitude-sensitive.
    Dot,
    /// Euclidean distance converted to similarity via `1 / (1 + dist)`.
    /// Range (0, 1]; identical vectors score 1.0.
    Euclidean,
}

// ---------------------------------------------------------------------------
// VectorDef — index metadata persisted in redb
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorDef {
    pub collection: String,
    pub field: String,
    /// Expected dimensionality; enforced on insert and search.
    pub dimensions: usize,
    pub metric: VectorMetric,
}

// ---------------------------------------------------------------------------
// HnswOptions — HNSW index configuration (stored in META_HNSW_TABLE)
// ---------------------------------------------------------------------------

/// Configuration for an HNSW vector index.
///
/// Only relevant when the `vector-hnsw` feature is enabled.  Storing these
/// options in a separate table keeps `VectorDef` backward-compatible: flat
/// indexes have no entry in `META_HNSW_TABLE`, HNSW indexes do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswOptions {
    /// Number of bi-directional links per node (connectivity).
    /// Higher values improve recall but increase memory and build time.
    /// Typical range: 8–48. Default: 16.
    pub m: u32,
    /// Build-time quality parameter (ef during construction).
    /// Higher values produce a better graph at the cost of slower builds.
    /// Must be ≥ `m`. Default: 200.
    pub ef_construction: u32,
}

impl Default for HnswOptions {
    fn default() -> Self {
        HnswOptions {
            m: 16,
            ef_construction: 200,
        }
    }
}

// ---------------------------------------------------------------------------
// HNSW build / search  (compiled only when feature = "vector-hnsw")
//
// Design: the HNSW graph lives entirely in memory (no serialisation).
//
// • `instant-distance` 0.6.x does not expose a working serde implementation
//   for its const-generic array types across all Rust toolchains.  To stay
//   toolchain-agnostic and WASM-safe we skip the serde feature and instead
//   keep the built graph in a `SharedHnswCache` that is shared between every
//   `Collection` handle returned by the same `Database` instance.
//
// • On `create_vector_index`/`upgrade_vector_index` the graph is built from
//   the always-persisted flat `vec::` table and inserted into the cache.
//
// • On `Database::open*` callers may call `Database::rebuild_hnsw_indexes()`
//   to warm the cache from any existing HNSW metadata entries.
//
// • HNSW graphs survive for the lifetime of the `Database` handle; they are
//   dropped when the `Database` is dropped or when `upgrade_vector_index` is
//   called (which replaces the entry).
// ---------------------------------------------------------------------------

#[cfg(feature = "vector-hnsw")]
mod hnsw_impl {
    use std::cell::Cell;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use instant_distance::{Builder, HnswMap, Point as HnswTrait, Search};
    use ulid::Ulid;

    use super::{VectorMetric, cosine_similarity, dot_similarity};
    use crate::error::TalaDbError;

    // ------------------------------------------------------------------
    // Shared in-memory HNSW cache
    // ------------------------------------------------------------------

    /// HNSW map type — values are `()` because the ULID is embedded in each point.
    pub type HnswGraph = HnswMap<HnswPoint, ()>;

    /// Graph entry keyed by `"{collection}::{field}"`.
    pub type HnswCacheMap = HashMap<String, Arc<HnswGraph>>;
    /// Thread-safe, reference-counted cache shared by all Collection handles
    /// belonging to the same Database.
    pub type SharedHnswCache = Arc<Mutex<HnswCacheMap>>;

    pub fn new_shared_cache() -> SharedHnswCache {
        Arc::new(Mutex::new(HashMap::new()))
    }

    // ------------------------------------------------------------------
    // Thread-local metric dispatch (set before every build/search)
    // ------------------------------------------------------------------

    thread_local! {
        /// 0 = Cosine, 1 = Dot, 2 = Euclidean
        static ACTIVE_METRIC: Cell<u8> = const { Cell::new(0) };
    }

    pub fn set_metric(metric: &VectorMetric) {
        let code: u8 = match metric {
            VectorMetric::Cosine => 0,
            VectorMetric::Dot => 1,
            VectorMetric::Euclidean => 2,
        };
        ACTIVE_METRIC.with(|m| m.set(code));
    }

    pub fn distance_to_similarity(metric: &VectorMetric, distance: f32) -> f32 {
        match metric {
            VectorMetric::Cosine => 1.0 - distance,
            VectorMetric::Dot => -distance,
            VectorMetric::Euclidean => 1.0 / (1.0 + distance),
        }
    }

    // ------------------------------------------------------------------
    // HnswPoint — embeds the ULID so search results are self-identifying
    // ------------------------------------------------------------------

    #[derive(Clone)]
    pub struct HnswPoint {
        pub id_bytes: [u8; 16],
        pub vec: Vec<f32>,
    }

    impl HnswTrait for HnswPoint {
        fn distance(&self, other: &Self) -> f32 {
            ACTIVE_METRIC.with(|m| match m.get() {
                0 => 1.0 - cosine_similarity(&self.vec, &other.vec),
                1 => -dot_similarity(&self.vec, &other.vec),
                _ => self
                    .vec
                    .iter()
                    .zip(other.vec.iter())
                    .map(|(a, b)| (a - b).powi(2))
                    .sum::<f32>()
                    .sqrt(),
            })
        }
    }

    // ------------------------------------------------------------------
    // Build
    // ------------------------------------------------------------------

    /// Build an HNSW graph from `vectors` and return it ready for search.
    ///
    /// `VectorMetric::Dot` is rejected here because raw dot product is not a
    /// valid HNSW distance (can be arbitrarily negative, violating the
    /// greedy-descent invariant). Callers that want magnitude-sensitive
    /// similarity should L2-normalise their vectors and use `Cosine`, which is
    /// mathematically equivalent on unit vectors.
    pub fn build_hnsw(
        vectors: &[(Ulid, Vec<f32>)],
        metric: &VectorMetric,
        ef_construction: u32,
    ) -> Result<Arc<HnswGraph>, TalaDbError> {
        if matches!(metric, VectorMetric::Dot) {
            return Err(TalaDbError::InvalidOperation(
                "HNSW indexes do not support the Dot metric; L2-normalise vectors and use Cosine instead"
                    .into(),
            ));
        }
        set_metric(metric);

        let points: Vec<HnswPoint> = vectors
            .iter()
            .map(|(id, vec)| HnswPoint {
                id_bytes: id.to_bytes(),
                vec: vec.clone(),
            })
            .collect();

        // `HnswMap::build` requires a parallel values vec; we embed the ULID in
        // the point itself so the values vec is `()`.
        let values = vec![(); points.len()];
        let hnsw = Builder::default()
            .ef_construction(ef_construction as usize)
            .build(points, values);

        Ok(Arc::new(hnsw))
    }

    // ------------------------------------------------------------------
    // Search
    // ------------------------------------------------------------------

    /// Search an in-memory HNSW graph and return the top-k results.
    pub fn search_hnsw(
        hnsw: &HnswGraph,
        query: &[f32],
        metric: &VectorMetric,
        top_k: usize,
    ) -> Vec<(Ulid, f32)> {
        set_metric(metric);

        let query_point = HnswPoint {
            id_bytes: [0u8; 16],
            vec: query.to_vec(),
        };
        let mut search = Search::default();

        let mut results: Vec<(Ulid, f32)> = hnsw
            .search(&query_point, &mut search)
            .take(top_k)
            .map(|item| {
                let id = Ulid::from_bytes(item.point.id_bytes);
                let score = distance_to_similarity(metric, item.distance);
                (id, score)
            })
            .collect();

        results.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }
}

#[cfg(feature = "vector-hnsw")]
pub use hnsw_impl::{
    HnswGraph, HnswPoint, SharedHnswCache, build_hnsw, new_shared_cache, search_hnsw,
};

// ---------------------------------------------------------------------------
// Table / key naming helpers
// ---------------------------------------------------------------------------

pub fn vec_meta_key(collection: &str, field: &str) -> String {
    format!("{}::{}", collection, field)
}

pub fn vec_table_name(collection: &str, field: &str) -> String {
    format!("vec::{}::{}", collection, field)
}

// ---------------------------------------------------------------------------
// Encoding / decoding
// ---------------------------------------------------------------------------

/// Encode a `&[f32]` as little-endian bytes.
pub fn encode_f32_vec(v: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(v.len() * 4);
    for f in v {
        buf.extend_from_slice(&f.to_le_bytes());
    }
    buf
}

/// Decode little-endian bytes back to `Vec<f32>`.
/// Returns `None` if `bytes.len()` is not a multiple of 4.
pub fn decode_f32_vec(bytes: &[u8]) -> Option<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Extract a float vector from a document field value.
/// Accepts `Value::Array` whose elements are all numeric (`Float` or `Int`).
/// Returns `None` if the value is not a numeric array.
pub fn value_to_f32_vec(v: &Value) -> Option<Vec<f32>> {
    match v {
        Value::Array(arr) => arr
            .iter()
            .map(|item| match item {
                Value::Float(f) => Some(*f as f32),
                Value::Int(n) => Some(*n as f32),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Similarity functions
// ---------------------------------------------------------------------------

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

pub fn dot_similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Converts Euclidean distance to a similarity score in `(0, 1]`.
/// Identical vectors → 1.0; the further apart, the closer to 0.
pub fn euclidean_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dist: f32 = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f32>()
        .sqrt();
    1.0 / (1.0 + dist)
}

pub fn compute_similarity(metric: &VectorMetric, a: &[f32], b: &[f32]) -> f32 {
    match metric {
        VectorMetric::Cosine => cosine_similarity(a, b),
        VectorMetric::Dot => dot_similarity(a, b),
        VectorMetric::Euclidean => euclidean_similarity(a, b),
    }
}

/// L2 (Euclidean) norm of a vector. Hoist this out of a scan loop for cosine —
/// the query's norm is constant across every candidate, so recomputing it per
/// candidate (as `cosine_similarity` does) wastes one dot+sqrt per stored
/// vector.
#[inline]
pub fn l2_norm(a: &[f32]) -> f32 {
    a.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Score a query against a stored vector held as raw little-endian f32 bytes,
/// without decoding it into an intermediate `Vec<f32>` first. In a flat scan
/// this removes one heap allocation per stored vector per query.
///
/// `query_norm` is the query's precomputed L2 norm ([`l2_norm`]); it is only
/// read for [`VectorMetric::Cosine`], so pass `0.0` for the other metrics.
/// Returns `None` when `bytes` is not exactly `query.len() * 4` bytes (a
/// dimension mismatch or a truncated/corrupt entry), which the caller skips.
///
/// The per-metric branch is hoisted out of the element loop so each variant
/// compiles to a tight reduction the optimiser can vectorise.
#[inline]
pub fn score_from_bytes(
    metric: &VectorMetric,
    query: &[f32],
    query_norm: f32,
    bytes: &[u8],
) -> Option<f32> {
    if bytes.len() != query.len() * 4 {
        return None;
    }
    let chunks = bytes.chunks_exact(4);
    let score = match metric {
        VectorMetric::Dot => {
            let mut dot = 0.0f32;
            for (q, c) in query.iter().zip(chunks) {
                dot += q * f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            }
            dot
        }
        VectorMetric::Cosine => {
            let mut dot = 0.0f32;
            let mut norm_b = 0.0f32;
            for (q, c) in query.iter().zip(chunks) {
                let b = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                dot += q * b;
                norm_b += b * b;
            }
            let norm_b = norm_b.sqrt();
            if query_norm == 0.0 || norm_b == 0.0 {
                0.0
            } else {
                dot / (query_norm * norm_b)
            }
        }
        VectorMetric::Euclidean => {
            let mut dist_sq = 0.0f32;
            for (q, c) in query.iter().zip(chunks) {
                let d = q - f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                dist_sq += d * d;
            }
            1.0 / (1.0 + dist_sq.sqrt())
        }
    };
    Some(score)
}

// ---------------------------------------------------------------------------
// Search result
// ---------------------------------------------------------------------------

/// A single result returned by `Collection::find_nearest`.
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    /// The matched document (all fields, including `_id`).
    pub document: Document,
    /// Similarity score — higher is more similar.
    /// Range depends on the metric: cosine ∈ [-1,1], dot ∈ ℝ, euclidean ∈ (0,1].
    pub score: f32,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let v = vec![0.1f32, -0.5, 1.0, 99.9];
        let decoded = decode_f32_vec(&encode_f32_vec(&v)).unwrap();
        for (a, b) in v.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1e-6, "{a} != {b}");
        }
    }

    #[test]
    fn decode_rejects_odd_length() {
        assert!(decode_f32_vec(&[1, 2, 3]).is_none());
    }

    #[test]
    fn cosine_identical() {
        let a = vec![1.0f32, 2.0, 3.0];
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn euclidean_identical() {
        let a = vec![1.0f32, 2.0, 3.0];
        assert!((euclidean_similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn value_extraction_floats() {
        let v = Value::Array(vec![Value::Float(0.5), Value::Int(1), Value::Float(-0.25)]);
        let r = value_to_f32_vec(&v).unwrap();
        assert_eq!(r.len(), 3);
        assert!((r[0] - 0.5f32).abs() < 1e-6);
        assert_eq!(r[1], 1.0f32);
        assert!((r[2] - (-0.25f32)).abs() < 1e-6);
    }

    #[test]
    fn value_extraction_rejects_mixed() {
        let v = Value::Array(vec![Value::Float(0.5), Value::Str("x".into())]);
        assert!(value_to_f32_vec(&v).is_none());
    }

    #[test]
    fn value_extraction_rejects_non_array() {
        assert!(value_to_f32_vec(&Value::Str("vec".into())).is_none());
    }
}
