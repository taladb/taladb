use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Bound;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use redb::{Database, Durability, ReadableTable, TableDefinition, TableHandle};

use crate::error::TalaDbError;

// ---------------------------------------------------------------------------
// Storage abstraction — lets WASM swap in an OPFS backend
// ---------------------------------------------------------------------------

/// Key-value pairs returned from range/scan operations.
pub type KvPairs = Vec<(Vec<u8>, Vec<u8>)>;

/// What a [`ScanFn`] tells the scanner to do after each entry.
///
/// `Stop` exists so a bounded query (`find_one`, `count` with a limit) can quit
/// walking the B-tree the moment it has enough, instead of draining the range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanFlow {
    Continue,
    Stop,
}

/// Callback invoked once per entry by [`ReadTxn::scan`], borrowing the key and
/// value straight out of storage — no `Vec` is allocated per entry.
pub type ScanFn<'f> = &'f mut dyn FnMut(&[u8], &[u8]) -> Result<ScanFlow, TalaDbError>;

/// A single mutation in a [`WriteTxn::apply_batch`] group.
pub enum KvOp<'a> {
    Put(&'a [u8], &'a [u8]),
    Delete(&'a [u8]),
}

pub trait StorageBackend: Send + Sync {
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError>;
    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError>;
    /// Compact the underlying storage, reclaiming space from deleted/updated
    /// records. No-op on backends that manage compaction automatically.
    fn compact(&self) -> Result<(), TalaDbError> {
        Ok(())
    }
    /// Set write durability. `eventual = false` (default) fsyncs every commit;
    /// `true` batches commits for throughput, requiring [`flush`](Self::flush)
    /// to force a durable sync. No-op on backends without a durability knob.
    fn set_durability(&self, _eventual: bool) {}
    /// Force any batched (eventual) commits to durable storage. No-op on
    /// in-memory backends and when already committing immediately.
    fn flush(&self) -> Result<(), TalaDbError> {
        Ok(())
    }
}

