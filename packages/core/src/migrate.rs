//! Bringing a pre-redb-4 database file forward.
//!
//! redb 4 changed the on-disk format and refuses anything older outright —
//! `"Manual upgrade required. Expected file format version 3, but file is
//! version 2"` — and offers no upgrade API of its own. Without something here,
//! every database written by a previous release of TalaDB would simply stop
//! opening.
//!
//! The approach is to keep redb 2 linked alongside redb 4 (they are separate
//! crates as far as cargo is concerned, so both can be present) and copy the
//! data forward: read every table with the version that wrote the file, write it
//! into a fresh v4 file, then swap.
//!
//! # Why a copy rather than an in-place rewrite
//!
//! The conversion writes to a temporary file and renames it over the original
//! only once it has been fully written and synced. A process killed halfway
//! through leaves the original untouched — the rename is the only step that
//! changes what the database path points at, and on every platform TalaDB
//! targets that is atomic. An in-place rewrite would have a window where the
//! file is neither valid v2 nor valid v4.
//!
//! A `.v2.bak` copy is kept as well. The rename makes the swap safe against a
//! crash, but not against this code being *wrong*, and a database is not
//! something a user can regenerate.

// The file-based migration is native-only: it needs a filesystem to copy and
// atomically rename. Everything wasm keeps is below — the help text and the error
// sniffer, which the OPFS path uses to report something actionable.
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

#[cfg(not(target_arch = "wasm32"))]
use crate::error::TalaDbError;

/// Extension appended to the pre-migration copy of the database.
#[cfg(not(target_arch = "wasm32"))]
const BACKUP_SUFFIX: &str = "v2.bak";

/// Does this file need bringing forward before redb 4 will open it?
///
/// Detection is by *attempting the open*, not by parsing the header: redb owns
/// its format, and a hand-rolled version check here would be one more thing to
/// keep in step with a dependency that has already changed format twice.
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
pub fn migrate_file_if_needed(path: &Path) -> Result<bool, TalaDbError> {
    if !path.exists() || !needs_migration(path) {
        return Ok(false);
    }

    let backup = backup_path(path);
    std::fs::copy(path, &backup).map_err(|e| {
        TalaDbError::Storage(format!(
            "could not back up {} before migrating it: {e}",
            path.display()
        ))
    })?;

    // Write beside the original so the rename below stays on one filesystem —
    // a cross-device rename is not atomic and, on most platforms, not permitted.
    let staging = path.with_extension("v4.partial");
    let _ = std::fs::remove_file(&staging);

    match convert(path, &staging) {
        Ok(()) => {}
        Err(e) => {
            // Leave nothing half-written behind; the original is still intact.
            let _ = std::fs::remove_file(&staging);
            return Err(e);
        }
    }

    std::fs::rename(&staging, path).map_err(|e| {
        let _ = std::fs::remove_file(&staging);
        TalaDbError::Storage(format!(
            "migrated database could not replace {}: {e} (the original is unchanged, \
             and a copy is at {})",
            path.display(),
            backup.display()
        ))
    })?;

    Ok(true)
}

/// Where the pre-migration copy is kept.
#[cfg(not(target_arch = "wasm32"))]
fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".");
    name.push(BACKUP_SUFFIX);
    PathBuf::from(name)
}

/// Copy every table from a redb 2 database into a new redb 4 one.
#[cfg(not(target_arch = "wasm32"))]
fn convert(src: &Path, dst: &Path) -> Result<(), TalaDbError> {
    use redb2::{ReadableTable as _, TableHandle as _};

    let old = redb2::Database::open(src).map_err(|e| {
        TalaDbError::Storage(format!("could not open {} as redb 2: {e}", src.display()))
    })?;
    let rtx = old
        .begin_read()
        .map_err(|e| TalaDbError::Storage(format!("redb 2 read transaction failed: {e}")))?;

    let tables: Vec<String> = rtx
        .list_tables()
        .map_err(|e| TalaDbError::Storage(format!("could not list redb 2 tables: {e}")))?
        .map(|h| h.name().to_string())
        .collect();

    let new = redb::Database::create(dst)
        .map_err(|e| TalaDbError::Storage(format!("could not create {}: {e}", dst.display())))?;
    {
        let wtx = new
            .begin_write()
            .map_err(|e| TalaDbError::Storage(format!("redb 4 write transaction failed: {e}")))?;
        for name in &tables {
            // Every TalaDB table is bytes-to-bytes; the typing above this layer
            // lives in the encoding, not in redb's schema, so one definition
            // covers all of them.
            let def_old: redb2::TableDefinition<&[u8], &[u8]> = redb2::TableDefinition::new(name);
            let def_new: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new(name);

            let from = rtx.open_table(def_old).map_err(|e| {
                TalaDbError::Storage(format!("could not read table \"{name}\": {e}"))
            })?;
            let mut to = wtx.open_table(def_new).map_err(|e| {
                TalaDbError::Storage(format!("could not create table \"{name}\": {e}"))
            })?;
            for entry in from
                .iter()
                .map_err(|e| TalaDbError::Storage(format!("could not scan \"{name}\": {e}")))?
            {
                let (k, v) = entry
                    .map_err(|e| TalaDbError::Storage(format!("bad entry in \"{name}\": {e}")))?;
                to.insert(k.value(), v.value()).map_err(|e| {
                    TalaDbError::Storage(format!("could not write into \"{name}\": {e}"))
                })?;
            }
        }
        wtx.commit()
            .map_err(|e| TalaDbError::Storage(format!("could not commit migration: {e}")))?;
    }

    // Drop the handle so the file is closed and flushed before it is renamed
    // over the original.
    drop(new);
    Ok(())
}

/// The message given to callers whose database lives somewhere this code cannot
/// migrate — the browser's OPFS, or any custom `StorageBackend`.
///
/// There is no filesystem to copy or rename there, and the second file the
/// conversion needs would have to be supplied by the host. Rather than fail with
/// redb's own wording, which says "manual upgrade" without saying how, callers
/// get something they can act on.
pub const LEGACY_BACKEND_HELP: &str = "this database was written by an older version of TalaDB \
     (redb file format 2) and cannot be opened directly. Export a snapshot with \
     the previous version (`exportSnapshot`) and restore it here \
     (`openWithSnapshot`), which rewrites the data in the current format.";

/// Recognise redb's pre-v4 format complaint so a custom backend can report
/// something actionable instead of passing the raw storage error up.
#[must_use]
pub fn is_legacy_format_error(msg: &str) -> bool {
    msg.contains("Manual upgrade required") || msg.contains("file is version 2")
}

#[cfg(all(test, not(target_arch = "wasm32")))]
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
