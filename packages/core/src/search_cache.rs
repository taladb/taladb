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
    fn partial(&self) -> bool {
        matches!(&self.value, Value::Vectors(v) if v.vectors.partial)
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
        // Partial vector blocks occupy memory that was idle when they were
        // built; a graph walk or build gets all of it back, as if they had
        // never been retained.
        self.entries.retain(|_, entry| !entry.partial());
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
    /// Reserve a partial vector block: up to `wanted` vectors of
    /// `vector_bytes` each from memory that no entry, reservation or loan
    /// holds, and at least `minimum`. Returns the reservation and the number
    /// of vectors it covers.
    ///
    /// It is refused while a graph is resident or anything is reserved or
    /// pinned, and it never evicts, so it cannot displace another entry. It
    /// leaves the 64 KiB walk workspace idle, so a graph walk that starts
    /// while the block is built still gets a working allowance instead of
    /// falling back to exact search. Once retained it is the first entry
    /// evicted, and graph reservations drop it.
    pub fn reserve_partial(
        &mut self,
        vector_bytes: usize,
        wanted: usize,
        minimum: usize,
    ) -> Option<(MemoryReservation, usize)> {
        self.vector_loans.retain(|block| block.strong_count() > 0);
        if vector_bytes == 0
            || self.active_bytes() > 0
            || self
                .entries
                .values()
                .any(|entry| matches!(entry.value, Value::Graph(_)))
        {
            return None;
        }
        let retained = self.stats().retained_bytes;
        let idle = self.budget.saturating_sub(retained).min(
            self.memory_budget()
                .saturating_sub(retained)
                .saturating_sub(64 * 1024),
        );
        let vectors = (idle / vector_bytes).min(wanted);
        if vectors == 0 || vectors < minimum {
            return None;
        }
        Some((self.reserve(vectors * vector_bytes)?, vectors))
    }
    /// Admission for an unfiltered exact search that found no block of
    /// `generation` for `key`, under one lock: a full block of `count`
    /// vectors with cosine norms, then without them, then a prefix
    /// ([`Self::reserve_partial`], at least `minimum_partial` vectors).
    ///
    /// An older generation's block for `key` is dropped first, as
    /// [`Self::vectors`] would have dropped it. If no reader still holds it,
    /// its buffers are emptied and fitted to the reservation instead of
    /// freed, so the rebuild writes into memory that is already mapped. A
    /// buffer too small for the new block is freed, and the build allocates
    /// it as it would without a stale block.
    pub fn reserve_block(
        &mut self,
        key: &str,
        generation: u64,
        count: usize,
        dimensions: usize,
        cosine: bool,
        minimum_partial: usize,
    ) -> Option<BlockLease> {
        // Unwrapped before admission: a stale block still held here would
        // count as pinned memory and refuse the very reservation it could
        // serve. `try_unwrap` succeeds only for the last reference, and loans
        // are weak and upgraded only under this lock, so no reader can appear
        // later; a block a reader still holds is released to that reader.
        let stale = self
            .take_stale(key, generation)
            .and_then(|block| Arc::try_unwrap(block).ok());
        let estimate = count.checked_mul(dimensions.saturating_mul(4).saturating_add(16));
        let mut lease = None;
        if cosine {
            lease = self
                .reserve_full(estimate.and_then(|n| n.checked_add(count.checked_mul(4)?)))
                .map(|r| (r, count, true));
        }
        if lease.is_none() {
            lease = self.reserve_full(estimate).map(|r| (r, count, false));
        }
        if lease.is_none() {
            let norm_bytes = if cosine { 4 } else { 0 };
            let vector_bytes = dimensions.saturating_mul(4).saturating_add(16 + norm_bytes);
            lease = self
                .reserve_partial(vector_bytes, count, minimum_partial)
                .map(|(r, vectors)| (r, vectors, cosine));
        }
        let (reservation, capacity, norms) = lease?;
        let block = stale
            .filter(|old| old.block.dimensions == dimensions)
            .map(|old| RetainedVectors {
                block: crate::vector::VectorBlock {
                    ids: refit(old.block.ids, capacity),
                    values: refit(old.block.values, capacity.saturating_mul(dimensions)),
                    dimensions,
                },
                norms: refit(old.norms, if norms { capacity } else { 0 }),
                partial: false,
            })
            .filter(|block| block.bytes() <= reservation.bytes)
            .unwrap_or_default();
        Some(BlockLease {
            reservation,
            capacity,
            norms,
            block,
        })
    }
    /// A full-block reservation, if it fits the retention budget.
    fn reserve_full(&mut self, bytes: Option<usize>) -> Option<MemoryReservation> {
        let budget = self.budget;
        bytes
            .filter(|&n| n <= budget && budget > 0)
            .and_then(|n| self.reserve(n))
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
        // A partial block was sized to the memory idle under the old budget;
        // the next exact search sizes a new one.
        self.entries.retain(|_, entry| !entry.partial());
        self.trim(0);
        self.trim_memory(0);
        // Existing loans keep their reservation until exit. Reducing a live
        // budget prevents new admissions; it cannot revoke borrowed memory.
        self.peak = self
            .stats()
            .retained_bytes
            .saturating_add(self.active_bytes());
    }
    /// The next entry to evict: partial vector blocks first, then the least
    /// recently used, so a partial block only ever gives way.
    fn victim(&self) -> Option<String> {
        self.entries
            .iter()
            .min_by_key(|(_, e)| (!e.partial(), e.touched))
            .map(|(k, _)| k.clone())
    }
    fn trim(&mut self, incoming: usize) {
        while self.stats().retained_bytes.saturating_add(incoming) > self.budget {
            let Some(key) = self.victim() else {
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
            let Some(key) = self.victim() else {
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
        if entry.partial() {
            // Admitted only into idle memory: a partial block never evicts.
            // A graph cached while the prefix was built makes it give way, as
            // `reserve_partial` would have refused it.
            let retained = self.stats().retained_bytes.saturating_add(entry.bytes());
            if retained > self.budget
                || retained.saturating_add(self.active_bytes()) > self.memory_budget()
                || self
                    .entries
                    .values()
                    .any(|entry| matches!(entry.value, Value::Graph(_)))
            {
                return;
            }
        } else {
            self.trim(entry.bytes());
            self.trim_memory(entry.bytes());
            if self.active_bytes().saturating_add(entry.bytes()) > self.memory_budget() {
                return;
            }
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
        drop(self.take_stale(key, generation));
        self.current_vectors(key, generation)
    }
    /// Remove and return `key`'s block if it is older than `generation`.
    fn take_stale(&mut self, key: &str, generation: u64) -> Option<Arc<RetainedVectors>> {
        let key = format!("v:{key}");
        if !self
            .entries
            .get(&key)
            .is_some_and(|e| matches!(&e.value, Value::Vectors(v) if v.generation < generation))
        {
            return None;
        }
        match self.entries.remove(&key)?.value {
            Value::Vectors(v) => Some(v.vectors),
            Value::Graph(_) => None,
        }
    }
    /// [`Self::vectors`] for a search that rebuilds the block on a miss: an
    /// older generation's block stays resident, and charged, until
    /// [`Self::reserve_block`] drops it or reuses its buffers.
    pub fn current_vectors(&mut self, key: &str, generation: u64) -> Option<Arc<RetainedVectors>> {
        let key = format!("v:{key}");
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
        if self.epoch != epoch || (vectors.partial && vectors.block.ids.is_empty()) {
            return;
        }
        let key = format!("v:{key}");
        // Never replace a newer block, nor a complete block with a partial one.
        if self.entries.get(&key).is_some_and(|e| {
            matches!(&e.value, Value::Vectors(v) if v.generation > generation
                || (v.generation == generation && !v.vectors.partial && vectors.partial))
        }) {
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

/// What [`SearchCache::reserve_block`] admitted for one block build.
pub(crate) struct BlockLease {
    pub reservation: MemoryReservation,
    /// Vectors the block may hold; fewer than the table holds is a prefix.
    pub capacity: usize,
    /// Whether the block carries cosine norms.
    pub norms: bool,
    /// Empty buffers to build into: a stale block's, fitted to the
    /// reservation, or new ones with no capacity.
    pub block: RetainedVectors,
}

/// `buffer` emptied and shrunk to `len` elements, or a new empty one if it
/// is too small (growing it could copy it, which saves nothing).
fn refit<T>(mut buffer: Vec<T>, len: usize) -> Vec<T> {
    if buffer.capacity() < len {
        return Vec::new();
    }
    buffer.clear();
    buffer.shrink_to(len);
    buffer
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
            partial: false,
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
    fn partial(n: usize) -> RetainedVectors {
        RetainedVectors {
            partial: true,
            ..block(n)
        }
    }
    fn graph() -> CachedGraph {
        CachedGraph {
            revision: 1,
            cache: Box::new(crate::vector_graph::NodeCache::with_budget(0)),
        }
    }
    #[test]
    fn partial_blocks_take_only_idle_memory_and_never_evict() {
        let (per, walk) = (block(1).bytes(), 64 * 1024);
        let budget = walk + per * 100;
        let mut cache = SearchCache::default();
        cache.set_budget(budget);
        cache.insert_vectors("full", 1, block(30), cache.epoch);
        // Sized to what is idle beyond the walk workspace, bounded by the
        // table and the minimum.
        let (reservation, n) = cache.reserve_partial(per, 10_000, 10).unwrap();
        assert_eq!((reservation.bytes, n), (per * 70, 70));
        // Anything live, reserved or pinned, refuses a second one.
        assert!(cache.reserve_partial(per, 10_000, 1).is_none());
        drop(reservation);
        assert!(cache.reserve_partial(per, 10_000, 71).is_none());
        assert_eq!(cache.reserve_partial(per, 20, 1).unwrap().1, 20);
        assert!(cache.reserve_partial(0, 20, 1).is_none());
        let pinned = cache.vectors("full", 1).unwrap();
        cache.insert_vectors("other", 1, block(40), cache.epoch);
        cache.entries.remove("v:full");
        assert!(cache.reserve_partial(per, 10_000, 1).is_none(), "pinned");
        drop(pinned);
        assert_eq!(cache.reserve_partial(per, 10_000, 1).unwrap().1, 60);
        // A partial insert that does not fit beside the others is refused
        // rather than evicting them.
        let over = (budget - per * 40) / per + 1;
        cache.insert_vectors("p", 1, partial(over), cache.epoch);
        assert_eq!(cache.stats().vector_indexes, 1);
        assert_eq!(cache.stats().retained_bytes, per * 40);
        cache.insert_vectors("p", 1, partial(60), cache.epoch);
        assert_eq!(cache.stats().retained_bytes, per * 100);
        // An empty prefix is never retained.
        cache.insert_vectors("q", 1, partial(0), cache.epoch);
        assert!(cache.vectors("q", 1).is_none());
        // Budgets inside the walk workspace never hold a prefix.
        cache.set_budget(walk);
        assert!(cache.reserve_partial(per, 10_000, 1).is_none());
    }
    #[test]
    fn a_resident_graph_refuses_partial_blocks_and_graph_reservations_reclaim_them() {
        let per = block(1).bytes();
        let budget = per * 10_000;
        let mut cache = SearchCache::default();
        cache.set_budget(budget);
        cache.insert_graph("g", graph(), cache.epoch);
        assert_eq!(cache.stats().graph_indexes, 1);
        assert!(cache.reserve_partial(per, 10_000, 1).is_none());
        cache.take_graph("g");
        let (reservation, held) = cache.reserve_partial(per, 10_000, 1).unwrap();
        assert_eq!(held, (budget - 64 * 1024) / per);
        // A walk starting meanwhile still gets the workspace (three quarters
        // of it, the rest held back for overlapping walks).
        let walk = cache.reserve_graph(0);
        assert!(walk.bytes >= 48 * 1024, "{}", walk.bytes);
        drop(walk);
        drop(reservation);
        cache.insert_vectors("p", 1, partial(held), cache.epoch);
        assert_eq!(cache.stats().retained_bytes, per * held);
        // The walk gets the allowance it would have had without the block.
        let lease = cache.reserve_graph(0);
        assert_eq!(cache.stats().vector_indexes, 0);
        assert_eq!(lease.bytes, budget - budget / 4);
    }
    #[test]
    fn partial_blocks_are_evicted_first_and_dropped_when_the_budget_changes() {
        let per = block(1).bytes();
        let mut cache = SearchCache::default();
        cache.set_budget(per * 100);
        let epoch = cache.epoch;
        cache.insert_vectors("old", 1, block(30), epoch);
        cache.insert_vectors("p", 1, partial(40), epoch);
        // `p` is the most recently used, yet it gives way first.
        assert!(cache.vectors("p", 1).is_some());
        cache.insert_vectors("new", 1, block(40), epoch);
        assert!(cache.vectors("p", 1).is_none());
        assert!(cache.vectors("old", 1).is_some());
        assert!(cache.vectors("new", 1).is_some());
        // A complete block of the same generation is never replaced by a
        // partial one; a newer generation's prefix replaces an older block.
        cache.insert_vectors("old", 1, partial(10), cache.epoch);
        assert_eq!(cache.stats().retained_bytes, per * 70);
        cache.insert_vectors("old", 2, partial(10), cache.epoch);
        assert_eq!(cache.stats().retained_bytes, per * 50);
        // Any budget change drops prefixes, even when they would still fit.
        cache.set_budget(per * 200);
        assert_eq!(cache.stats().retained_bytes, per * 40);
        assert!(cache.vectors("old", 2).is_none());
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

    fn buffers(b: &RetainedVectors) -> [(usize, usize); 3] {
        [
            (b.block.ids.as_ptr() as usize, b.block.ids.capacity()),
            (b.block.values.as_ptr() as usize, b.block.values.capacity()),
            (b.norms.as_ptr() as usize, b.norms.capacity()),
        ]
    }
    /// Build what a scan would into the lease's buffers: `capacity` vectors
    /// of two dimensions, all `value`, with norms if the lease has them.
    fn build(lease: BlockLease, value: f32, partial: bool) -> (MemoryReservation, RetainedVectors) {
        let BlockLease {
            reservation,
            capacity,
            norms,
            block: mut b,
        } = lease;
        b.block.dimensions = 2;
        b.block.ids.reserve_exact(capacity);
        b.block.values.reserve_exact(capacity * 2);
        if norms {
            b.norms.reserve_exact(capacity);
        }
        for i in 0..capacity {
            b.block.ids.push(ulid::Ulid::from(i as u128 + 1));
            b.block.values.extend([value, value]);
            if norms {
                b.norms.push(2.0 * value * value);
            }
        }
        b.partial = partial;
        assert!(b.bytes() <= reservation.bytes);
        (reservation, b)
    }
    fn within_budget(cache: &SearchCache) {
        let s = cache.stats();
        assert!(s.retained_bytes + s.active_bytes <= s.memory_budget_bytes);
        assert!(s.peak_bytes <= s.memory_budget_bytes);
    }
    #[test]
    fn a_stale_block_lends_its_buffers_to_the_next_generation() {
        let n = 1000;
        let mut cache = SearchCache::default();
        cache.set_budget(block(n).bytes());
        let old = block(n);
        let before = buffers(&old);
        cache.insert_vectors("a", 1, old, cache.epoch);
        // A finished query leaves a loan behind, as every hit does.
        drop(cache.vectors("a", 1).unwrap());
        // The lookup leaves the older block resident and charged.
        assert!(cache.current_vectors("a", 2).is_none());
        assert_eq!(cache.stats().retained_bytes, n * 28);
        let lease = cache.reserve_block("a", 2, n, 2, true, 1).unwrap();
        assert_eq!((lease.capacity, lease.norms), (n, true));
        // The same allocations, emptied, now charged as the reservation.
        assert_eq!(buffers(&lease.block), before);
        assert!(lease.block.block.ids.is_empty() && lease.block.block.values.is_empty());
        assert!(lease.block.norms.is_empty() && !lease.block.partial);
        assert_eq!(lease.reservation.bytes, n * 28);
        let s = cache.stats();
        assert_eq!(
            (s.retained_bytes, s.active_bytes, s.vector_indexes),
            (0, n * 28, 0)
        );
        within_budget(&cache);
        let epoch = cache.epoch;
        let (reservation, b) = build(lease, 3.0, false);
        drop(reservation);
        cache.insert_vectors("a", 2, b, epoch);
        let hit = cache.vectors("a", 2).unwrap();
        assert_eq!(buffers(&hit), before);
        assert!(hit.block.values.iter().all(|&v| v == 3.0));
        assert!(hit.norms.iter().all(|&v| v == 18.0));
        assert_eq!(cache.stats().retained_bytes, n * 28);
        within_budget(&cache);
    }
    #[test]
    fn a_borrowed_stale_block_is_never_reused() {
        // Larger than the 64 KiB walk workspace, so two blocks never fit a
        // budget of one.
        let n = 3000;
        let mut cache = SearchCache::default();
        cache.set_budget(block(n).bytes() * 2);
        cache.insert_vectors("a", 1, block(n), cache.epoch);
        // An old-snapshot reader still scores the block.
        let reader = cache.vectors("a", 1).unwrap();
        let held = buffers(&reader);
        let lease = cache.reserve_block("a", 2, n, 2, true, 1).unwrap();
        assert_eq!(lease.block.bytes(), 0, "new buffers, not the borrowed ones");
        // The borrowed block left the cache but is still charged, beside the
        // new reservation.
        let s = cache.stats();
        assert_eq!((s.retained_bytes, s.active_bytes), (0, 2 * n * 28));
        within_budget(&cache);
        let epoch = cache.epoch;
        let (reservation, b) = build(lease, 5.0, false);
        drop(reservation);
        cache.insert_vectors("a", 2, b, epoch);
        // The reader's block is untouched.
        assert_eq!(buffers(&reader), held);
        assert_eq!(reader.block.ids.len(), n);
        assert!(reader.block.values.iter().all(|&v| v == 1.0));
        assert!(reader.norms.iter().all(|&v| v == 2.0));
        assert!(
            cache
                .vectors("a", 2)
                .unwrap()
                .block
                .values
                .iter()
                .all(|&v| v == 5.0)
        );
        drop(reader);
        assert_eq!(cache.stats().active_bytes, 0);
        // A block borrowed while the cache cannot fit another one is
        // dropped from the cache and nothing is reserved: the query streams.
        let reader = cache.vectors("a", 2).unwrap();
        cache.set_budget(block(n).bytes());
        cache.insert_vectors("a", 2, block(n), cache.epoch);
        assert!(cache.reserve_block("a", 3, n, 2, true, 1).is_none());
        assert_eq!(cache.stats().vector_indexes, 0);
        assert!(reader.block.values.iter().all(|&v| v == 5.0));
    }
    #[test]
    fn reused_buffers_are_fitted_to_the_new_block_or_replaced() {
        let n = 1000;
        let per = block(1).bytes();
        let mut cache = SearchCache::default();
        cache.set_budget(per * 2 * n);
        // Fewer vectors: the same allocations, shrunk to the reservation.
        cache.insert_vectors("a", 1, block(n), cache.epoch);
        drop(cache.vectors("a", 1).unwrap());
        let lease = cache.reserve_block("a", 2, n - 10, 2, true, 1).unwrap();
        let fitted = buffers(&lease.block);
        assert_eq!(
            [fitted[0].1, fitted[1].1, fitted[2].1],
            [n - 10, 2 * (n - 10), n - 10]
        );
        assert_eq!(lease.block.bytes(), lease.reservation.bytes);
        drop(lease);
        // More vectors: every buffer is too small and is replaced.
        cache.insert_vectors("a", 2, block(n), cache.epoch);
        let lease = cache.reserve_block("a", 3, n + 1, 2, true, 1).unwrap();
        assert_eq!(lease.block.bytes(), 0);
        drop(lease);
        // Another metric: the norms are freed, the vectors reused.
        cache.insert_vectors("a", 3, block(n), cache.epoch);
        let lease = cache.reserve_block("a", 4, n, 2, false, 1).unwrap();
        assert!(!lease.norms);
        assert_eq!(lease.block.norms.capacity(), 0);
        assert_eq!(lease.block.block.ids.capacity(), n);
        assert_eq!(lease.reservation.bytes, n * 24);
        drop(lease);
        // A block without norms reused for one with them: only the norms
        // are new.
        let plain = RetainedVectors {
            norms: Vec::new(),
            ..block(n)
        };
        cache.insert_vectors("a", 4, plain, cache.epoch);
        let lease = cache.reserve_block("a", 5, n, 2, true, 1).unwrap();
        assert!(lease.norms);
        assert_eq!(lease.block.block.ids.capacity(), n);
        assert_eq!(lease.block.norms.capacity(), 0);
        drop(lease);
        // Other dimensions are never reused.
        cache.insert_vectors("a", 5, block(n), cache.epoch);
        let lease = cache.reserve_block("a", 6, n / 2, 4, true, 1).unwrap();
        assert_eq!(lease.block.bytes(), 0);
        drop(lease);
        assert_eq!(cache.stats().active_bytes, 0);
        within_budget(&cache);
    }
    #[test]
    fn a_stale_prefix_lends_its_buffers_only_under_partial_admission() {
        let (per, walk) = (block(1).bytes(), 64 * 1024);
        let budget = walk + per * 100;
        let mut cache = SearchCache::default();
        cache.set_budget(budget);
        // The table holds 10,000 vectors: only a 100-vector prefix fits.
        let refused = cache.reserve_block("p", 1, 10_000, 2, true, 125).is_none();
        assert!(refused, "below the minimum share");
        let lease = cache.reserve_block("p", 1, 10_000, 2, true, 10).unwrap();
        assert_eq!(lease.capacity, 100);
        let epoch = cache.epoch;
        let (reservation, b) = build(lease, 1.0, true);
        let before = buffers(&b);
        drop(reservation);
        cache.insert_vectors("p", 1, b, epoch);
        drop(cache.vectors("p", 1).unwrap());
        assert_eq!(cache.stats().retained_bytes, per * 100);
        let lease = cache.reserve_block("p", 2, 10_000, 2, true, 10).unwrap();
        assert_eq!(lease.capacity, 100);
        assert_eq!(buffers(&lease.block), before);
        drop(lease);
        // A resident graph refuses the prefix; the stale block is dropped,
        // not kept for later.
        let (reservation, b) = build(
            cache.reserve_block("p", 2, 10_000, 2, true, 10).unwrap(),
            1.0,
            true,
        );
        drop(reservation);
        cache.insert_vectors("p", 2, b, cache.epoch);
        cache.insert_graph("g", graph(), cache.epoch);
        assert!(cache.reserve_block("p", 3, 10_000, 2, true, 10).is_none());
        let s = cache.stats();
        assert_eq!(
            (s.vector_indexes, s.graph_indexes, s.active_bytes),
            (0, 1, 0)
        );
        cache.take_graph("g");
        // A budget change during the build: the lease's block is refused.
        let lease = cache.reserve_block("p", 3, 10_000, 2, true, 10).unwrap();
        let epoch = cache.epoch;
        cache.set_budget(budget);
        let (reservation, b) = build(lease, 1.0, true);
        drop(reservation);
        cache.insert_vectors("p", 3, b, epoch);
        assert_eq!(cache.stats().retained_bytes, 0);
        // A budget of zero reserves nothing and drops the stale block.
        cache.insert_vectors("p", 3, partial(100), cache.epoch);
        cache.set_budget(budget);
        cache.insert_vectors("full", 1, block(10), cache.epoch);
        cache.set_budget(0);
        assert!(cache.reserve_block("full", 2, 10, 2, true, 1).is_none());
        assert_eq!(cache.stats().active_bytes, 0);
    }
    #[test]
    fn only_older_generations_are_taken_from_the_cache() {
        let n = 100;
        let mut cache = SearchCache::default();
        cache.set_budget(block(n).bytes() * 3);
        cache.insert_vectors("a", 2, block(n), cache.epoch);
        // An old snapshot rebuilding its own revision leaves the newer one.
        let lease = cache.reserve_block("a", 1, n, 2, true, 1).unwrap();
        assert_eq!(lease.block.bytes(), 0);
        assert_eq!(cache.stats().vector_indexes, 1);
        let epoch = cache.epoch;
        let (reservation, b) = build(lease, 9.0, false);
        drop(reservation);
        cache.insert_vectors("a", 1, b, epoch);
        assert!(
            cache
                .vectors("a", 2)
                .unwrap()
                .block
                .values
                .iter()
                .all(|&v| v == 1.0)
        );
        // A concurrent rebuild of the same revision leaves it too.
        let lease = cache.reserve_block("a", 2, n, 2, true, 1).unwrap();
        assert_eq!(lease.block.bytes(), 0);
        assert_eq!(cache.stats().vector_indexes, 1);
        drop(lease);
        // So does another key.
        let lease = cache.reserve_block("b", 7, n, 2, true, 1).unwrap();
        assert_eq!(lease.block.bytes(), 0);
        assert!(cache.current_vectors("a", 2).is_some());
    }
    #[test]
    fn a_graph_cached_while_a_prefix_is_built_keeps_the_prefix_out() {
        let per = block(1).bytes();
        let mut cache = SearchCache::default();
        cache.set_budget(per * 10_000);
        let (reservation, held) = cache.reserve_partial(per, 20_000, 1).unwrap();
        let epoch = cache.epoch;
        // A walk finishes and caches its graph meanwhile.
        cache.insert_graph("g", graph(), epoch);
        drop(reservation);
        cache.insert_vectors("p", 1, partial(held), epoch);
        let s = cache.stats();
        assert_eq!((s.vector_indexes, s.graph_indexes), (0, 1));
        // A full block still fits beside the graph.
        cache.insert_vectors("f", 1, block(10), cache.epoch);
        assert_eq!(cache.stats().vector_indexes, 1);
    }
}
