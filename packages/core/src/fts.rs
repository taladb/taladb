//! Full-text search index for TalaDB.
//!
//! Strategy: token-based inverted index stored in a redb table.
//!
//! Table name:  `fts::<collection>::<field>`
//! Key format:  `<token_bytes> ++ <ulid_bytes>` (16 B ULID suffix)
//! Value:       term frequency, u32 LE (v1). Empty in v0 indexes — see
//!              [`decode_tf`], which reads those as a frequency of 1.
//!
//! Two side tables carry what BM25 needs beyond the postings list:
//!
//! - `fts_len::<collection>::<field>`   — ULID → token count, u32 LE
//! - `fts_stats::<collection>::<field>` — a single [`crate::bm25::FtsStats`]
//!
//! On insert/update/delete, the set of tokens for the affected field is
//! computed and the corresponding index entries are added or removed.
//!
//! Query:  tokenize the search string, collect ULID sets per token, return
//!         the intersection (AND semantics, i.e. all tokens must appear).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use ulid::Ulid;

// ---------------------------------------------------------------------------
// FTS index metadata (stored in `meta::fts_indexes`)
// ---------------------------------------------------------------------------

/// Current on-disk layout for FTS indexes.
///
/// v0 — postings only, empty values, no side tables.
/// v1 — term frequencies in the postings value, plus the length and stats
///      tables. Required for BM25 ranking.
pub const FTS_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtsDef {
    pub collection: String,
    pub field: String,
    /// Layout version; see [`FTS_VERSION`]. Indexes created before ranking
    /// existed decode as `0` and are readable but not rankable.
    pub version: u32,
}

impl FtsDef {
    /// Whether this index carries the statistics BM25 needs.
    pub fn supports_ranking(&self) -> bool {
        self.version >= 1
    }
}

/// The pre-versioning layout, kept only so old metadata still loads.
#[derive(Deserialize)]
struct FtsDefV0 {
    collection: String,
    field: String,
}

