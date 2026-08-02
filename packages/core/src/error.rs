//! The crate-wide error type.
//!
//! # Why storage failures are split by kind
//!
//! TalaDB runs on-device, where the application — not an operator — has to
//! decide what to do when a write fails. "Retry in a moment" is right for a
//! contended transaction and badly wrong for a corrupt page or a full disk.
//! Every storage failure used to be flattened into `Storage(String)` with the
//! underlying error stringified, which erased both the distinction and the
//! [`std::error::Error::source`] chain needed to recover it.
//!
//! Each storage failure now carries its originating [`redb`] error as a real
//! source, so callers can walk the chain (or downcast) to make that decision:
//!
//! ```no_run
//! # use taladb_core::TalaDbError;
//! # fn handle(err: TalaDbError) {
//! use std::error::Error;
//!
//! if let Some(source) = err.source() {
//!     if let Some(io_err) = source.downcast_ref::<redb::StorageError>() {
//!         // Disk-level failure — distinguishable from a table or commit error.
//!         eprintln!("storage layer: {io_err}");
//!     }
//! }
//! # }
//! ```
//!
//! [`Display`](std::fmt::Display) output is unchanged: each variant still
//! renders the same `storage error: …` / `serialization error: …` text it did
//! when the message was carried as a `String`, so error text crossing the WASM
//! and React Native boundaries is unaffected.

use thiserror::Error;

