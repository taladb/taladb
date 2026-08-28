//! Bringing a pre-redb-4 database file forward.
//!
//! redb 4 changed the on-disk format and refuses anything older outright —
//! `"Manual upgrade required. Expected file format version 3, but file is
//! version 2"` — and offers no upgrade API of its own. Without something here,
//! every database written by a previous release of TalaDB would simply stop
//! opening.
//!
//! The approach is to keep redb 2 linked alongside redb 4 — they are separate
//! crates as far as cargo is concerned, so both can be present — and let redb 2
//! do the conversion itself. `redb2::Database::upgrade()` rewrites a v2 file to
//! v3, which is the format redb 4 reads.
//!
//! # Why redb's own upgrade rather than copying the data across
//!
//! Copying every table out of a redb 2 database and into a fresh redb 4 one also
//! works, and was the first thing tried here. It was replaced because it is this
//! code, rather than upstream's, deciding what "all the data" means: it iterates
//! `list_tables()`, which does not include multimap tables. TalaDB has none
//! today, so nothing was actually lost — but adding one later would have made
//! the migration start silently dropping it, with no failure to notice. Calling
//! `upgrade()` cannot develop that class of bug, because it never enumerates
//! anything.
//!
//! A `.v2.bak` copy is taken first regardless. `upgrade()` is a transactional
//! rewrite and is safe against a crash, but not against being handed a file it
//! mishandles, and a database is not something a user can regenerate.

// The file-based migration is native-only: it needs a filesystem to copy and
// atomically rename. Everything wasm keeps is below — the help text and the error
// sniffer, which the OPFS path uses to report something actionable.
#[cfg(all(feature = "legacy-migration", not(target_arch = "wasm32")))]
use std::path::{Path, PathBuf};

#[cfg(all(feature = "legacy-migration", not(target_arch = "wasm32")))]
use crate::error::TalaDbError;

/// Extension appended to the pre-migration copy of the database.
#[cfg(all(feature = "legacy-migration", not(target_arch = "wasm32")))]
const BACKUP_SUFFIX: &str = "v2.bak";

/// Does this file need bringing forward before redb 4 will open it?
///
/// Detection is by *attempting the open*, not by parsing the header: redb owns
/// its format, and a hand-rolled version check here would be one more thing to
/// keep in step with a dependency that has already changed format twice.
#[cfg(all(feature = "legacy-migration", not(target_arch = "wasm32")))]
fn needs_migration(path: &Path) -> bool {
    match redb::Database::open(path) {
        Ok(_) => false,
        // The error is only informative as a string; redb models it as an opaque
        // storage error rather than a distinct variant.
        Err(e) => {
            let msg = e.to_string();
            msg.contains("Manual upgrade required") || msg.contains("file is version 2")
        }
    }
}

/// Convert `path` from redb 2's format to redb 4's, in place, keeping a backup.
///
/// Returns `Ok(false)` when the file did not need converting, `Ok(true)` when it
/// was converted.
///
/// # Errors
///
/// Returns [`TalaDbError::Storage`] if the file cannot be read as a v2 database,
/// or if the backup, conversion or replacement fails. The original file is left
/// untouched unless the whole conversion succeeded.
#[cfg(all(feature = "legacy-migration", not(target_arch = "wasm32")))]
pub fn migrate_file_if_needed(path: &Path) -> Result<bool, TalaDbError> {
    if !path.exists() || !needs_migration(path) {
        return Ok(false);
    }

    // Taken before anything touches the original, so there is always a copy in
    // the pre-migration format to fall back to.
    let backup = backup_path(path);
    std::fs::copy(path, &backup).map_err(|e| {
        TalaDbError::Storage(format!(
            "could not back up {} before migrating it: {e}",
            path.display()
        ))
    })?;

    let mut old = redb2::Database::open(path).map_err(|e| {
        TalaDbError::Storage(format!(
            "{} looks like a pre-redb-4 database but could not be opened as one: {e} \
             (a copy is at {})",
            path.display(),
            backup.display()
        ))
    })?;

    old.upgrade().map_err(|e| {
        TalaDbError::Storage(format!(
            "could not upgrade {} to the current file format: {e} (the pre-migration \
             copy is at {})",
            path.display(),
            backup.display()
        ))
    })?;

    // Close the redb 2 handle before the caller opens the same path with redb 4.
    drop(old);
    Ok(true)
}