/// Decode index metadata, tolerating records written before `version` existed.
///
/// postcard is not self-describing, so a v0 record is simply too short to
/// satisfy the current struct and fails cleanly — which is what makes the
/// fallback safe rather than a guess.
pub fn decode_fts_def(bytes: &[u8]) -> Result<FtsDef, postcard::Error> {
    match postcard::from_bytes::<FtsDef>(bytes) {
        Ok(def) => Ok(def),
        Err(_) => {
            let v0: FtsDefV0 = postcard::from_bytes(bytes)?;
            Ok(FtsDef {
                collection: v0.collection,
                field: v0.field,
                version: 0,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Table naming
// ---------------------------------------------------------------------------

pub fn fts_table_name(collection: &str, field: &str) -> String {
    format!("fts::{collection}::{field}")
}

/// Per-document token counts, keyed by ULID.
pub fn fts_len_table_name(collection: &str, field: &str) -> String {
    format!("fts_len::{collection}::{field}")
}

/// Corpus statistics; holds exactly one record under [`FTS_STATS_KEY`].
pub fn fts_stats_table_name(collection: &str, field: &str) -> String {
    format!("fts_stats::{collection}::{field}")
}

/// The only key in a `fts_stats::` table.
pub const FTS_STATS_KEY: &[u8] = b"stats";

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// A ranked full-text match. Mirrors [`crate::vector::VectorSearchResult`]
/// so both retrieval paths return the same shape.
#[derive(Debug, Clone)]
pub struct TextSearchResult {
    /// The matched document (all fields, including `_id`).
    pub document: crate::document::Document,
    /// BM25 relevance score — higher is more relevant. Unbounded above, and
    /// only comparable within a single query's result set.
    pub score: f32,
}

/// A hybrid (text + vector) match.
#[derive(Debug, Clone)]
pub struct HybridSearchResult {
    /// The matched document (all fields, including `_id`).
    pub document: crate::document::Document,
    /// Fused RRF score. Small by construction — the top possible value with
    /// default parameters is `2/61` — and meaningful only as an ordering
    /// within one result set, never as a similarity or a confidence.
    pub score: f32,
    /// Zero-based position in the text ranking, or `None` if the text
    /// retriever did not return this document.
    pub text_rank: Option<usize>,
    /// Zero-based position in the vector ranking, or `None` if the vector
    /// retriever did not return this document.
    pub vector_rank: Option<usize>,
}

/// Inputs for [`crate::collection::Collection::hybrid_search`].
pub struct HybridQuery<'a> {
    /// Field carrying the FTS index.
    pub text_field: &'a str,
    /// Raw query text; tokenized the same way indexed text was.
    pub text: &'a str,
    /// Field carrying the vector index.
    pub vector_field: &'a str,
    /// Query embedding, matching the vector index's dimensions.
    pub vector: &'a [f32],
    /// Maximum results to return after fusion.
    pub top_k: usize,
    /// Optional metadata filter, applied to both retrievers before ranking.
    pub filter: Option<crate::query::Filter>,
    /// Fusion tuning.
    pub rrf: crate::bm25::RrfParams,
    /// BM25 tuning for the text half.
    pub bm25: crate::bm25::Bm25Params,
    /// Candidates to pull from each retriever before fusing. Defaults to
    /// `max(top_k * 4, 20)`; raise it for better recall at more cost.
    pub candidates: Option<usize>,
}

impl<'a> HybridQuery<'a> {
    /// A query with default fusion and scoring parameters.
    pub fn new(
        text_field: &'a str,
        text: &'a str,
        vector_field: &'a str,
        vector: &'a [f32],
        top_k: usize,
    ) -> Self {
        Self {
            text_field,
            text,
            vector_field,
            vector,
            top_k,
            filter: None,
            rrf: crate::bm25::RrfParams::default(),
            bm25: crate::bm25::Bm25Params::default(),
            candidates: None,
        }
    }

    /// Restrict both retrievers to documents matching `filter`.
    pub fn filter(mut self, filter: crate::query::Filter) -> Self {
        self.filter = Some(filter);
        self
    }
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

/// Tokenize a string into lowercase, whitespace/punctuation-split tokens.
/// Tokens shorter than 2 characters are ignored.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_lowercase)
        .collect()
}

/// Tokenize and count, returning each distinct token's frequency alongside
/// the document's total token count.
///
/// The postings list only needs one entry per distinct token, but BM25 needs
/// both the repeat count and the untruncated length, so both come from a
/// single pass.
pub fn token_frequencies(text: &str) -> (HashMap<String, u32>, u32) {
    let tokens = tokenize(text);
    let total = tokens.len().min(u32::MAX as usize) as u32;
    let mut freqs: HashMap<String, u32> = HashMap::new();
    for token in tokens {
        *freqs.entry(token).or_insert(0) += 1;
    }
    (freqs, total)
}

// ---------------------------------------------------------------------------
// Value encoding
// ---------------------------------------------------------------------------

/// Encode a term frequency for the postings value.
pub fn encode_tf(tf: u32) -> [u8; 4] {
    tf.to_le_bytes()
}

/// Decode a postings value into a term frequency.
///
/// v0 indexes stored an empty value. Reading those as `1` lets a legacy
/// index be ranked — imperfectly, since every term looks like it occurred
/// once — instead of scoring zero and disappearing from results.
pub fn decode_tf(bytes: &[u8]) -> u32 {
    if bytes.len() < 4 {
        return 1;
    }
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).max(1)
}

/// Encode a document's token count for the `fts_len::` table.
pub fn encode_doc_len(len: u32) -> [u8; 4] {
    len.to_le_bytes()
}

/// Decode a document's token count, defaulting to `0` when absent so callers
/// fall back to the corpus average.
pub fn decode_doc_len(bytes: &[u8]) -> u32 {
    if bytes.len() < 4 {
        return 0;
    }
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

// ---------------------------------------------------------------------------
// Key encoding
// ---------------------------------------------------------------------------

/// Encode a token + ULID into a sortable index key.
/// Format: `<token_utf8_bytes> ++ 0x00 ++ <ulid_16_bytes>`
/// The NUL separator lets us efficiently range-scan all ULIDs for a given token.
pub fn encode_fts_key(token: &str, ulid: &Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(token.len() + 1 + 16);
    key.extend_from_slice(token.as_bytes());
    key.push(0x00); // separator
    key.extend_from_slice(&ulid.to_bytes());
    key
}

/// Build the inclusive key range for all ULIDs associated with `token`.
/// start = `<token> ++ 0x00 ++ [0x00 × 16]`
/// end   = `<token> ++ 0x00 ++ [0xFF × 16]`
pub fn fts_token_range(token: &str) -> (Vec<u8>, Vec<u8>) {
    let mut start = Vec::with_capacity(token.len() + 17);
    start.extend_from_slice(token.as_bytes());
    start.push(0x00);
    start.extend_from_slice(&[0x00u8; 16]);

    let mut end = Vec::with_capacity(token.len() + 17);
    end.extend_from_slice(token.as_bytes());
    end.push(0x00);
    end.extend_from_slice(&[0xFFu8; 16]);

    (start, end)
}

/// Extract the ULID from an FTS key (last 16 bytes).
pub fn ulid_from_fts_key(key: &[u8]) -> Option<Ulid> {
    if key.len() < 17 {
        return None;
    }
    let bytes: [u8; 16] = key[key.len() - 16..].try_into().ok()?;
    Some(Ulid::from_bytes(bytes))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_basic() {
        let tokens = tokenize("Hello, World! This is TalaDB.");
        assert!(tokens.contains(&"hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"taladb".to_string()));
        // "is" has length 2 — kept
        assert!(tokens.contains(&"is".to_string()));
    }

    #[test]
    fn tokenize_strips_short() {
        let tokens = tokenize("a bb ccc");
        // "a" (len 1) dropped, "bb" kept, "ccc" kept
        assert!(!tokens.contains(&"a".to_string()));
        assert!(tokens.contains(&"bb".to_string()));
        assert!(tokens.contains(&"ccc".to_string()));
    }

    #[test]
    fn fts_key_round_trip() {
        let ulid = Ulid::new();
        let key = encode_fts_key("hello", &ulid);
        let decoded = ulid_from_fts_key(&key).unwrap();
        assert_eq!(decoded, ulid);
    }

    #[test]
    fn fts_token_range_bounds() {
        let (start, end) = fts_token_range("rust");
        assert!(start < end);
        // start must begin with "rust\0"
        assert_eq!(&start[..5], b"rust\0");
    }

    #[test]
    fn token_range_excludes_longer_tokens_sharing_a_prefix() {
        // The NUL separator is what stops a scan for "rust" from also
        // returning every "rusty" posting.
        let (start, end) = fts_token_range("rust");
        let rusty = encode_fts_key("rusty", &Ulid::new());
        assert!(rusty.as_slice() > end.as_slice());

        let rust = encode_fts_key("rust", &Ulid::new());
        assert!(rust.as_slice() >= start.as_slice() && rust.as_slice() <= end.as_slice());
    }

    #[test]
    fn token_frequencies_counts_repeats_and_total() {
        let (freqs, total) = token_frequencies("the cat sat on the mat the end");
        assert_eq!(freqs.get("the"), Some(&3));
        assert_eq!(freqs.get("cat"), Some(&1));
        // "on" is length 2 and kept; single-char tokens are dropped.
        assert_eq!(total, 8);
    }

    #[test]
    fn token_frequencies_of_empty_text_is_empty() {
        let (freqs, total) = token_frequencies("");
        assert!(freqs.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn tf_round_trips() {
        assert_eq!(decode_tf(&encode_tf(7)), 7);
        assert_eq!(decode_tf(&encode_tf(1)), 1);
    }

    #[test]
    fn legacy_empty_postings_value_decodes_as_one() {
        // v0 indexes wrote `&[]`; they must stay rankable rather than
        // scoring zero across the board.
        assert_eq!(decode_tf(&[]), 1);
    }

    #[test]
    fn doc_len_round_trips_and_defaults_to_zero() {
        assert_eq!(decode_doc_len(&encode_doc_len(42)), 42);
        assert_eq!(decode_doc_len(&[]), 0);
    }

    #[test]
    fn fts_def_decodes_current_layout() {
        let def = FtsDef {
            collection: "articles".into(),
            field: "body".into(),
            version: FTS_VERSION,
        };
        let bytes = postcard::to_allocvec(&def).unwrap();
        let back = decode_fts_def(&bytes).unwrap();
        assert_eq!(back.collection, "articles");
        assert_eq!(back.field, "body");
        assert_eq!(back.version, FTS_VERSION);
        assert!(back.supports_ranking());
    }

    #[test]
    fn fts_def_falls_back_to_v0_metadata() {
        // Exactly what a pre-versioning build wrote: two strings, nothing else.
        #[derive(Serialize)]
        struct Legacy {
            collection: String,
            field: String,
        }
        let bytes = postcard::to_allocvec(&Legacy {
            collection: "articles".into(),
            field: "body".into(),
        })
        .unwrap();

        let def = decode_fts_def(&bytes).unwrap();
        assert_eq!(def.collection, "articles");
        assert_eq!(def.field, "body");
        assert_eq!(def.version, 0);
        assert!(!def.supports_ranking());
    }
}
