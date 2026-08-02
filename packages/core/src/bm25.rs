//! BM25 relevance scoring for the full-text index.
//!
//! The inverted index in [`crate::fts`] answers *which* documents contain a
//! token. This module answers *how well* each one matches, so `$contains`
//! results can be ranked instead of returned in arbitrary ULID order.
//!
//! Scoring needs three inputs that a plain postings list does not carry:
//!
//! - `tf` — how often the token occurs in the document (stored in the
//!   postings value; see [`crate::fts::encode_tf`])
//! - `|D|` — the document's token count (the `fts_len::` table)
//! - `N`, `avgdl` — corpus size and mean length (the `fts_stats::` table)
//!
//! `df` (how many documents contain the token) needs no storage: the query
//! already range-scans a token's postings, so its length *is* `df`.

use serde::{Deserialize, Serialize};

/// Tuning knobs for BM25. The defaults are the values Lucene and
/// Elasticsearch ship with, and are a sensible choice for prose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bm25Params {
    /// Term-frequency saturation. Higher values let repeated terms keep
    /// adding score for longer; lower values flatten quickly after the
    /// first occurrence.
    pub k1: f32,
    /// Length normalisation. `0.0` ignores document length entirely,
    /// `1.0` normalises fully by `|D| / avgdl`.
    pub b: f32,
}

impl Default for Bm25Params {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

/// Corpus statistics for a single indexed field, persisted in
/// `fts_stats::<collection>::<field>`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct FtsStats {
    /// Number of documents that have a value for the indexed field.
    pub doc_count: u64,
    /// Sum of every indexed document's token count.
    pub total_len: u64,
}

impl FtsStats {
    /// Mean token count per document, or `0.0` for an empty corpus.
    pub fn avg_doc_len(&self) -> f32 {
        if self.doc_count == 0 {
            0.0
        } else {
            self.total_len as f32 / self.doc_count as f32
        }
    }

    /// Record a newly indexed document.
    pub fn add_doc(&mut self, len: u32) {
        self.doc_count = self.doc_count.saturating_add(1);
        self.total_len = self.total_len.saturating_add(u64::from(len));
    }

    /// Remove a document from the statistics.
    pub fn remove_doc(&mut self, len: u32) {
        self.doc_count = self.doc_count.saturating_sub(1);
        self.total_len = self.total_len.saturating_sub(u64::from(len));
    }
}

/// Inverse document frequency.
///
/// This is the Lucene form — `ln(1 + (N - df + 0.5) / (df + 0.5))` — rather
/// than the textbook one. The `1 +` matters: without it a term appearing in
/// more than half the corpus scores *negative*, so a document could be
/// pushed down the ranking for containing a query term.
pub fn idf(doc_count: u64, doc_freq: u64) -> f32 {
    let n = doc_count as f32;
    // A stale df can exceed N; clamping keeps the numerator non-negative.
    let df = (doc_freq as f32).min(n);
    // `ln_1p(x)` rather than `(1.0 + x).ln()`: for a very common term the ratio
    // approaches zero, where adding 1 first discards most of its significant
    // bits and the log of the rounded sum loses precision. `ln_1p` is computed
    // for exactly this case.
    ((n - df + 0.5) / (df + 0.5)).ln_1p()
}

/// BM25 contribution of one query term to one document.
///
/// `idf` is passed in rather than recomputed so a caller scoring many
/// documents for the same term computes it once.
pub fn term_score(tf: u32, doc_len: u32, avg_doc_len: f32, idf: f32, params: &Bm25Params) -> f32 {
    if tf == 0 {
        return 0.0;
    }
    let tf = tf as f32;
    // With no corpus statistics yet, treat the document as average length.
    let length_ratio = if avg_doc_len > 0.0 {
        doc_len as f32 / avg_doc_len
    } else {
        1.0
    };
    let denominator = tf + params.k1 * (1.0 - params.b + params.b * length_ratio);
    if denominator <= 0.0 {
        return 0.0;
    }
    idf * (tf * (params.k1 + 1.0)) / denominator
}

// ---------------------------------------------------------------------------
// Reciprocal rank fusion
// ---------------------------------------------------------------------------

/// How to combine a text ranking with a vector ranking.
///
/// Fusion works on *ranks*, not scores. That is the whole point: BM25 is
/// unbounded and cosine lives in `[-1, 1]`, so any attempt to add the raw
/// numbers needs a normalisation step that quietly changes meaning as the
/// corpus grows. Ranks are already comparable.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RrfParams {
    /// Smoothing constant. 60 is the value from the original RRF paper and
    /// the de-facto default. Larger values flatten the contribution of the
    /// top ranks relative to the tail.
    pub k: f32,
    /// Relative weight of the text ranking.
    pub text_weight: f32,
    /// Relative weight of the vector ranking.
    pub vector_weight: f32,
}

impl Default for RrfParams {
    fn default() -> Self {
        Self {
            k: 60.0,
            text_weight: 1.0,
            vector_weight: 1.0,
        }
    }
}