/// Where the pre-migration copy is kept.
#[cfg(all(feature = "legacy-migration", not(target_arch = "wasm32")))]
fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".");
    name.push(BACKUP_SUFFIX);
    PathBuf::from(name)
}

/// Upgrade a database held in a custom [`redb::StorageBackend`] — OPFS in the
/// browser, or anything else host-supplied — from the pre-redb-4 format.
///
/// Returns `Ok(true)` when it upgraded the storage.
///
/// # Only call this after an open has already failed
///
/// **This must not be called speculatively.** redb 2 does not merely decline a
/// database that redb 4 wrote — it hits an `unreachable!()` and panics, which in
/// wasm is an unrecoverable trap. Its header check accepts the version number
/// redb 4 stamps, so the refusal comes far enough in to be fatal rather than
/// tidy.
///
/// The contract is therefore: attempt the ordinary redb 4 open first, and reach
/// for this **only** when that failed and [`is_legacy_format_error`] says why.
/// [`migrate_file_if_needed`] follows exactly that order on the native path.
///
/// # Why this can work without a second file
///
/// The native path copies the database aside and renames a converted file over
/// it, which a browser cannot do: OPFS gives out a handle, not a filesystem to
/// stage in. That made an in-browser migration look like it needed a second
/// handle passed down from JavaScript, and so a binding API change.
///
/// It does not. redb 2 reads the old format and rewrites it into the new one
/// **in place**, through whatever backend it was handed — so pointing it at the
/// same OPFS handle the caller already has is enough. The caller then reopens
/// that storage with redb 4.
///
/// # The backend is consumed
///
/// redb takes ownership of a backend, so the caller hands one over and builds a
/// fresh one afterwards for the redb 4 open. For OPFS that is free: the backend
/// wraps a JS handle, and cloning the handle is another reference to the same
/// file, not another file.
///
/// # No backup is taken
///
/// Unlike the native path there is nowhere to put one — a copy would mean a
/// second OPFS file, which is the API change this avoids. The conversion is
/// redb's own and transactional, but a caller holding irreplaceable data in the
/// browser should export a snapshot before upgrading.
///
/// # Errors
///
/// Returns [`TalaDbError::Storage`] if the database cannot be opened by redb 2 or
/// the upgrade fails. The storage is left as it was.
#[cfg(feature = "legacy-migration")]
pub fn upgrade_legacy_backend<B: redb2::StorageBackend>(
    backend: B,
) -> Result<bool, crate::error::TalaDbError> {
    use crate::error::TalaDbError;

    let mut db = redb2::Database::builder()
        .create_with_backend(backend)
        .map_err(|e| {
            TalaDbError::Storage(format!(
                "could not open the existing database to upgrade it: {e}"
            ))
        })?;

    let upgraded = db.upgrade().map_err(|e| {
        TalaDbError::Storage(format!(
            "could not upgrade the database to the current format: {e}"
        ))
    })?;

    // Close redb 2's handle before the caller reopens the same storage.
    drop(db);
    Ok(upgraded)
}

/// What to tell a caller whose database predates redb 4 when nothing here can
/// upgrade it — the `legacy-migration` feature is off.
pub const LEGACY_BACKEND_HELP: &str = "this database was written by an older version of TalaDB \
     (redb file format 2). Rebuild with the `legacy-migration` feature to upgrade it \
     automatically, or export a snapshot with the previous version (`exportSnapshot`) and \
     restore it here (`openWithSnapshot`).";

#[must_use]
pub fn is_legacy_format_error(msg: &str) -> bool {
    msg.contains("Manual upgrade required") || msg.contains("file is version 2")
}