pub trait WriteTxn {
    fn put(&mut self, table: &str, key: &[u8], value: &[u8]) -> Result<(), TalaDbError>;
    fn delete(&mut self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError>;
    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError>;
    fn range(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, TalaDbError>;
    fn commit(self: Box<Self>) -> Result<(), TalaDbError>;

    /// Fetch many keys from one table. Semantically `keys.map(|k| self.get(k))`.
    ///
    /// The point is that a backend can resolve the table **once** for the whole
    /// batch instead of once per key — on redb that is a ~2.3 µs `open_table`
    /// saved per key, which dominates a candidate re-fetch loop.
    fn get_many(&self, table: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        keys.iter().map(|k| self.get(table, k)).collect()
    }

    /// Apply a group of mutations to one table, in order. Same one-open-per-batch
    /// rationale as [`get_many`](Self::get_many).
    fn apply_batch(&mut self, table: &str, ops: &[KvOp<'_>]) -> Result<(), TalaDbError> {
        for op in ops {
            match op {
                KvOp::Put(k, v) => self.put(table, k, v)?,
                KvOp::Delete(k) => {
                    self.delete(table, k)?;
                }
            }
        }
        Ok(())
    }
}

pub trait ReadTxn {
    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError>;
    fn range(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, TalaDbError>;
    fn scan_all(&self, table: &str) -> Result<KvPairs, TalaDbError>;
    /// Return the names of every table in the database.
    fn list_tables(&self) -> Result<Vec<String>, TalaDbError>;
    /// Count entries in `table` without loading them. Returns 0 if the table
    /// does not exist. Used by `Collection::count(Filter::All)` to skip the
    /// load-and-deserialise-everything path.
    fn count_entries(&self, table: &str) -> Result<u64, TalaDbError>;

    /// Fetch many keys from one table — see [`WriteTxn::get_many`].
    fn get_many(&self, table: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        keys.iter().map(|k| self.get(table, k)).collect()
    }

    /// Walk `[start, end]` in key order, handing each entry to `f` as borrowed
    /// slices.
    ///
    /// This is the allocation-free counterpart to [`range`](Self::range), which
    /// copies every key and value into a freshly allocated `Vec` pair and
    /// materialises the entire result set before the caller sees the first
    /// entry. Returning [`ScanFlow::Stop`] ends the walk early.
    ///
    /// The default implementation falls back to `range`, so a backend that has
    /// no streaming primitive stays correct (just not faster).
    fn scan(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
        f: ScanFn<'_>,
    ) -> Result<(), TalaDbError> {
        for (k, v) in self.range(table, start, end)? {
            if f(&k, &v)? == ScanFlow::Stop {
                break;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// redb backend
// ---------------------------------------------------------------------------

pub struct RedbBackend {
    // Mutex is needed because redb::Database::compact() takes &mut self.
    // begin_write/begin_read return owned transactions so the lock is held
    // only for the duration of the call, not across the transaction lifetime.
    db: Arc<Mutex<Database>>,
    /// When true, write transactions commit with `Durability::Eventual`
    /// (batched fsync) instead of the default `Immediate`. Toggled by
    /// [`set_durability`](StorageBackend::set_durability).
    eventual: AtomicBool,
}

impl RedbBackend {
    pub fn open(path: &Path) -> Result<Self, TalaDbError> {
        let db = Database::create(path)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            eventual: AtomicBool::new(false),
        })
    }

    pub fn open_in_memory() -> Result<Self, TalaDbError> {
        let db = Database::builder().create_with_backend(redb::backends::InMemoryBackend::new())?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            eventual: AtomicBool::new(false),
        })
    }

    /// Open a database using any `redb::StorageBackend` (e.g. OPFS in WASM).
    pub fn open_with_redb_backend<B: redb::StorageBackend + 'static>(
        backend: B,
    ) -> Result<Self, TalaDbError> {
        let db = Database::builder().create_with_backend(backend)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            eventual: AtomicBool::new(false),
        })
    }
}

impl StorageBackend for RedbBackend {
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        let mut txn = self
            .db
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_write()?;
        txn.set_durability(if self.eventual.load(Ordering::Relaxed) {
            Durability::Eventual
        } else {
            Durability::Immediate
        });
        Ok(Box::new(RedbWriteTxn { txn }))
    }

    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        let txn = self
            .db
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_read()?;
        Ok(Box::new(RedbReadTxn {
            txn,
            open: RefCell::new(HashMap::new()),
        }))
    }

    fn compact(&self) -> Result<(), TalaDbError> {
        self.db
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .compact()
            .map_err(|e| TalaDbError::Storage(e.to_string()))?;
        Ok(())
    }

    fn set_durability(&self, eventual: bool) {
        self.eventual.store(eventual, Ordering::Relaxed);
    }

    fn flush(&self) -> Result<(), TalaDbError> {
        // An empty commit with Immediate durability fsyncs the file, persisting
        // any prior Eventual commits. No-op cost when already committing
        // immediately (the data is already durable).
        let mut txn = self
            .db
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_write()?;
        txn.set_durability(Durability::Immediate);
        txn.commit()?;
        Ok(())
    }
}

// --- Write transaction ---

struct RedbWriteTxn {
    txn: redb::WriteTransaction,
}

/// Maximum number of distinct table names that may be interned per process.
/// Each name is `Box::leak`ed exactly once (~50–200 bytes each), so this
/// caps total memory growth from interning at well under a megabyte.
const MAX_INTERNED_NAMES: usize = 4096;