/// Errors returned by every fallible TalaDB operation.
///
/// This enum is `#[non_exhaustive]`: match arms must include a `_` fallback.
/// Use [`TalaDbError::code`] for a stable machine-readable discriminant rather
/// than matching every variant by hand.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TalaDbError {
    /// A storage failure raised by TalaDB itself rather than by the storage
    /// engine — there is no underlying [`redb`] error to attach.
    #[error("storage error: {0}")]
    Storage(String),

    /// The database file could not be opened, created, or recovered.
    #[error("storage error: {0}")]
    StorageDatabase(#[from] redb::DatabaseError),

    /// A transaction could not be started.
    #[error("storage error: {0}")]
    StorageTransaction(#[source] Box<redb::TransactionError>),

    /// A table could not be opened or created — including "table does not
    /// exist", which several read paths treat as an empty result.
    #[error("storage error: {0}")]
    StorageTable(#[source] Box<redb::TableError>),

    /// A read or write against the underlying storage failed: I/O errors, a
    /// full disk, and page corruption all surface here.
    #[error("storage error: {0}")]
    StorageIo(#[from] redb::StorageError),

    /// A transaction failed to commit; the write did not land.
    #[error("storage error: {0}")]
    StorageCommit(#[from] redb::CommitError),

    /// A storage failure [`redb`] reported through its catch-all error type.
    #[error("storage error: {0}")]
    StorageEngine(#[source] Box<redb::Error>),

    /// A serialization failure raised by TalaDB itself rather than by the
    /// codec — there is no underlying [`postcard`] error to attach.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// A document or metadata record failed to encode or decode.
    #[error("serialization error: {0}")]
    Postcard(#[from] postcard::Error),

    #[error("document not found")]
    NotFound,

    #[error("invalid filter: {0}")]
    InvalidFilter(String),

    #[error("index already exists: {0}")]
    IndexExists(String),

    #[error("index not found: {0}")]
    IndexNotFound(String),

    #[error("migration error: {0}")]
    Migration(String),

    #[error("type error: expected {expected}, got {got}")]
    TypeError { expected: String, got: String },

    #[error("encryption error: {0}")]
    Encryption(String),

    #[error("watch channel closed")]
    WatchClosed,

    #[error("watch subscriber dropped due to full channel")]
    WatchBackpressure,

    #[error("invalid or corrupt snapshot data")]
    InvalidSnapshot,

    #[error("vector index not found: {0}")]
    VectorIndexNotFound(String),

    #[error("vector dimension mismatch: index expects {expected}, got {got}")]
    VectorDimensionMismatch { expected: usize, got: usize },

    #[error("invalid operation: {0}")]
    InvalidOperation(String),

    #[error("config error: {0}")]
    Config(String),

    #[error("invalid name: {0}")]
    InvalidName(String),

    #[error("query exceeded the configured timeout")]
    QueryTimeout,

    #[error("changeset exceeds the maximum allowed entry count")]
    ChangesetTooLarge,
}

/// Box the oversized `redb` sources rather than storing them inline.
///
/// `redb::Error` and `redb::TransactionError` are 160 bytes each and
/// `TableError` is 88. Stored inline they made `TalaDbError` 176 bytes, and so
/// every `Result<T, TalaDbError>` in the engine at least that wide — paid on
/// every *successful* call to carry detail that only matters on a failing one.
///
/// The three sources that already fit (`DatabaseError`, `StorageError`,
/// `CommitError` — 24 bytes each) stay inline, because `source()` then
/// downcasts straight to the `redb` type. `StorageError` is the one that
/// distinguishes a full disk from a corrupt page, so it is the one worth
/// keeping ergonomic; the boxed three downcast to `Box<T>` instead.
macro_rules! boxed_from {
    ($($source:ty => $variant:ident),* $(,)?) => {
        $(
            impl From<$source> for TalaDbError {
                fn from(e: $source) -> Self {
                    TalaDbError::$variant(Box::new(e))
                }
            }
        )*
    };
}

boxed_from! {
    redb::TransactionError => StorageTransaction,
    redb::TableError => StorageTable,
    redb::Error => StorageEngine,
}

impl TalaDbError {
    /// A stable, machine-readable discriminant for this error.
    ///
    /// The language bindings surface this to JavaScript as `error.code`, so
    /// these strings are part of the public contract — they may be added to,
    /// but an existing code must not change meaning. Every storage failure maps
    /// to `"Storage"` regardless of which [`redb`] error produced it, keeping
    /// the JS-visible taxonomy the same as before the split; callers that need
    /// the finer distinction walk [`source`](std::error::Error::source).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Storage(_)
            | Self::StorageDatabase(_)
            | Self::StorageTransaction(_)
            | Self::StorageTable(_)
            | Self::StorageIo(_)
            | Self::StorageCommit(_)
            | Self::StorageEngine(_) => "Storage",
            Self::Serialization(_) | Self::Postcard(_) => "Serialization",
            Self::NotFound => "NotFound",
            Self::InvalidFilter(_) => "InvalidFilter",
            Self::IndexExists(_) => "IndexExists",
            Self::IndexNotFound(_) => "IndexNotFound",
            Self::Migration(_) => "Migration",
            Self::TypeError { .. } => "TypeError",
            Self::Encryption(_) => "Encryption",
            Self::WatchClosed => "WatchClosed",
            Self::WatchBackpressure => "WatchBackpressure",
            Self::InvalidSnapshot => "InvalidSnapshot",
            Self::VectorIndexNotFound(_) => "VectorIndexNotFound",
            Self::VectorDimensionMismatch { .. } => "VectorDimensionMismatch",
            Self::InvalidOperation(_) => "InvalidOperation",
            Self::Config(_) => "Config",
            Self::InvalidName(_) => "InvalidName",
            Self::QueryTimeout => "QueryTimeout",
            Self::ChangesetTooLarge => "ChangesetTooLarge",
        }
    }

    /// Whether this error came from the storage engine, in any of its forms.
    pub fn is_storage(&self) -> bool {
        self.code() == "Storage"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    /// The whole point of the split: a storage failure must still hand back the
    /// originating error, not just a sentence about it.
    #[test]
    fn storage_errors_expose_their_source() {
        let err = TalaDbError::from(redb::StorageError::Corrupted("bad page".into()));
        let source = err.source().expect("storage errors must carry a source");
        assert!(
            source.downcast_ref::<redb::StorageError>().is_some(),
            "source must downcast back to the originating redb error"
        );
        assert_eq!(err.code(), "Storage");
    }

    /// Display text is load-bearing — it is what crosses into JS through the
    /// WASM and React Native bindings.
    #[test]
    fn storage_display_text_is_unchanged_by_the_split() {
        let inner = redb::StorageError::Corrupted("bad page".into());
        let expected = format!("storage error: {inner}");
        assert_eq!(TalaDbError::from(inner).to_string(), expected);
    }

    /// The oversized sources are boxed, so they downcast to `Box<T>` rather
    /// than `T`. Asymmetric, and deliberately so — this test is the record of
    /// which is which.
    #[test]
    fn oversized_sources_downcast_through_their_box() {
        let err = TalaDbError::from(redb::TableError::TableDoesNotExist("books".into()));
        let source = err.source().expect("must carry a source");
        assert!(
            source.downcast_ref::<Box<redb::TableError>>().is_some(),
            "boxed sources downcast to Box<T>"
        );
        assert_eq!(err.code(), "Storage");
    }

    /// `Result<T, TalaDbError>` is returned by every engine call, so the error
    /// type's size is paid on the success path too. `redb::Error` is 160 bytes
    /// on its own; storing the sources inline took `TalaDbError` to 176. Keep
    /// them boxed.
    #[test]
    fn error_type_stays_small_enough_for_the_hot_path() {
        let size = std::mem::size_of::<TalaDbError>();
        assert!(
            size <= 56,
            "TalaDbError grew to {size} bytes — box any large source rather than \
             storing it inline"
        );
    }

    /// Hand-built messages have no source, and that is fine — they are not
    /// wrapping anything.
    #[test]
    fn hand_built_storage_errors_have_no_source() {
        let err = TalaDbError::Storage("cannot write encryption salt".into());
        assert!(err.source().is_none());
        assert!(err.is_storage());
    }
}