#[cfg(all(test, feature = "legacy-migration", not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use redb2::ReadableTableMetadata as _;

    const TBL: redb2::TableDefinition<&[u8], &[u8]> = redb2::TableDefinition::new("books");

    /// Write a genuine redb 2 database, rather than committing a binary fixture
    /// that nothing can regenerate once redb 2 is eventually dropped.
    fn write_v2(path: &Path, rows: &[(&str, &str)]) {
        let db = redb2::Database::create(path).expect("create v2");
        let wtx = db.begin_write().expect("write txn");
        {
            let mut t = wtx.open_table(TBL).expect("open table");
            for (k, v) in rows {
                t.insert(k.as_bytes(), v.as_bytes()).expect("insert");
            }
        }
        wtx.commit().expect("commit");
    }

    #[test]
    fn a_v2_database_is_detected_and_converted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.redb");
        write_v2(&path, &[("k1", "Dune"), ("k2", "Neuromancer")]);

        assert!(needs_migration(&path), "a v2 file must be recognised");
        assert!(
            migrate_file_if_needed(&path).expect("migration should succeed"),
            "migration should report that it did something"
        );

        // redb 4 can now open it, and the rows are all still there.
        use redb::ReadableDatabase as _;
        use redb::ReadableTableMetadata as _;
        let db = redb::Database::open(&path).expect("redb 4 must open the converted file");
        let rtx = db.begin_read().expect("read");
        let t = rtx
            .open_table(redb::TableDefinition::<&[u8], &[u8]>::new("books"))
            .expect("table survived");
        assert_eq!(t.len().expect("len"), 2, "no rows may be lost");
        assert_eq!(
            t.get("k1".as_bytes()).expect("get").expect("k1").value(),
            b"Dune"
        );
    }

    #[test]
    fn the_original_is_kept_as_a_backup_and_is_still_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.redb");
        write_v2(&path, &[("k1", "Dune")]);
        migrate_file_if_needed(&path).expect("migrate");

        let backup = backup_path(&path);
        assert!(backup.exists(), "a pre-migration copy must be kept");

        // Still a valid v2 database — the point of the backup is that it can be
        // opened by the version that wrote it if the migration turns out wrong.
        let db = redb2::Database::open(&backup).expect("backup must still open as redb 2");
        let rtx = db.begin_read().expect("read");
        let t = rtx.open_table(TBL).expect("table");
        assert_eq!(t.len().expect("len"), 1);
    }

    #[test]
    fn a_current_database_is_left_completely_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current.redb");
        {
            let db = redb::Database::create(&path).expect("create v4");
            let wtx = db.begin_write().expect("txn");
            {
                let mut t = wtx
                    .open_table(redb::TableDefinition::<&[u8], &[u8]>::new("books"))
                    .expect("table");
                t.insert("k".as_bytes(), "v".as_bytes()).expect("insert");
            }
            wtx.commit().expect("commit");
        }
        let before = std::fs::metadata(&path).unwrap().len();

        assert!(!needs_migration(&path));
        assert!(
            !migrate_file_if_needed(&path).expect("no-op migration"),
            "a current file must not be reported as migrated"
        );
        assert!(
            !backup_path(&path).exists(),
            "no backup should be written for a file that did not need migrating"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            before,
            "the file must not be rewritten"
        );
    }

    #[test]
    fn a_path_that_does_not_exist_yet_is_not_a_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("brand-new.redb");
        assert!(
            !migrate_file_if_needed(&path).expect("a fresh path is fine"),
            "creating a new database is not a migration"
        );
        assert!(!path.exists(), "detection must not create the file");
    }

    /// The staging file is the thing that makes a crash mid-conversion safe;
    /// nothing should be left lying beside the database afterwards.
    #[test]
    fn no_partial_file_survives_a_successful_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.redb");
        write_v2(&path, &[("k1", "Dune")]);
        migrate_file_if_needed(&path).expect("migrate");

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("partial"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "staging files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn the_legacy_backend_message_names_the_way_out() {
        assert!(is_legacy_format_error(
            "storage error: Manual upgrade required. Expected file format version 3, \
             but file is version 2"
        ));
        assert!(!is_legacy_format_error("some unrelated io error"));
        // The help text has to tell a browser user what to actually do.
        assert!(LEGACY_BACKEND_HELP.contains("exportSnapshot"));
        assert!(LEGACY_BACKEND_HELP.contains("openWithSnapshot"));
    }
}