/// Intern a table name string so we can hand a `&'static str` to redb's
/// `TableDefinition::new`, which requires `'static`.
///
/// Each unique name is leaked exactly once; subsequent calls return the same
/// pointer. The intern set is bounded by [`MAX_INTERNED_NAMES`] to prevent
/// unbounded memory growth when a workload generates many distinct names.
/// Returns [`TalaDbError::InvalidOperation`] if the cap is exceeded — the
/// caller then sees the limit as a runtime error rather than a process crash.
fn intern_name(name: &str) -> Result<&'static str, TalaDbError> {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static INTERNED: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let set = INTERNED.get_or_init(|| Mutex::new(HashSet::new()));
    // Recover from a poisoned mutex: the only content is interned strings
    // (immutable &'static str), so the state is always valid even after panic.
    let mut guard = set.lock().unwrap_or_else(|p| {
        tracing::warn!("intern_name: mutex was poisoned; recovering");
        p.into_inner()
    });
    if let Some(&existing) = guard.get(name) {
        return Ok(existing);
    }
    if guard.len() >= MAX_INTERNED_NAMES {
        return Err(TalaDbError::InvalidOperation(format!(
            "exceeded {MAX_INTERNED_NAMES} interned table names; avoid dynamically \
             generating unbounded collection or index names"
        )));
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    guard.insert(leaked);
    Ok(leaked)
}

fn table_def(
    name: &str,
) -> Result<TableDefinition<'static, &'static [u8], &'static [u8]>, TalaDbError> {
    Ok(TableDefinition::new(intern_name_cached(name)?))
}

thread_local! {
    /// Per-thread memo of names already interned, so the steady state never
    /// touches the process-global `INTERNED` mutex.
    ///
    /// Table names are resolved on *every* key operation, and the global lock —
    /// while individually cheap (~75 ns) — serialises otherwise-independent
    /// readers on multi-threaded runtimes. The set of names is tiny and bounded
    /// by `MAX_INTERNED_NAMES`, so a per-thread copy costs nothing meaningful.
    static LOCAL_NAMES: RefCell<HashMap<Box<str>, &'static str>> =
        RefCell::new(HashMap::new());
}

fn intern_name_cached(name: &str) -> Result<&'static str, TalaDbError> {
    if let Some(hit) = LOCAL_NAMES.with(|c| c.borrow().get(name).copied()) {
        return Ok(hit);
    }
    let interned = intern_name(name)?;
    LOCAL_NAMES.with(|c| c.borrow_mut().insert(name.into(), interned));
    Ok(interned)
}

impl WriteTxn for RedbWriteTxn {
    fn put(&mut self, table: &str, key: &[u8], value: &[u8]) -> Result<(), TalaDbError> {
        let mut tbl = self.txn.open_table(table_def(table)?)?;
        tbl.insert(key, value)?;
        Ok(())
    }