/// One retriever's contribution to a document's fused score.
///
/// `rank` is zero-based; the `+ 1.0` restores the one-based rank the RRF
/// formula is defined over, so the best hit contributes `weight / (k + 1)`.
pub fn rrf_contribution(rank: usize, k: f32, weight: f32) -> f32 {
    let denominator = k + rank as f32 + 1.0;
    if denominator <= 0.0 {
        return 0.0;
    }
    weight / denominator
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Bm25Params = Bm25Params { k1: 1.2, b: 0.75 };

    #[test]
    fn idf_never_goes_negative_for_common_terms() {
        // A term in every document carries no information, but must not
        // actively penalise the documents that contain it.
        assert!(idf(100, 100) >= 0.0);
        assert!(idf(100, 99) >= 0.0);
        assert!(idf(1, 1) >= 0.0);
    }

    #[test]
    fn idf_survives_a_doc_freq_larger_than_the_corpus() {
        assert!(idf(10, 50).is_finite());
        assert!(idf(10, 50) >= 0.0);
    }

    #[test]
    fn rarer_terms_score_higher() {
        let rare = idf(1000, 5);
        let common = idf(1000, 800);
        assert!(rare > common, "rare {rare} should beat common {common}");
    }

    #[test]
    fn term_frequency_saturates() {
        let one = term_score(1, 100, 100.0, 1.0, &P);
        let two = term_score(2, 100, 100.0, 1.0, &P);
        let ten = term_score(10, 100, 100.0, 1.0, &P);

        assert!(two > one);
        assert!(ten > two);
        // The whole point of k1: the 10th occurrence is worth far less than
        // the 2nd, so keyword stuffing cannot dominate the ranking.
        assert!(two < one * 2.0, "tf=2 should not be worth twice tf=1");
        assert!(ten < one * (P.k1 + 1.0));
    }

    #[test]
    fn shorter_documents_win_at_equal_term_frequency() {
        let short = term_score(3, 20, 100.0, 1.0, &P);
        let long = term_score(3, 500, 100.0, 1.0, &P);
        assert!(short > long, "short {short} should beat long {long}");
    }

    #[test]
    fn length_normalisation_can_be_disabled() {
        let no_norm = Bm25Params { k1: 1.2, b: 0.0 };
        let short = term_score(3, 20, 100.0, 1.0, &no_norm);
        let long = term_score(3, 500, 100.0, 1.0, &no_norm);
        assert!((short - long).abs() < f32::EPSILON);
    }

    #[test]
    fn absent_terms_contribute_nothing() {
        assert_eq!(term_score(0, 100, 100.0, 5.0, &P), 0.0);
    }

    #[test]
    fn an_empty_corpus_still_produces_a_finite_score() {
        // avg_doc_len == 0.0 happens on the very first indexed document.
        assert!(term_score(1, 10, 0.0, 1.0, &P).is_finite());
    }

    #[test]
    fn stats_track_mean_length() {
        let mut s = FtsStats::default();
        assert_eq!(s.avg_doc_len(), 0.0);

        s.add_doc(10);
        s.add_doc(20);
        assert_eq!(s.doc_count, 2);
        assert_eq!(s.avg_doc_len(), 15.0);

        s.remove_doc(20);
        assert_eq!(s.doc_count, 1);
        assert_eq!(s.avg_doc_len(), 10.0);
    }

    #[test]
    fn rrf_rewards_better_ranks() {
        let d = RrfParams::default();
        let first = rrf_contribution(0, d.k, 1.0);
        let second = rrf_contribution(1, d.k, 1.0);
        let hundredth = rrf_contribution(99, d.k, 1.0);
        assert!(first > second);
        assert!(second > hundredth);
        assert!(hundredth > 0.0, "every rank still contributes something");
    }

    #[test]
    fn rrf_matches_the_published_formula() {
        // Rank 0 (i.e. first place) must be weight / (k + 1).
        assert!((rrf_contribution(0, 60.0, 1.0) - 1.0 / 61.0).abs() < 1e-9);
        assert!((rrf_contribution(4, 60.0, 1.0) - 1.0 / 65.0).abs() < 1e-9);
    }

    #[test]
    fn rrf_weights_scale_a_retriever() {
        let unweighted = rrf_contribution(0, 60.0, 1.0);
        let doubled = rrf_contribution(0, 60.0, 2.0);
        assert!((doubled - unweighted * 2.0).abs() < 1e-9);
        assert_eq!(rrf_contribution(0, 60.0, 0.0), 0.0, "zero weight disables");
    }

    #[test]
    fn agreeing_retrievers_beat_a_single_strong_hit() {
        let k = 60.0;
        // Ranked 2nd by both retrievers...
        let agreed = rrf_contribution(1, k, 1.0) + rrf_contribution(1, k, 1.0);
        // ...beats being 1st in one and absent from the other.
        let lopsided = rrf_contribution(0, k, 1.0);
        assert!(
            agreed > lopsided,
            "consensus {agreed} should outrank a lone top hit {lopsided}"
        );
    }

    #[test]
    fn stats_do_not_underflow_when_over_removed() {
        let mut s = FtsStats::default();
        s.remove_doc(5);
        assert_eq!(s.doc_count, 0);
        assert_eq!(s.total_len, 0);
        assert_eq!(s.avg_doc_len(), 0.0);
    }
}
