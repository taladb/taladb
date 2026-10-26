//! Shared accounting for retained caches, pinned vector blocks and active graph
//! reservations. Queries never hold the mutex while accessing storage.
//! This estimates decoded search allocations, not process RSS.
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::vector::RetainedVectors;
use crate::vector_graph::CachedGraph;

#[cfg(any(target_arch = "wasm32", target_os = "android", target_os = "ios"))]
pub(crate) const DEFAULT_SEARCH_CACHE_BYTES: usize = 8 * 1024 * 1024;
#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
pub(crate) const DEFAULT_SEARCH_CACHE_BYTES: usize = 64 * 1024 * 1024;

#[cfg(any(target_arch = "wasm32", target_os = "android", target_os = "ios"))]
const MAX_ADAPTIVE_BYTES: u64 = 16 * 1024 * 1024;
#[cfg(not(any(target_arch = "wasm32", target_os = "android", target_os = "ios")))]
const MAX_ADAPTIVE_BYTES: u64 = 64 * 1024 * 1024;

/// Host-reported pressure, independent of the configured normal budget.
/// Recovery is explicit: a timer or app foreground event does not prove that
/// memory pressure has ended.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryPressure {
    #[default]
    Normal,
    Moderate,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VectorCachePolicy {
    Adaptive,
    Fixed,
}

/// Estimated decoded search allocations. Reservations conservatively charge the
/// full allowance of active graph loans (including large-filter bitmaps) and
/// vector-block construction. Capped filter ID sets, page windows/group keys,
/// result documents, storage and build scratch are outside these estimates.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorCacheStats {
    pub budget_bytes: usize,
    pub baseline_budget_bytes: usize,
    pub policy: VectorCachePolicy,
    pub memory_hint_bytes: Option<u64>,
    pub pressure: MemoryPressure,
    pub retained_bytes: usize,
    pub memory_budget_bytes: usize,
    pub active_bytes: usize,
    pub peak_bytes: usize,
    pub vector_indexes: usize,
    pub graph_indexes: usize,
}

/// Decoded vectors for one `collection::field`, tagged with its vector revision.
struct CachedBlock {
    generation: u64,
    vectors: Arc<RetainedVectors>,
}

enum Value {
    Vectors(CachedBlock),
    Graph(CachedGraph),
}
struct Entry {
    value: Value,
    touched: u64,
}
impl Entry {
    fn bytes(&self) -> usize {
        match &self.value {
            Value::Vectors(v) => v.vectors.bytes(),
            Value::Graph(g) => g.cache.bytes(),
        }
    }
}

