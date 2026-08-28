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
    format!("hnsw::{collection}::{field}")
}

// ---------------------------------------------------------------------------
// VectorMetric
// ---------------------------------------------------------------------------

/// Similarity metric used when searching a vector index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
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
        Self {
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
    pub(super) type HnswCacheMap = HashMap<String, Arc<HnswGraph>>;
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

    pub(super) fn set_metric(metric: &VectorMetric) {
        let code: u8 = match metric {
            VectorMetric::Cosine => 0,
            VectorMetric::Dot => 1,
            VectorMetric::Euclidean => 2,
        };
        ACTIVE_METRIC.with(|m| m.set(code));
    }

    pub(super) fn distance_to_similarity(metric: &VectorMetric, distance: f32) -> f32 {
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
    format!("{collection}::{field}")
}

pub fn vec_table_name(collection: &str, field: &str) -> String {
    format!("vec::{collection}::{field}")
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
    // `as_chunks` splits and length-checks in one step: a non-empty remainder
    // *is* "not a multiple of 4", so the guard cannot drift from the split it
    // guards. The chunks come back as `[u8; 4]`, which `from_le_bytes` takes
    // directly — no per-element indexing to bounds-check.
    let (quads, rest) = bytes.as_chunks::<4>();
    if !rest.is_empty() {
        return None;
    }
    Some(quads.iter().copied().map(f32::from_le_bytes).collect())
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
    // Lane-parallel for the same reason as `dot_and_norm_sq` — see `LANES`.
    let mut acc = [0.0f32; LANES];
    let (a_lanes, a_rest) = a.as_chunks::<LANES>();
    let (b_lanes, b_rest) = b.as_chunks::<LANES>();
    for (x, y) in a_lanes.iter().zip(b_lanes) {
        for i in 0..LANES {
            acc[i] += x[i] * y[i];
        }
    }
    let mut total: f32 = acc.iter().sum();
    for (x, y) in a_rest.iter().zip(b_rest) {
        total += x * y;
    }
    total
}

/// Converts Euclidean distance to a similarity score in `(0, 1]`.
/// Identical vectors → 1.0; the further apart, the closer to 0.
pub fn euclidean_similarity(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; LANES];
    let (a_lanes, a_rest) = a.as_chunks::<LANES>();
    let (b_lanes, b_rest) = b.as_chunks::<LANES>();
    for (x, y) in a_lanes.iter().zip(b_lanes) {
        for i in 0..LANES {
            let d = x[i] - y[i];
            acc[i] += d * d;
        }
    }
    let mut dist_sq: f32 = acc.iter().sum();
    for (x, y) in a_rest.iter().zip(b_rest) {
        let d = x - y;
        dist_sq += d * d;
    }
    1.0 / (1.0 + dist_sq.sqrt())
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

/// [`compute_similarity`] with the query's L2 norm supplied by the caller.
///
/// This is the form a flat scan wants: `query_norm` is invariant across the
/// whole table, so computing it once at the top of the loop removes a dot
/// product and a square root per stored vector. `query_norm` is ignored for
/// metrics other than [`VectorMetric::Cosine`].
///
/// `query` and `stored` are expected to have the same length. They are walked
/// in lockstep and the shorter one ends the reduction, so a mismatch yields a
/// score computed over the common prefix rather than an error — callers that
/// can encounter untrusted or corrupt vectors should length-check first, as
/// `Collection::find_nearest` does.
#[inline]
pub fn score_with_query_norm(
    metric: &VectorMetric,
    query: &[f32],
    query_norm: f32,
    stored: &[f32],
) -> f32 {
    match metric {
        VectorMetric::Cosine => {
            let (dot, norm_sq) = dot_and_norm_sq(query, stored);
            let norm_b = norm_sq.sqrt();
            if query_norm == 0.0 || norm_b == 0.0 {
                0.0
            } else {
                dot / (query_norm * norm_b)
            }
        }
        VectorMetric::Dot => dot_similarity(query, stored),
        VectorMetric::Euclidean => euclidean_similarity(query, stored),
    }
}

/// Number of independent accumulators used by the scoring reductions.
///
/// Floating-point addition is not associative, so LLVM may not reorder a `+=`
/// chain into vector lanes: every element waits on the previous partial sum.
/// Splitting the reduction into `LANES` independent accumulators authorises
/// that reordering *in the source*, which is both portable — x86 AVX, aarch64
/// NEON, and the wasm128 SIMD every shipping browser has — and free of
/// `core::arch` intrinsics, `unsafe`, and nightly features.
///
/// Eight covers a 256-bit vector of `f32` and splits cleanly into two 128-bit
/// operations where that is all the target has. Embedding dimensions are
/// multiples of 8 in practice (384, 768, 1536), so the scalar remainder loop is
/// usually empty.
const LANES: usize = 8;

/// Dot product and the stored vector's squared L2 norm, in one pass.
///
/// Cosine needs both, and reading each element once keeps the pass
/// memory-bound rather than doing two walks over the same data.
#[inline]
fn dot_and_norm_sq(query: &[f32], stored: &[f32]) -> (f32, f32) {
    let mut dot = [0.0f32; LANES];
    let mut norm_sq = [0.0f32; LANES];

    // `as_chunks` rather than `chunks_exact`: the chunks arrive as
    // `&[[f32; LANES]]`, so `q[i]` for `i in 0..LANES` is in bounds by type and
    // needs no bounds check at all. With `chunks_exact` each chunk is a
    // runtime-length `&[f32]` and the checks only vanish if the optimiser
    // happens to prove the range — which is a thin thing to rest a hot loop on.
    let (q_lanes, q_rest) = query.as_chunks::<LANES>();
    let (s_lanes, s_rest) = stored.as_chunks::<LANES>();
    for (q, s) in q_lanes.iter().zip(s_lanes) {
        for i in 0..LANES {
            dot[i] += q[i] * s[i];
            norm_sq[i] += s[i] * s[i];
        }
    }

    let mut dot_total: f32 = dot.iter().sum();
    let mut norm_total: f32 = norm_sq.iter().sum();
    for (q, s) in q_rest.iter().zip(s_rest) {
        dot_total += q * s;
        norm_total += s * s;
    }
    (dot_total, norm_total)
}

// NOTE: a `score_from_bytes` variant that scored directly against the stored
// little-endian bytes used to live here. It had no callers: `find_nearest`
// decodes the whole vector table once into a generation-keyed cache
// (`Collection::find_nearest`, step 3b) and scores against `&[f32]` via
// `score_with_query_norm`, so nothing ever reaches the raw bytes in the hot
// loop. It was removed rather than wired up — its own doc comment claimed to be
// the flat scan's fast path, which sent at least one reviewer optimising a
// function that never ran.

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

    // -----------------------------------------------------------------------
    // Lane-parallel reductions
    //
    // The scoring loops split their sums across `LANES` independent
    // accumulators so the compiler can vectorise them. That changes the order
    // of floating-point additions, so these tests pin the results against a
    // straightforward scalar reference — including at lengths that are not a
    // multiple of `LANES`, where the remainder loop has to pick up the tail.
    // -----------------------------------------------------------------------

    fn scalar_dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    fn scalar_euclidean(a: &[f32], b: &[f32]) -> f32 {
        let d: f32 = a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum();
        1.0 / (1.0 + d.sqrt())
    }

    fn ramp(len: usize, offset: f32) -> Vec<f32> {
        (0..len)
            .map(|i| (i as f32).mul_add(0.125, offset))
            .collect()
    }

    #[test]
    fn lane_reductions_match_scalar_at_every_remainder_length() {
        // 0 and 1..=17 covers empty, sub-LANES, exactly LANES, and two full
        // chunks plus a tail.
        for len in std::iter::once(0).chain(1..=17) {
            let a = ramp(len, 0.5);
            let b = ramp(len, -0.25);

            let dot = dot_similarity(&a, &b);
            assert!(
                (dot - scalar_dot(&a, &b)).abs() < 1e-4,
                "dot mismatch at len {len}: {dot} vs {}",
                scalar_dot(&a, &b)
            );

            let euc = euclidean_similarity(&a, &b);
            assert!(
                (euc - scalar_euclidean(&a, &b)).abs() < 1e-6,
                "euclidean mismatch at len {len}"
            );

            // Cosine goes through the fused dot + norm pass.
            let norm = l2_norm(&a);
            let cos = score_with_query_norm(&VectorMetric::Cosine, &a, norm, &b);
            let expected = cosine_similarity(&a, &b);
            assert!(
                (cos - expected).abs() < 1e-5,
                "cosine mismatch at len {len}: {cos} vs {expected}"
            );
        }
    }

    #[test]
    fn identical_vectors_still_score_perfectly_after_lane_split() {
        // A reassociated sum must not drift far enough to break the invariant
        // callers rely on: a vector is maximally similar to itself.
        for len in [4usize, 8, 12, 384] {
            let v = ramp(len, 1.0);
            let cos = score_with_query_norm(&VectorMetric::Cosine, &v, l2_norm(&v), &v);
            assert!(
                (cos - 1.0).abs() < 1e-5,
                "cosine self-similarity at len {len}: {cos}"
            );
            let euc = euclidean_similarity(&v, &v);
            assert!(
                (euc - 1.0).abs() < 1e-6,
                "euclidean self-similarity at len {len}"
            );
        }
    }
    /// `dot_and_norm_sq` was refactored from `chunks_exact(LANES)` to
    /// `as_chunks::<LANES>()` to clear `clippy::chunks_exact_to_as_chunks`. It is
    /// a hot float reduction whose *lane structure is the point*, so the bar is
    /// bit-identical output, not "close enough" — a reordered accumulation would
    /// silently shift every cosine score in the database.
    ///
    /// This lives in-module because `dot_and_norm_sq` is private; the public
    /// `dot_similarity` / `euclidean_similarity` / `decode_f32_vec` were checked
    /// the same way.
    #[test]
    fn dot_and_norm_sq_matches_the_chunks_exact_original_bit_for_bit() {
        // The whole point of `original` is to be the pre-refactor code, so the
        // lint that prompted the refactor must not be applied to it. Rewriting
        // it to `as_chunks` would make it a copy of the new implementation and
        // the test would compare something to itself.
        #[allow(clippy::chunks_exact_to_as_chunks)]
        fn original(query: &[f32], stored: &[f32]) -> (f32, f32) {
            let mut dot = [0.0f32; LANES];
            let mut norm_sq = [0.0f32; LANES];
            let mut q_chunks = query.chunks_exact(LANES);
            let mut s_chunks = stored.chunks_exact(LANES);
            for (q, s) in q_chunks.by_ref().zip(s_chunks.by_ref()) {
                for i in 0..LANES {
                    dot[i] += q[i] * s[i];
                    norm_sq[i] += s[i] * s[i];
                }
            }
            let mut dot_total: f32 = dot.iter().sum();
            let mut norm_total: f32 = norm_sq.iter().sum();
            for (q, s) in q_chunks.remainder().iter().zip(s_chunks.remainder()) {
                dot_total += q * s;
                norm_total += s * s;
            }
            (dot_total, norm_total)
        }

        // Deterministic, and spread across magnitudes so summation order matters.
        let mut seed = 0xC0FF_EE12_u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let bits = (seed >> 33) as u32;
            (f32::from_bits((bits & 0x007F_FFFF) | 0x3F80_0000) - 1.5) * 1e3
        };

        // Lengths either side of LANES, and real embedding sizes, so both the
        // lane loop and the remainder tail are exercised.
        for len in [0usize, 1, 7, 8, 9, 15, 16, 17, 31, 384, 385, 768, 1536] {
            let q: Vec<f32> = (0..len).map(|_| next()).collect();
            let s: Vec<f32> = (0..len).map(|_| next()).collect();
            let (new_dot, new_norm) = dot_and_norm_sq(&q, &s);
            let (old_dot, old_norm) = original(&q, &s);
            assert_eq!(
                new_dot.to_bits(),
                old_dot.to_bits(),
                "dot diverged at len={len}"
            );
            assert_eq!(
                new_norm.to_bits(),
                old_norm.to_bits(),
                "norm_sq diverged at len={len}"
            );
        }
    }
}