    fn delete(&mut self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        let mut tbl = self.txn.open_table(table_def(table)?)?;
        let old = tbl.remove(key)?.map(|v| v.value().to_vec());
        Ok(old)
    }

    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        match self.txn.open_table(table_def(table)?) {
            Ok(tbl) => Ok(tbl.get(key)?.map(|v| v.value().to_vec())),
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn range(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, TalaDbError> {
        match self.txn.open_table(table_def(table)?) {
            Ok(tbl) => {
                let iter = tbl.range::<&[u8]>((start, end))?;
                let mut out = Vec::new();
                for entry in iter {
                    let (k, v) = entry?;
                    out.push((k.value().to_vec(), v.value().to_vec()));
                }
                Ok(out)
            }
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(vec![]),
            Err(e) => Err(e.into()),
        }
    }

    fn commit(self: Box<Self>) -> Result<(), TalaDbError> {
        self.txn.commit()?;
        Ok(())
    }

    /// One `open_table` for the whole batch instead of one per key.
    fn get_many(&self, table: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        match self.txn.open_table(table_def(table)?) {
            Ok(tbl) => {
                let mut out = Vec::with_capacity(keys.len());
                for key in keys {
                    out.push(tbl.get(*key)?.map(|v| v.value().to_vec()));
                }
                Ok(out)
            }
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(vec![None; keys.len()]),
            Err(e) => Err(e.into()),
        }
    }

    /// One `open_table` for the whole batch instead of one per mutation.
    fn apply_batch(&mut self, table: &str, ops: &[KvOp<'_>]) -> Result<(), TalaDbError> {
        if ops.is_empty() {
            return Ok(());
        }
        let mut tbl = self.txn.open_table(table_def(table)?)?;
        for op in ops {
            match op {
                KvOp::Put(k, v) => {
                    tbl.insert(*k, *v)?;
                }
                KvOp::Delete(k) => {
                    tbl.remove(*k)?;
                }
            }
        }
        Ok(())
    }
}

// --- Read transaction ---

type RedbTable = redb::ReadOnlyTable<&'static [u8], &'static [u8]>;

struct RedbReadTxn {
    txn: redb::ReadTransaction,
    /// Tables already opened in this transaction.
    ///
    /// `ReadTransaction::open_table` costs ~2.3 µs — dwarfing the ~0.8 µs of an
    /// actual point lookup — and the document-fetch path performs one lookup
    /// per result. Caching per transaction makes that cost O(tables) instead of
    /// O(keys). `None` records a table that does not exist, so a miss is not
    /// re-probed on every call.
    ///
    /// `RefCell` (not `Mutex`) because a `ReadTxn` is used by one thread at a
    /// time: it is handed out as `Box<dyn ReadTxn + '_>` and never shared.
    open: RefCell<HashMap<Box<str>, Option<Rc<RedbTable>>>>,
}

impl RedbReadTxn {
    /// Resolve `table` through the per-transaction cache.
    fn table(&self, name: &str) -> Result<Option<Rc<RedbTable>>, TalaDbError> {
        if let Some(hit) = self.open.borrow().get(name) {
            return Ok(hit.clone());
        }
        let opened = match self.txn.open_table(table_def(name)?) {
            Ok(tbl) => Some(Rc::new(tbl)),
            Err(redb::TableError::TableDoesNotExist(_)) => None,
            Err(e) => return Err(e.into()),
        };
        self.open
            .borrow_mut()
            .insert(name.into(), opened.clone());
        Ok(opened)
    }
}

impl ReadTxn for RedbReadTxn {
    fn list_tables(&self) -> Result<Vec<String>, TalaDbError> {
        let names = self
            .txn
            .list_tables()?
            .map(|t| t.name().to_string())
            .collect();
        Ok(names)
    }

    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        match self.table(table)? {
            Some(tbl) => Ok(tbl.get(key)?.map(|v| v.value().to_vec())),
            None => Ok(None),
        }
    }

    fn get_many(&self, table: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        let Some(tbl) = self.table(table)? else {
            return Ok(vec![None; keys.len()]);
        };
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            out.push(tbl.get(*key)?.map(|v| v.value().to_vec()));
        }
        Ok(out)
    }

    fn range(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, TalaDbError> {
        let Some(tbl) = self.table(table)? else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();
        for entry in tbl.range::<&[u8]>((start, end))? {
            let (k, v) = entry?;
            out.push((k.value().to_vec(), v.value().to_vec()));
        }
        Ok(out)
    }

    fn scan(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
        f: ScanFn<'_>,
    ) -> Result<(), TalaDbError> {
        let Some(tbl) = self.table(table)? else {
            return Ok(());
        };
        for entry in tbl.range::<&[u8]>((start, end))? {
            let (k, v) = entry?;
            // Borrowed straight out of the page cache — no per-entry Vec.
            if f(k.value(), v.value())? == ScanFlow::Stop {
                break;
            }
        }
        Ok(())
    }

    fn scan_all(&self, table: &str) -> Result<KvPairs, TalaDbError> {
        let Some(tbl) = self.table(table)? else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();
        for entry in tbl.iter()? {
            let (k, v) = entry?;
            out.push((k.value().to_vec(), v.value().to_vec()));
        }
        Ok(out)
    }

    fn count_entries(&self, table: &str) -> Result<u64, TalaDbError> {
        use redb::ReadableTableMetadata;
        match self.table(table)? {
            Some(tbl) => Ok(tbl.len()?),
            None => Ok(0),
        }
    }
}