pub(crate) type SharedSearchCache = Arc<Mutex<SearchCache>>;
pub(crate) struct SearchCache {
    entries: HashMap<String, Entry>,
    budget: usize,
    baseline_budget: usize,
    policy: VectorCachePolicy,
    memory_hint: Option<u64>,
    pressure: MemoryPressure,
    clock: u64,
    active: Arc<AtomicUsize>,
    vector_loans: Vec<Weak<RetainedVectors>>,
    peak: usize,
    /// Invalidation also discards loans returned by queries already running.
    pub epoch: u64,
}
impl Default for SearchCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            budget: DEFAULT_SEARCH_CACHE_BYTES,
            baseline_budget: DEFAULT_SEARCH_CACHE_BYTES,
            policy: VectorCachePolicy::Adaptive,
            memory_hint: None,
            pressure: MemoryPressure::Normal,
            clock: 0,
            active: Arc::new(AtomicUsize::new(0)),
            vector_loans: Vec::new(),
            peak: 0,
            epoch: 0,
        }
    }
}
pub(crate) fn new_shared_search_cache() -> SharedSearchCache {
    Arc::new(Mutex::new(SearchCache::default()))
}
impl SearchCache {
    pub fn budget(&self) -> usize {
        self.budget
    }
    // Retention may be disabled without disabling ANN. A small, shared 64 KiB
    // workspace still allows short walks; longer ones fall back to streaming.
    pub fn memory_budget(&self) -> usize {
        self.budget.max(64 * 1024)
    }
    fn pinned_bytes(&self) -> usize {
        self.vector_loans
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|block| {
                !self.entries.values().any(|entry| {
                    matches!(&entry.value,
                Value::Vectors(v) if Arc::ptr_eq(&v.vectors, block))
                })
            })
            .map(|block| block.bytes())
            .sum()
    }
    fn active_bytes(&self) -> usize {
        self.active
            .load(Ordering::Relaxed)
            .saturating_add(self.pinned_bytes())
    }
    fn record_peak(&mut self) {
        let stats = self.stats();
        self.peak = self
            .peak
            .max(stats.retained_bytes.saturating_add(stats.active_bytes));
    }
    /// Reserve before allocating. Eviction cannot free a block pinned by a query.
    pub fn reserve(&mut self, bytes: usize) -> Option<MemoryReservation> {
        self.trim_memory(bytes);
        if self
            .stats()
            .retained_bytes
            .saturating_add(self.active_bytes())
            .saturating_add(bytes)
            > self.memory_budget()
        {
            return None;
        }
        self.active.fetch_add(bytes, Ordering::Relaxed);
        self.record_peak();
        Some(MemoryReservation {
            active: Arc::clone(&self.active),
            bytes,
        })
    }
    pub fn reserve_graph(&mut self, minimum: usize) -> MemoryReservation {
        // Leave other retained indexes resident when they fit. Evict only if
        // a cached graph or a useful minimum workspace needs their allowance.
        self.trim_memory(minimum.max(64 * 1024).min(self.memory_budget()));
        let available = self
            .memory_budget()
            .saturating_sub(self.active_bytes())
            .saturating_sub(self.stats().retained_bytes);
        if available == 0 {
            return MemoryReservation {
                active: Arc::clone(&self.active),
                bytes: 0,
            };
        }
        // Node caches evict under their allowance, so a walk only needs room
        // for its visited set and queue. Hold a quarter back for walks that
        // start while this one runs; otherwise every overlapping query falls
        // back to an exact scan however large the budget is.
        let grant = available
            .saturating_sub(available / 4)
            .max(minimum.max(64 * 1024).min(available));
        self.reserve(grant)
            .expect("available graph reservation fits")
    }
    pub fn stats(&self) -> VectorCacheStats {
        VectorCacheStats {
            budget_bytes: self.budget,
            baseline_budget_bytes: self.baseline_budget,
            policy: self.policy,
            memory_hint_bytes: self.memory_hint,
            pressure: self.pressure,
            memory_budget_bytes: self.memory_budget(),
            active_bytes: self.active_bytes(),
            peak_bytes: self.peak,
            retained_bytes: self.entries.values().map(Entry::bytes).sum(),
            vector_indexes: self
                .entries
                .values()
                .filter(|e| matches!(e.value, Value::Vectors(_)))
                .count(),
            graph_indexes: self
                .entries
                .values()
                .filter(|e| matches!(e.value, Value::Graph(_)))
                .count(),
        }
    }
    pub fn set_budget(&mut self, budget: usize) {
        self.policy = VectorCachePolicy::Fixed;
        self.memory_hint = None;
        self.baseline_budget = budget;
        self.apply_budget(self.pressure_budget());
    }
    pub fn set_adaptive(&mut self, memory_bytes: Option<u64>) -> Result<(), crate::TalaDbError> {
        if memory_bytes == Some(0) {
            return Err(crate::TalaDbError::InvalidOperation(
                "memory hint must be positive".into(),
            ));
        }
        self.policy = VectorCachePolicy::Adaptive;
        self.memory_hint = memory_bytes;
        // Approximately 0.2% of the host's memory hint, capped per database.
        // Clamp before converting: WASM32 can receive hints above 4 GiB.
        self.baseline_budget = memory_bytes.map_or(DEFAULT_SEARCH_CACHE_BYTES, |bytes| {
            (bytes / 512).clamp(1024 * 1024, MAX_ADAPTIVE_BYTES) as usize
        });
        self.apply_budget(self.pressure_budget());
        Ok(())
    }
    pub fn notify_pressure(&mut self, pressure: MemoryPressure) {
        if self.pressure == pressure {
            return;
        }
        self.pressure = pressure;
        let budget = self.pressure_budget();
        if budget != self.budget {
            self.apply_budget(budget);
        }
    }
    fn pressure_budget(&self) -> usize {
        match self.pressure {
            MemoryPressure::Normal => self.baseline_budget,
            MemoryPressure::Moderate => self.baseline_budget / 4,
            MemoryPressure::Critical => 0,
        }
    }
    fn apply_budget(&mut self, budget: usize) {
        self.budget = budget;
        self.epoch = self.epoch.wrapping_add(1);
        self.trim(0);
        self.trim_memory(0);
        // Existing loans keep their reservation until exit. Reducing a live
        // budget prevents new admissions; it cannot revoke borrowed memory.
        self.peak = self
            .stats()
            .retained_bytes
            .saturating_add(self.active_bytes());
    }
    fn trim(&mut self, incoming: usize) {
        while self.stats().retained_bytes.saturating_add(incoming) > self.budget {
            let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.touched)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.entries.remove(&key);
        }
    }
    fn trim_memory(&mut self, incoming: usize) {
        self.vector_loans.retain(|block| block.strong_count() > 0);
        while self
            .stats()
            .retained_bytes
            .saturating_add(self.active_bytes())
            .saturating_add(incoming)
            > self.memory_budget()
        {
            let Some(key) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.touched)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.entries.remove(&key);
        }
    }
    fn insert(&mut self, key: String, value: Value) {
        let entry = Entry {
            value,
            touched: self.clock,
        };
        if entry.bytes() > self.budget || self.budget == 0 {
            return;
        }
        self.entries.remove(&key);
        self.trim(entry.bytes());
        self.trim_memory(entry.bytes());
        if self.active_bytes().saturating_add(entry.bytes()) > self.memory_budget() {
            return;
        }
        self.clock = self.clock.wrapping_add(1);
        self.entries.insert(
            key,
            Entry {
                touched: self.clock,
                ..entry
            },
        );
        self.record_peak();
    }
    pub fn vectors(&mut self, key: &str, generation: u64) -> Option<Arc<RetainedVectors>> {
        let key = format!("v:{key}");
        if self
            .entries
            .get(&key)
            .is_some_and(|e| matches!(&e.value, Value::Vectors(v) if v.generation < generation))
        {
            self.entries.remove(&key);
            return None;
        }
        let entry = self.entries.get_mut(&key)?;
        match &entry.value {
            Value::Vectors(v) if v.generation == generation => {
                self.clock = self.clock.wrapping_add(1);
                entry.touched = self.clock;
                if !self
                    .vector_loans
                    .iter()
                    .any(|loan| loan.ptr_eq(&Arc::downgrade(&v.vectors)))
                {
                    self.vector_loans.push(Arc::downgrade(&v.vectors));
                }
                Some(Arc::clone(&v.vectors))
            }
            _ => None,
        }
    }
    /// The block for exactly `generation`, for a reader that only borrows it.
    /// Unlike [`Self::vectors`] it neither refreshes the entry's recency nor
    /// drops an older generation, so eviction order stays what exact searches
    /// made it. The loan is still recorded: a block evicted while borrowed
    /// stays counted as active memory until the reader drops it.
    pub fn peek_vectors(&mut self, key: &str, generation: u64) -> Option<Arc<RetainedVectors>> {
        let vectors = match &self.entries.get(&format!("v:{key}"))?.value {
            Value::Vectors(v) if v.generation == generation => Arc::clone(&v.vectors),
            _ => return None,
        };
        let loan = Arc::downgrade(&vectors);
        if !self.vector_loans.iter().any(|l| l.ptr_eq(&loan)) {
            self.vector_loans.push(loan);
        }
        Some(vectors)
    }
    pub fn insert_vectors(
        &mut self,
        key: &str,
        generation: u64,
        vectors: RetainedVectors,
        epoch: u64,
    ) {
        if self.epoch != epoch {
            return;
        }
        let key = format!("v:{key}");
        if self
            .entries
            .get(&key)
            .is_some_and(|e| matches!(&e.value, Value::Vectors(v) if v.generation > generation))
        {
            return;
        }
        self.insert(
            key,
            Value::Vectors(CachedBlock {
                generation,
                vectors: Arc::new(vectors),
            }),
        );
    }
    pub fn take_graph(&mut self, table: &str) -> Option<CachedGraph> {
        self.entries
            .remove(&format!("g:{table}"))
            .and_then(|e| match e.value {
                Value::Graph(g) => Some(g),
                _ => None,
            })
    }
    pub fn insert_graph(&mut self, table: &str, graph: CachedGraph, epoch: u64) {
        if self.epoch != epoch {
            return;
        }
        let key = format!("g:{table}");
        if self
            .entries
            .get(&key)
            .is_some_and(|e| matches!(&e.value, Value::Graph(g) if g.revision > graph.revision))
        {
            return;
        }
        self.insert(key, Value::Graph(graph));
    }
    pub fn evict_field(&mut self, collection: &str, field: &str) {
        let vector = format!("v:{collection}::{field}");
        let graph = format!("g:hnsw::{collection}::{field}::");
        self.epoch = self.epoch.wrapping_add(1);
        self.entries
            .retain(|k, _| *k != vector && !k.starts_with(&graph));
    }
    pub fn evict_graph(&mut self, table: &str) {
        // Also reject a build loan returned after cancellation released its
        // write lock. Active-index entries remain available for subsequent reads.
        self.epoch = self.epoch.wrapping_add(1);
        self.entries.remove(&format!("g:{table}"));
    }
}

