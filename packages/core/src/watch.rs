//! Live queries / reactive subscriptions for TalaDB.
//!
//! `Collection::watch(filter)` returns a `WatchHandle` that yields a fresh
//! snapshot of matching documents whenever any write to that collection occurs.
//!
//! Architecture
//! ------------
//! Each `Collection` shares a `WatchRegistry` held behind an `Arc<Mutex>`.
//! On every successful write, the collection calls `WatchRegistry::notify`.
//! Each active `WatchHandle` receives the write event through a `std::sync`
//! MPSC channel, re-runs the query, and delivers the new snapshot to the caller.
//!
//! Non-blocking by default: if the caller does not consume events fast enough
//! the channel queue grows up to `CHANNEL_CAPACITY` and then the oldest event
//! is dropped (lossy). The handle is always re-queried at receive time so no
//! documents are ever silently skipped — the worst that happens is that two
//! rapid writes coalesce into one snapshot.

use std::sync::{Arc, Mutex};

use crate::document::Document;
use crate::error::TalaDbError;
use crate::query::filter::Filter;

// ---------------------------------------------------------------------------
// Internal event
// ---------------------------------------------------------------------------

/// A lightweight notification that a write has occurred in a collection.
/// No payload — the receiver re-runs the query to get the current snapshot.
#[derive(Clone, Debug)]
pub struct WriteEvent;

const CHANNEL_CAPACITY: usize = 64;

// ---------------------------------------------------------------------------
// WatchRegistry — shared state per collection
// ---------------------------------------------------------------------------

type Sender = std::sync::mpsc::SyncSender<WriteEvent>;

/// Holds all active subscribers for a collection.
#[derive(Default)]
pub struct WatchRegistry {
    senders: Vec<Sender>,
    /// Monotonic write counter for this collection, bumped by `notify` after
    /// every committed mutation. Read caches (e.g. the decoded-vector cache)
    /// tag an entry with the generation it was built at and treat any later
    /// value as stale — so `notify` being the single post-commit chokepoint
    /// makes it the single cache-invalidation point too.
    generation: u64,
}

impl WatchRegistry {
    /// Notify all active subscribers of a write.
    pub fn notify(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.senders.retain(|tx| match tx.try_send(WriteEvent) {
            Ok(()) => true,
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                // Subscriber is too slow to consume events; drop it (WatchBackpressure).
                tracing::warn!("taladb: watch subscriber dropped: channel full (backpressure)");
                false
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => false,
        });
    }

    /// The current write generation for this collection.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Register a new subscriber and return the receiving end.
    fn subscribe(&mut self) -> std::sync::mpsc::Receiver<WriteEvent> {
        let (tx, rx) = std::sync::mpsc::sync_channel(CHANNEL_CAPACITY);
        self.senders.push(tx);
        rx
    }
}

// ---------------------------------------------------------------------------
// WatchHandle — returned to the caller
// ---------------------------------------------------------------------------

type QueryFn = Box<dyn Fn(&Filter) -> Result<Vec<Document>, TalaDbError> + Send>;

/// A live query handle.
///
/// Call `next()` (blocking) or `try_next()` (non-blocking) to receive
/// the latest snapshot of matching documents after each write.
pub struct WatchHandle {
    rx: std::sync::mpsc::Receiver<WriteEvent>,
    filter: Filter,
    /// Callback to re-execute the query against the current DB state.
    query_fn: QueryFn,
}

impl WatchHandle {
    /// Block until the next write event, then return the fresh snapshot.
    pub fn next(&self) -> Result<Vec<Document>, TalaDbError> {
        self.rx.recv().map_err(|_| TalaDbError::WatchClosed)?;
        // Drain any additional coalesced events
        while self.rx.try_recv().is_ok() {}
        (self.query_fn)(&self.filter)
    }

    /// Non-blocking: return `None` if no write has occurred since last call.
    pub fn try_next(&self) -> Result<Option<Vec<Document>>, TalaDbError> {
        match self.rx.try_recv() {
            Ok(_) => {
                while self.rx.try_recv().is_ok() {}
                Ok(Some((self.query_fn)(&self.filter)?))
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(TalaDbError::WatchClosed),
        }
    }

    /// Iterate snapshots indefinitely.
    pub fn iter(&self) -> WatchIter<'_> {
        WatchIter { handle: self }
    }
}

pub struct WatchIter<'a> {
    handle: &'a WatchHandle,
}

impl<'a> Iterator for WatchIter<'a> {
    type Item = Result<Vec<Document>, TalaDbError>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.handle.next())
    }
}

// ---------------------------------------------------------------------------
// CollectionWatcher — extension for Collection
// ---------------------------------------------------------------------------

/// A registry shared between a `Collection` and its `WatchHandle`s.
pub type SharedRegistry = Arc<Mutex<WatchRegistry>>;

/// Create a new shared registry.
pub fn new_registry() -> SharedRegistry {
    Arc::new(Mutex::new(WatchRegistry::default()))
}

/// Create a `WatchHandle` that re-runs `query_fn` after every write.
pub fn create_watch<F>(registry: &SharedRegistry, filter: Filter, query_fn: F) -> WatchHandle
where
    F: Fn(&Filter) -> Result<Vec<Document>, TalaDbError> + Send + 'static,
{
    let rx = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .subscribe();
    WatchHandle {
        rx,
        filter,
        query_fn: Box::new(query_fn),
    }
}

/// Notify all watchers that a write occurred. Called by `Collection` after
/// every successful write transaction.
pub fn notify(registry: &SharedRegistry) {
    // A full lock, not try_lock: skipping the notification under contention
    // would mean a committed write never wakes its watchers. notify() is
    // non-blocking (try_send), so holding the lock is cheap.
    let mut guard = registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.notify();
}

/// The current write generation for a collection's registry. Used by read
/// caches to detect when their contents have been superseded by a write.
pub fn generation(registry: &SharedRegistry) -> u64 {
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .generation()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    #[test]
    fn watch_receives_insert() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let registry = new_registry();

        let db_clone = Arc::clone(&db);
        let handle = create_watch(&registry, Filter::All, move |filter| {
            let col = db_clone.collection("users").unwrap();
            col.find(filter.clone())
        });

        // Simulate a write notification
        notify(&registry);

        let snapshot = handle.next().unwrap();
        assert!(snapshot.is_empty()); // No docs yet — notification arrived
    }
}