/// Releases on success, error or unwind without re-entering the cache mutex.
pub(crate) struct MemoryReservation {
    active: Arc<AtomicUsize>,
    pub bytes: usize,
}
impl Drop for MemoryReservation {
    fn drop(&mut self) {
        self.active.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adaptive_hints_are_capped_and_pressure_is_idempotent_with_explicit_recovery() {
        let mut cache = SearchCache::default();
        for (hint, expected) in [
            (1, 1024 * 1024),
            (512 * 1024 * 1024, 1024 * 1024),
            (2 * 1024 * 1024 * 1024, 4 * 1024 * 1024),
            (u64::MAX, MAX_ADAPTIVE_BYTES as usize),
        ] {
            cache.set_adaptive(Some(hint)).unwrap();
            assert_eq!(cache.stats().baseline_budget_bytes, expected);
            assert_eq!(cache.budget(), expected);
            assert_eq!(cache.stats().policy, VectorCachePolicy::Adaptive);
        }
        let before = cache.stats();
        assert!(cache.set_adaptive(Some(0)).is_err());
        assert_eq!(cache.stats().memory_hint_bytes, before.memory_hint_bytes);
        assert_eq!(cache.budget(), before.budget_bytes);
        cache.set_budget(8 * 1024 * 1024);
        cache.insert_vectors("a", 1, block(128 * 1024), cache.epoch);
        cache.notify_pressure(MemoryPressure::Moderate);
        assert_eq!(cache.budget(), 2 * 1024 * 1024);
        assert_eq!(cache.stats().retained_bytes, 0);
        let epoch = cache.epoch;
        cache.notify_pressure(MemoryPressure::Moderate);
        assert_eq!(cache.epoch, epoch);
        cache.notify_pressure(MemoryPressure::Critical);
        assert_eq!(cache.budget(), 0);
        assert_eq!(cache.memory_budget(), 64 * 1024);
        // Changing policy while pressure is active must not restore retention.
        cache.set_adaptive(Some(2 * 1024 * 1024 * 1024)).unwrap();
        assert_eq!(cache.budget(), 0);
        cache.notify_pressure(MemoryPressure::Normal);
        assert_eq!(cache.budget(), 4 * 1024 * 1024);
        cache.set_adaptive(None).unwrap();
        assert_eq!(cache.budget(), DEFAULT_SEARCH_CACHE_BYTES);
    }

    #[test]
    fn pressure_invalidates_returning_loans_and_preserves_live_accounting() {
        let mut cache = SearchCache::default();
        cache.set_budget(1024 * 1024);
        let epoch = cache.epoch;
        let loan = cache.reserve_graph(0);
        cache.notify_pressure(MemoryPressure::Critical);
        assert_eq!(cache.stats().active_bytes, 768 * 1024);
        assert!(cache.reserve(1).is_none());
        assert_eq!(cache.reserve_graph(0).bytes, 0);
        cache.notify_pressure(MemoryPressure::Normal);
        drop(loan);
        cache.insert_vectors("obsolete", 1, block(10), epoch);
        assert_eq!(cache.stats().retained_bytes, 0);
        cache.insert_vectors("fresh", 1, block(10), cache.epoch);
        assert!(cache.stats().retained_bytes > 0);
        assert_eq!(cache.stats().active_bytes, 0);
    }
    // Shaped like a cosine block: two dimensions plus one norm per vector.
    fn block(n: usize) -> RetainedVectors {
        RetainedVectors {
            block: crate::vector::VectorBlock {
                ids: vec![ulid::Ulid::from(1); n],
                values: vec![1.0; n * 2],
                dimensions: 2,
            },
            norms: vec![2.0; n],
        }
    }
    #[test]
    fn cached_norms_are_charged_to_retained_pinned_and_peak_bytes() {
        let n = 1000;
        // 16 B id + 2 × 4 B values + 4 B norm per vector.
        assert_eq!(block(n).bytes(), n * 28);
        assert_eq!(block(n).bytes(), block(n).block.bytes() + n * 4);

        // A budget that holds the vectors but not their norms admits nothing.
        let mut cache = SearchCache::default();
        cache.set_budget(block(n).block.bytes());
        cache.insert_vectors("a", 1, block(n), cache.epoch);
        assert_eq!(cache.stats().retained_bytes, 0);

        cache.set_budget(block(n).bytes());
        cache.insert_vectors("a", 1, block(n), cache.epoch);
        assert_eq!(cache.stats().retained_bytes, n * 28);
        assert_eq!(cache.stats().peak_bytes, n * 28);
        // Evicted while a query holds it: still counted, norms included.
        let pinned = cache.vectors("a", 1).unwrap();
        cache.set_budget(0);
        assert_eq!(cache.stats().retained_bytes, 0);
        assert_eq!(cache.stats().active_bytes, n * 28);
        drop(pinned);
        assert_eq!(cache.stats().active_bytes, 0);
    }
    #[test]
    fn eviction_preserves_recent_indexes_and_replacements_do_not_double_count() {
        let mut cache = SearchCache::default();
        cache.set_budget(block(10).bytes() * 2);
        let epoch = cache.epoch;
        cache.insert_vectors("a", 1, block(10), epoch);
        cache.insert_vectors("b", 1, block(10), epoch);
        assert!(cache.vectors("a", 1).is_some());
        cache.insert_vectors("c", 1, block(10), epoch);
        assert!(cache.vectors("a", 1).is_some());
        assert!(cache.vectors("b", 1).is_none());
        cache.insert_vectors("a", 2, block(10), epoch);
        assert!(cache.vectors("c", 1).is_some());
        assert_eq!(cache.stats().retained_bytes, cache.budget());
        cache.set_budget(0);
        assert_eq!(cache.stats().retained_bytes, 0);
        cache.insert_vectors("a", 3, block(10), epoch);
        assert_eq!(cache.stats().retained_bytes, 0);
    }

    #[test]
    fn pinned_blocks_and_builders_share_admission_and_released_loans_restore_space() {
        let mut cache = SearchCache::default();
        let n = 4096;
        let bytes = block(n).bytes();
        cache.set_budget(bytes);
        cache.insert_vectors("a", 1, block(n), cache.epoch);
        let pinned = cache.vectors("a", 1).unwrap();
        let again = cache.vectors("a", 1).unwrap();
        assert!(cache.reserve(bytes).is_none());
        assert_eq!(cache.stats().retained_bytes, 0);
        assert_eq!(cache.stats().active_bytes, bytes); // counted once
        drop(again);
        drop(pinned);
        let reserved = cache.reserve(bytes).unwrap();
        assert_eq!(cache.stats().active_bytes, bytes);
        assert!(cache.reserve(1).is_none());
        drop(reserved);
        assert_eq!(cache.stats().active_bytes, 0);
        assert!(cache.reserve(bytes).is_some());
        assert!(cache.stats().peak_bytes <= cache.memory_budget());
    }
    #[test]
    fn lowering_a_live_budget_blocks_admission_without_panicking_or_losing_accounting() {
        let mut cache = SearchCache::default();
        cache.set_budget(1024 * 1024);
        let loan = cache.reserve_graph(0);
        cache.set_budget(0);
        assert_eq!(cache.stats().active_bytes, 768 * 1024);
        assert_eq!(cache.reserve_graph(0).bytes, 0);
        assert!(cache.reserve(1).is_none());
        drop(loan);
        assert_eq!(cache.stats().active_bytes, 0);
        assert_eq!(cache.reserve_graph(0).bytes, 64 * 1024);
    }
    #[test]
    fn overlapping_graph_walks_share_the_budget_instead_of_starving() {
        let mut cache = SearchCache::default();
        cache.set_budget(1024 * 1024);
        let first = cache.reserve_graph(0);
        let second = cache.reserve_graph(0);
        let third = cache.reserve_graph(0);
        assert_eq!(first.bytes, 768 * 1024);
        assert_eq!(second.bytes, 192 * 1024);
        assert_eq!(third.bytes, 64 * 1024); // the 64 KiB floor takes the remainder
        assert!(cache.stats().active_bytes <= cache.memory_budget());
        // A warm graph keeps its decoded nodes when they fit.
        drop((first, second, third));
        assert_eq!(cache.reserve_graph(900 * 1024).bytes, 900 * 1024);
    }
    #[test]
    fn reservation_cleanup_survives_unwind() {
        let shared = new_shared_search_cache();
        let result = std::panic::catch_unwind({
            let shared = shared.clone();
            move || {
                let _loan = shared.lock().unwrap().reserve_graph(0);
                panic!("injected");
            }
        });
        assert!(result.is_err());
        assert_eq!(shared.lock().unwrap().stats().active_bytes, 0);
    }
    #[test]
    fn peeking_borrows_one_generation_without_changing_eviction_order() {
        let mut cache = SearchCache::default();
        cache.set_budget(block(10).bytes() * 2);
        let epoch = cache.epoch;
        cache.insert_vectors("a", 1, block(10), epoch);
        cache.insert_vectors("b", 1, block(10), epoch);
        // Unlike `vectors`, a peek leaves `a` least recently used.
        assert!(cache.peek_vectors("a", 1).is_some());
        cache.insert_vectors("c", 1, block(10), epoch);
        assert!(cache.peek_vectors("a", 1).is_none());
        assert!(cache.peek_vectors("b", 1).is_some());
        // Other generations are neither returned nor dropped.
        assert!(cache.peek_vectors("b", 0).is_none());
        assert!(cache.peek_vectors("b", 2).is_none());
        assert_eq!(cache.stats().vector_indexes, 2);
        // A peeked block evicted while borrowed stays charged until dropped.
        let borrowed = cache.peek_vectors("b", 1).unwrap();
        cache.set_budget(0);
        assert_eq!(cache.stats().retained_bytes, 0);
        assert_eq!(cache.stats().active_bytes, block(10).bytes());
        assert!(cache.reserve(64 * 1024 - block(10).bytes() + 1).is_none());
        drop(borrowed);
        assert_eq!(cache.stats().active_bytes, 0);
    }
    #[test]
    fn obsolete_vectors_are_freed_without_an_old_snapshot_evicting_newer_vectors() {
        let mut cache = SearchCache::default();
        cache.insert_vectors("a", 2, block(10), cache.epoch);
        assert!(cache.vectors("a", 1).is_none());
        assert!(cache.vectors("a", 2).is_some());
        assert!(cache.vectors("a", 3).is_none());
        assert_eq!(cache.stats().retained_bytes, 0);
    }
}
