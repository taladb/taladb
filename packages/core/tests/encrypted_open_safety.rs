//! S1 (v0.13.2 M4): opening an encrypted database the wrong way — without its
//! passphrase, with the wrong one, or a plaintext database *with* one — must be
//! refused before anything is written, and the correct passphrase must still
//! open the database afterwards.
//!
//! The defect this pins: a plain open read the encrypted schema-version record
//! as plaintext. postcard decoded the ciphertext's leading `0x01` format byte as
//! "version 1" and ignored the remaining 29 bytes, so the v1→v2 migration ran on
//! ciphertext and committed a *plaintext* version record. From then on the
//! correct passphrase failed with "ciphertext too short".
//!
//! "Unchanged" is checked logically: every table and every raw key/value pair,
//! read through an unencrypted `RedbBackend` (so metadata, index tables and
//! ciphertext are all compared byte for byte), plus the salt sidecar's bytes.
#![cfg(feature = "encryption")]

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use taladb::crypto::{EncryptedBackend, EncryptionKey, MIN_PBKDF2_ITERATIONS, derive_key};
use taladb::document::Value;
use taladb::engine::{RedbBackend, StorageBackend};
use taladb::index::{META_VERSION_KEY, META_VERSION_TABLE};
use taladb::migration::{BUILTIN_MIGRATIONS, read_version};
use taladb::{CURRENT_SCHEMA_VERSION, Database, Filter, Migration, TalaDbError};

const PASS: &str = "correct horse battery staple";
const DOCS: i64 = 5;

type Fingerprint = BTreeMap<String, Vec<(Vec<u8>, Vec<u8>)>>;

fn fingerprint_backend(backend: &dyn StorageBackend) -> Fingerprint {
    let txn = backend.begin_read().unwrap();
    txn.list_tables()
        .unwrap()
        .into_iter()
        .map(|t| {
            let rows = txn.scan_all(&t).unwrap();
            (t, rows)
        })
        .collect()
}

/// Every table and raw key/value pair in the file, read without decryption.
fn fingerprint(path: &Path) -> Fingerprint {
    fingerprint_backend(&RedbBackend::open(path).unwrap())
}

fn salt_path(path: &Path) -> PathBuf {
    path.with_extension("redb.taladb-salt")
}

fn s(v: &str) -> Value {
    Value::Str(v.into())
}

/// Whether the fixture carries a secondary index.
///
/// Both shapes matter. With an index, the baseline's plain open happened to
/// fail: the v1→v2 index rebuild could not postcard-decode ciphertext index
/// metadata, so the transaction aborted. Without one, nothing in the chain
/// failed and the plaintext version record was committed — the M1 repro.
#[derive(Clone, Copy, Debug)]
enum Shape {
    Indexed,
    NoIndex,
}
/// Both shapes; `S1_SHAPE=indexed|noindex` narrows to one so each can be
/// observed failing on the baseline separately.
fn shapes() -> Vec<Shape> {
    match std::env::var("S1_SHAPE").as_deref() {
        Ok("indexed") => vec![Shape::Indexed],
        Ok("noindex") => vec![Shape::NoIndex],
        _ => vec![Shape::Indexed, Shape::NoIndex],
    }
}

fn seed(db: &Database, shape: Shape) {
    let c = db.collection("secrets").unwrap();
    if let Shape::Indexed = shape {
        c.create_index("kind").unwrap();
    }
    for n in 0..DOCS {
        c.insert(vec![
            ("kind".into(), s("token")),
            ("n".into(), Value::Int(n)),
            ("secret".into(), s(&format!("top-secret-{n}"))),
        ])
        .unwrap();
    }
}

fn assert_contents(db: &Database) {
    let c = db.collection("secrets").unwrap();
    assert_eq!(c.find(Filter::All).unwrap().len() as i64, DOCS);
    // Served by the secondary index when there is one, so stale or damaged
    // index data shows.
    assert_eq!(
        c.find(Filter::Eq("kind".into(), s("token"))).unwrap().len() as i64,
        DOCS
    );
}

fn new_encrypted(path: &Path, shape: Shape) {
    let db = Database::open_encrypted(path, PASS).unwrap();
    seed(&db, shape);
    db.close().unwrap();
}

fn new_plain(path: &Path, shape: Shape) {
    let db = Database::open(path).unwrap();
    seed(&db, shape);
    db.close().unwrap();
}

/// Turn a current database into the shape every release before 0.7.3 left
/// behind: identical data, but no schema-version record (those releases never
/// ran the built-in migrations, so they never wrote one).
fn strip_version_record(path: &Path) {
    let raw = RedbBackend::open(path).unwrap();
    let mut w = raw.begin_write().unwrap();
    assert!(
        w.delete(META_VERSION_TABLE, META_VERSION_KEY)
            .unwrap()
            .is_some()
    );
    w.commit().unwrap();
}

fn assert_encrypted_opens(path: &Path) {
    let db = Database::open_encrypted(path, PASS).expect("correct passphrase must still open");
    assert_contents(&db);
    db.close().unwrap();
}

// ---------------------------------------------------------------------------
// Plain open of an encrypted database
// ---------------------------------------------------------------------------

#[test]
fn plain_open_of_encrypted_db_is_refused_and_writes_nothing() {
    for shape in shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.redb");
        new_encrypted(&path, shape);
        let before = fingerprint(&path);
        let salt = std::fs::read(salt_path(&path)).unwrap();

        let err = Database::open(&path)
            .err()
            .expect("a plain open of an encrypted database must be refused");
        assert_eq!(err.code(), "Encryption", "{err}");

        assert_eq!(fingerprint(&path), before, "a refused open must not write");
        assert_eq!(std::fs::read(salt_path(&path)).unwrap(), salt);
        assert_encrypted_opens(&path);
    }
}

#[test]
fn plain_open_with_user_migrations_is_refused_before_they_run() {
    for shape in shapes() {
        fn must_not_run(_: &mut dyn taladb::engine::WriteTxn) -> Result<(), TalaDbError> {
            panic!("a user migration ran against an encrypted database opened without its key");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.redb");
        new_encrypted(&path, shape);
        let before = fingerprint(&path);
        let user = [Migration {
            from_version: CURRENT_SCHEMA_VERSION,
            to_version: CURRENT_SCHEMA_VERSION + 1,
            description: "probe",
            up: must_not_run,
        }];
        assert!(Database::open_with_migrations(&path, &user).is_err());
        assert_eq!(fingerprint(&path), before);
        assert_encrypted_opens(&path);
    }
}

/// The no-write variant from M1: merely opening plainly used to be enough.
/// The repeated refused attempts must not accumulate damage either.
#[test]
fn repeated_rejected_opens_then_correct_passphrase_opens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secure.redb");
    new_encrypted(&path, Shape::NoIndex);
    let before = fingerprint(&path);
    for _ in 0..3 {
        assert!(Database::open(&path).is_err());
        assert!(Database::open_encrypted(&path, "wrong passphrase").is_err());
    }
    assert_eq!(fingerprint(&path), before);
    assert_encrypted_opens(&path);
}

#[test]
fn wrong_passphrase_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secure.redb");
    new_encrypted(&path, Shape::Indexed);
    let before = fingerprint(&path);
    let salt = std::fs::read(salt_path(&path)).unwrap();

    let err = Database::open_encrypted(&path, "wrong passphrase")
        .err()
        .expect("a wrong passphrase must be refused");
    assert_eq!(err.code(), "Encryption");
    assert!(err.to_string().contains("wrong passphrase"), "{err}");

    assert_eq!(fingerprint(&path), before);
    assert_eq!(std::fs::read(salt_path(&path)).unwrap(), salt);
    assert_encrypted_opens(&path);
}

// ---------------------------------------------------------------------------
// Legacy databases: no schema-version record (written by releases < 0.7.3)
// ---------------------------------------------------------------------------

#[test]
fn legacy_encrypted_db_without_version_record_refuses_plain_open() {
    for shape in shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.redb");
        new_encrypted(&path, shape);
        strip_version_record(&path);
        let before = fingerprint(&path);

        let err = Database::open(&path)
            .err()
            .expect("a legacy encrypted database must not be migrated as plaintext");
        assert_eq!(err.code(), "Encryption", "{err}");
        assert_eq!(fingerprint(&path), before);

        // A wrong passphrase must also fail before the v0 migration writes.
        assert!(Database::open_encrypted(&path, "wrong passphrase").is_err());
        assert_eq!(fingerprint(&path), before);

        // The correct passphrase opens it and runs the v0 chain under encryption.
        assert_encrypted_opens(&path);
        let raw = RedbBackend::open(&path).unwrap();
        let key = key_for(&path);
        let enc = EncryptedBackend::new(Arc::new(raw), key);
        assert_eq!(
            read_version(enc.begin_read().unwrap().as_ref()).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
    }
}

/// Positive control: the legacy check must not reject a legacy *plaintext* file.
#[test]
fn legacy_plaintext_db_without_version_record_still_opens_and_migrates() {
    for shape in shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy-plain.redb");
        new_plain(&path, shape);
        strip_version_record(&path);

        let db = Database::open(&path).expect("legacy plaintext database must open");
        assert_contents(&db);
        assert_eq!(
            read_version(db.backend().begin_read().unwrap().as_ref()).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
    }
}

// ---------------------------------------------------------------------------
// Encrypted open of a plaintext database
// ---------------------------------------------------------------------------

#[test]
fn passphrase_open_of_plaintext_db_is_refused_without_creating_a_salt() {
    for shape in shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.redb");
        new_plain(&path, shape);
        let before = fingerprint(&path);

        let err = Database::open_encrypted(&path, PASS)
            .err()
            .expect("a plaintext database must not be opened as encrypted");
        assert_eq!(err.code(), "Encryption", "{err}");
        assert_eq!(fingerprint(&path), before);
        assert!(
            !salt_path(&path).exists(),
            "a refused open must not leave a salt sidecar behind"
        );
        assert_contents(&Database::open(&path).unwrap());
    }
}

/// Legacy plaintext file with no indexes: nothing in the v0 chain fails
/// to decrypt, so the baseline committed an *encrypted* version record into
/// a plaintext file and the plain open stopped working.
#[test]
fn passphrase_open_of_legacy_plaintext_db_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy-plain.redb");
    {
        let db = Database::open(&path).unwrap();
        db.collection("notes")
            .unwrap()
            .insert(vec![("body".into(), s("hello"))])
            .unwrap();
        db.close().unwrap();
    }
    strip_version_record(&path);
    let before = fingerprint(&path);

    assert!(Database::open_encrypted(&path, PASS).is_err());
    assert_eq!(fingerprint(&path), before);
    assert!(!salt_path(&path).exists());
    let db = Database::open(&path).expect("the plaintext file must still open plainly");
    assert_eq!(
        db.collection("notes")
            .unwrap()
            .find(Filter::All)
            .unwrap()
            .len(),
        1
    );
}

// ---------------------------------------------------------------------------
// The `.taladb-salt` sidecar
// ---------------------------------------------------------------------------

/// The refusal is decided by the stored bytes, not by the sidecar, so losing
/// the sidecar does not reopen the hole — and a missing sidecar is reported
/// instead of silently replaced with a fresh random salt (which derives a
/// different key and can never decrypt the file).
#[test]
fn encrypted_db_with_missing_salt_sidecar() {
    for shape in shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.redb");
        new_encrypted(&path, shape);
        let salt = std::fs::read(salt_path(&path)).unwrap();
        std::fs::remove_file(salt_path(&path)).unwrap();
        let before = fingerprint(&path);

        assert!(
            Database::open(&path).is_err(),
            "plain open must still be refused"
        );
        let err = Database::open_encrypted(&path, PASS)
            .err()
            .expect("without its salt the key cannot be derived");
        assert!(err.to_string().contains("salt"), "{err}");
        assert!(
            !salt_path(&path).exists(),
            "a replacement salt must not be invented for existing ciphertext"
        );
        assert_eq!(fingerprint(&path), before);

        std::fs::write(salt_path(&path), &salt).unwrap();
        assert_encrypted_opens(&path);
    }
}

/// Releases up to 0.13.1 create the sidecar *before* finding out the file is
/// plaintext, so a stray sidecar next to a plaintext database is a state that
/// exists in the field. It must not lock the user out of their plain database.
#[test]
fn stray_salt_sidecar_next_to_plaintext_db_does_not_block_plain_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.redb");
    new_plain(&path, Shape::Indexed);
    std::fs::write(salt_path(&path), [7u8; 16]).unwrap();
    assert_contents(&Database::open(&path).unwrap());
}

/// A brand-new database still gets a sidecar and opens encrypted.
#[test]
fn fresh_encrypted_db_creates_its_salt() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.redb");
    new_encrypted(&path, Shape::Indexed);
    assert_eq!(std::fs::read(salt_path(&path)).unwrap().len(), 16);
    assert_encrypted_opens(&path);
}

fn key_for(path: &Path) -> EncryptionKey {
    let salt = std::fs::read(salt_path(path)).unwrap();
    derive_key(PASS, &salt, MIN_PBKDF2_ITERATIONS).unwrap()
}

// ---------------------------------------------------------------------------
// Strict schema-version metadata
// ---------------------------------------------------------------------------

fn put_raw_version(path: &Path, bytes: &[u8]) {
    let raw = RedbBackend::open(path).unwrap();
    let mut w = raw.begin_write().unwrap();
    w.put(META_VERSION_TABLE, META_VERSION_KEY, bytes).unwrap();
    w.commit().unwrap();
}

#[test]
fn malformed_version_record_is_refused_and_writes_nothing() {
    for bad in [
        &[][..],
        &[0x01, 0x00][..],
        &[0x02, 0xff, 0xff][..],
        &[0xff; 6][..],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.redb");
        new_plain(&path, Shape::Indexed);
        put_raw_version(&path, bad);
        let before = fingerprint(&path);
        assert!(
            Database::open(&path).is_err(),
            "version record {bad:02x?} must be refused"
        );
        assert_eq!(fingerprint(&path), before, "version record {bad:02x?}");
        // Not ciphertext-shaped, so no passphrase can help: the real error,
        // and no salt sidecar invented for it.
        // (`Serialization` when postcard cannot decode it at all, as before;
        // `Migration` when it decodes with bytes left over.)
        let err = Database::open_encrypted(&path, PASS).err().unwrap();
        assert!(
            matches!(err.code(), "Migration" | "Serialization"),
            "{bad:02x?}: {err}"
        );
        assert!(!salt_path(&path).exists());
        assert_eq!(fingerprint(&path), before, "version record {bad:02x?}");
    }
}

/// A future version is kept as stored, exactly as M3 (R1) settled.
#[test]
fn well_formed_version_records_still_open() {
    for v in [0u32, 1, CURRENT_SCHEMA_VERSION, 7, 300] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.redb");
        new_plain(&path, Shape::Indexed);
        put_raw_version(&path, &postcard::to_allocvec(&v).unwrap());
        let db = Database::open(&path).unwrap_or_else(|e| panic!("version {v}: {e}"));
        assert_contents(&db);
        assert_eq!(
            read_version(db.backend().begin_read().unwrap().as_ref()).unwrap(),
            v.max(CURRENT_SCHEMA_VERSION)
        );
    }
}

// ---------------------------------------------------------------------------
// Browser OPFS shape: redb over a caller-supplied storage backend, with
// `EncryptedBackend` above it — the same construction `openWithConfigAndOpfs`
// performs. No browser involved; the bytes live in a shared buffer.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct SharedBytes(Arc<Mutex<Vec<u8>>>);

impl taladb::redb::StorageBackend for SharedBytes {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.0.lock().unwrap().len() as u64)
    }
    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        let buf = self.0.lock().unwrap();
        let start = usize::try_from(offset).unwrap();
        let src = buf
            .get(start..start + out.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "read past end"))?;
        out.copy_from_slice(src);
        Ok(())
    }
    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        self.0
            .lock()
            .unwrap()
            .resize(usize::try_from(len).unwrap(), 0);
        Ok(())
    }
    fn sync_data(&self) -> Result<(), io::Error> {
        Ok(())
    }
    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        let mut buf = self.0.lock().unwrap();
        let start = usize::try_from(offset).unwrap();
        if start + data.len() > buf.len() {
            buf.resize(start + data.len(), 0);
        }
        buf[start..start + data.len()].copy_from_slice(data);
        Ok(())
    }
}

fn opfs_raw(bytes: &SharedBytes) -> RedbBackend {
    RedbBackend::open_with_redb_backend(bytes.clone()).unwrap()
}

fn opfs_open(bytes: &SharedBytes, key: Option<&EncryptionKey>) -> Result<Database, TalaDbError> {
    let raw = opfs_raw(bytes);
    let backend: Box<dyn StorageBackend> = match key {
        Some(k) => Box::new(EncryptedBackend::new(Arc::new(raw), k.clone())),
        None => Box::new(raw),
    };
    Database::open_with_backend(backend)
}

#[test]
fn opfs_encrypted_storage_refuses_plain_and_wrong_key_opens() {
    for shape in shapes() {
        let salt = [9u8; 16];
        let key = derive_key(PASS, &salt, MIN_PBKDF2_ITERATIONS).unwrap();
        let wrong = derive_key("wrong passphrase", &salt, MIN_PBKDF2_ITERATIONS).unwrap();
        let bytes = SharedBytes::default();
        {
            let db = opfs_open(&bytes, Some(&key)).unwrap();
            seed(&db, shape);
            db.close().unwrap();
        }
        let before = fingerprint_backend(&opfs_raw(&bytes));

        // e.g. a second tab without the passphrase taking over ownership.
        assert!(
            opfs_open(&bytes, None).is_err(),
            "plain OPFS open must be refused"
        );
        assert!(opfs_open(&bytes, Some(&wrong)).is_err());
        assert_eq!(fingerprint_backend(&opfs_raw(&bytes)), before);

        let db = opfs_open(&bytes, Some(&key)).expect("correct key must still open");
        assert_contents(&db);
    }
}

#[test]
fn opfs_legacy_encrypted_storage_without_version_record_refuses_plain_open() {
    for shape in shapes() {
        let salt = [3u8; 16];
        let key = derive_key(PASS, &salt, MIN_PBKDF2_ITERATIONS).unwrap();
        let bytes = SharedBytes::default();
        {
            let db = opfs_open(&bytes, Some(&key)).unwrap();
            seed(&db, shape);
            db.close().unwrap();
        }
        {
            let raw = opfs_raw(&bytes);
            let mut w = raw.begin_write().unwrap();
            w.delete(META_VERSION_TABLE, META_VERSION_KEY).unwrap();
            w.commit().unwrap();
        }
        let before = fingerprint_backend(&opfs_raw(&bytes));
        assert!(opfs_open(&bytes, None).is_err());
        assert_eq!(fingerprint_backend(&opfs_raw(&bytes)), before);
        assert_contents(&opfs_open(&bytes, Some(&key)).unwrap());
    }
}

/// The opposite direction in the browser: a passphrase supplied for an OPFS
/// file that was written in plaintext.
#[test]
fn opfs_plaintext_storage_refuses_a_key() {
    for shape in shapes() {
        let key = derive_key(PASS, &[5u8; 16], MIN_PBKDF2_ITERATIONS).unwrap();
        let bytes = SharedBytes::default();
        {
            let db = opfs_open(&bytes, None).unwrap();
            seed(&db, shape);
            db.close().unwrap();
        }
        let before = fingerprint_backend(&opfs_raw(&bytes));
        assert!(opfs_open(&bytes, Some(&key)).is_err());
        assert_eq!(fingerprint_backend(&opfs_raw(&bytes)), before);
        assert_contents(&opfs_open(&bytes, None).unwrap());
    }
}

// ---------------------------------------------------------------------------
// Snapshots and encrypted databases (with the M3 R1 restore migration)
// ---------------------------------------------------------------------------

/// Re-emit a snapshot from raw table contents, in `export_snapshot`'s format.
fn snapshot_from(tables: &Fingerprint) -> Vec<u8> {
    let mut out = b"TDBS".to_vec();
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(tables.len() as u32).to_le_bytes());
    for (name, rows) in tables {
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(rows.len() as u64).to_le_bytes());
        for (k, v) in rows {
            out.extend_from_slice(&(k.len() as u32).to_le_bytes());
            out.extend_from_slice(k);
            out.extend_from_slice(&(v.len() as u32).to_le_bytes());
            out.extend_from_slice(v);
        }
    }
    out
}

/// `export_snapshot` reads through the decrypting backend, so the snapshot
/// of an encrypted database is plaintext and restores as a plaintext
/// in-memory database at the current schema version.
#[test]
fn snapshot_of_encrypted_db_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secure.redb");
    new_encrypted(&path, Shape::Indexed);
    let snap = {
        let db = Database::open_encrypted(&path, PASS).unwrap();
        let snap = db.export_snapshot().unwrap();
        db.close().unwrap();
        snap
    };
    let restored = Database::restore_from_snapshot(&snap).unwrap();
    assert_contents(&restored);
    assert_eq!(
        read_version(restored.backend().begin_read().unwrap().as_ref()).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
}

/// R1 path: the same snapshot claiming schema v1 restores *and migrates* —
/// the index is rebuilt from the decoded documents.
#[test]
fn snapshot_of_encrypted_db_at_old_version_migrates_on_restore() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secure.redb");
    new_encrypted(&path, Shape::Indexed);
    let db = Database::open_encrypted(&path, PASS).unwrap();
    let mut tables = fingerprint_backend(db.backend());
    db.close().unwrap();
    // Plaintext v1 version record, and an emptied index the v1→v2 rebuild
    // must repopulate.
    tables.insert(
        META_VERSION_TABLE.into(),
        vec![(
            META_VERSION_KEY.to_vec(),
            postcard::to_allocvec(&1u32).unwrap(),
        )],
    );
    for (name, rows) in tables.iter_mut() {
        if name.starts_with("idx::") {
            rows.clear();
        }
    }
    let restored = Database::restore_from_snapshot(&snapshot_from(&tables)).unwrap();
    assert_contents(&restored);
    assert_eq!(
        read_version(restored.backend().begin_read().unwrap().as_ref()).unwrap(),
        CURRENT_SCHEMA_VERSION
    );
}

/// A snapshot that carries raw *ciphertext* (copied from the encrypted file,
/// or crafted) used to restore "successfully": R1's migration read the
/// ciphertext version as v1, ran on ciphertext and returned a database
/// marked v2 whose documents cannot be decoded.
#[test]
fn snapshot_holding_ciphertext_is_refused() {
    for shape in shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secure.redb");
        new_encrypted(&path, shape);
        let raw_tables = fingerprint(&path);
        assert!(Database::restore_from_snapshot(&snapshot_from(&raw_tables)).is_err());

        // Legacy shape (no version record) carrying ciphertext.
        let mut legacy = raw_tables.clone();
        legacy.remove(META_VERSION_TABLE);
        assert!(Database::restore_from_snapshot(&snapshot_from(&legacy)).is_err());
    }
}

/// Guards the test helper itself: with the S1 fix the built-in chain is still
/// exactly what the helper assumes.
#[test]
fn builtin_chain_ends_at_current_version() {
    assert_eq!(
        BUILTIN_MIGRATIONS.last().map(|m| m.to_version),
        Some(CURRENT_SCHEMA_VERSION)
    );
}

// ---------------------------------------------------------------------------
// Files already damaged by S1 in 0.13.1 and earlier
// ---------------------------------------------------------------------------

/// The baseline damage, exactly: a *plaintext* version record over otherwise
/// intact ciphertext (the no-index case commits nothing else). The fixed
/// passphrase open names that state instead of reporting a wrong passphrase
/// or "not encrypted", writes nothing, and the documented repair — rewrite
/// the version record through `EncryptedBackend` — restores access.
#[test]
fn s1_damage_from_older_releases_is_reported_and_repairable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("damaged.redb");
    new_encrypted(&path, Shape::NoIndex);
    put_raw_version(
        &path,
        &postcard::to_allocvec(&CURRENT_SCHEMA_VERSION).unwrap(),
    );
    let before = fingerprint(&path);

    let err = Database::open_encrypted(&path, PASS)
        .err()
        .expect("a damaged file cannot open as is");
    assert!(err.to_string().contains("0.13.1"), "{err}");
    assert_eq!(fingerprint(&path), before);

    // Repair (release-notes recipe), with nothing but public API.
    {
        let raw: Arc<dyn StorageBackend> = Arc::new(RedbBackend::open(&path).unwrap());
        let enc = EncryptedBackend::new(raw, key_for(&path));
        // Prove the key first: the first document must decrypt.
        let first_doc = {
            let r = enc.begin_read().unwrap();
            r.scan_all("docs::secrets").unwrap().into_iter().next()
        };
        assert!(first_doc.is_some());
        let mut w = enc.begin_write().unwrap();
        w.put(
            META_VERSION_TABLE,
            META_VERSION_KEY,
            &postcard::to_allocvec(&CURRENT_SCHEMA_VERSION).unwrap(),
        )
        .unwrap();
        w.commit().unwrap();
    }
    assert_encrypted_opens(&path);
}

/// The same damaged file, opened *without* a passphrase. Its version record
/// reads as plaintext, so until the document check this open succeeded and
/// later writes mixed plaintext documents into the encrypted file, which then
/// broke the passphrase open even after the repair. Refused now, before any
/// built-in or user migration and without writing, in both fixture shapes;
/// the repair still works afterwards.
#[test]
fn plain_open_of_s1_damaged_db_is_refused_and_writes_nothing() {
    for shape in shapes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("damaged.redb");
        new_encrypted(&path, shape);
        put_raw_version(
            &path,
            &postcard::to_allocvec(&CURRENT_SCHEMA_VERSION).unwrap(),
        );
        let before = fingerprint(&path);

        let err = Database::open(&path)
            .err()
            .unwrap_or_else(|| panic!("{shape:?}: a damaged file must not open plainly"));
        assert!(
            matches!(err, TalaDbError::Encryption(_)),
            "{shape:?}: {err}"
        );
        assert!(err.to_string().contains("0.13.1"), "{shape:?}: {err}");
        assert_eq!(fingerprint(&path), before, "{shape:?}");

        let user = [Migration {
            from_version: CURRENT_SCHEMA_VERSION,
            to_version: CURRENT_SCHEMA_VERSION + 1,
            description: "must never run on a damaged file",
            up: |_| panic!("user migration ran on a damaged file"),
        }];
        assert!(
            Database::open_with_migrations(&path, &user).is_err(),
            "{shape:?}"
        );
        assert_eq!(fingerprint(&path), before, "{shape:?}");

        repair_version_record(&path);
        assert_encrypted_opens(&path);
    }
}

/// A damaged file that a 0.13.1 plain open has already written to holds both
/// plaintext and ciphertext collections. One encrypted collection is enough to
/// refuse, whatever order the collections sort in.
#[test]
fn plain_open_of_s1_damaged_db_with_plaintext_collections_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mixed.redb");
    new_encrypted(&path, Shape::NoIndex);
    put_raw_version(
        &path,
        &postcard::to_allocvec(&CURRENT_SCHEMA_VERSION).unwrap(),
    );
    // What the 0.13.1 plain open would have written: plaintext documents in
    // collections sorting before and after the encrypted one.
    {
        let raw = RedbBackend::open(&path).unwrap();
        let mut w = raw.begin_write().unwrap();
        for table in ["docs::aaa", "docs::zzz"] {
            let doc = taladb::document::Document::new(vec![("x".into(), Value::Int(1))]);
            w.put(
                table,
                &doc.id.to_bytes(),
                &postcard::to_allocvec(&doc).unwrap(),
            )
            .unwrap();
        }
        w.commit().unwrap();
    }
    let before = fingerprint(&path);
    let err = Database::open(&path)
        .err()
        .expect("one encrypted collection is enough to refuse");
    assert!(err.to_string().contains("docs::secrets"), "{err}");
    assert_eq!(fingerprint(&path), before);
}

/// Positive controls: ordinary databases open exactly as before, through every
/// path that runs the check, and opening them writes nothing.
#[test]
fn document_check_keeps_ordinary_databases_opening() {
    for shape in shapes() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.redb");
        new_plain(&plain, shape);
        let before = fingerprint(&plain);
        let db = Database::open(&plain).expect("plaintext database opens");
        assert_contents(&db);
        db.close().unwrap();
        assert_eq!(fingerprint(&plain), before, "{shape:?}");

        let enc = dir.path().join("enc.redb");
        new_encrypted(&enc, shape);
        let before = fingerprint(&enc);
        assert_encrypted_opens(&enc);
        assert_eq!(fingerprint(&enc), before, "{shape:?}");

        // A custom backend: the encrypted file through `open_with_backend`.
        let raw: Arc<dyn StorageBackend> = Arc::new(RedbBackend::open(&enc).unwrap());
        let db = Database::open_with_backend(Box::new(EncryptedBackend::new(raw, key_for(&enc))))
            .expect("encrypted database opens through a custom backend");
        assert_contents(&db);
        db.close().unwrap();
    }
}

/// Inside an encrypted database, a first document that fails to decrypt is
/// tampering for the reader to report, not plain-open damage: the open still
/// succeeds (as before the check) and only that document's reads fail.
#[test]
fn tampered_first_document_in_encrypted_db_still_opens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tampered.redb");
    new_encrypted(&path, Shape::NoIndex);
    {
        let raw = RedbBackend::open(&path).unwrap();
        let (key, mut value) = raw
            .begin_read()
            .unwrap()
            .scan_all("docs::secrets")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let last = value.len() - 1;
        value[last] ^= 0xff; // break the GCM tag
        let mut w = raw.begin_write().unwrap();
        w.put("docs::secrets", &key, &value).unwrap();
        w.commit().unwrap();
    }
    let db = Database::open_encrypted(&path, PASS).expect("open is unaffected by one bad document");
    assert!(db.collection("secrets").unwrap().find(Filter::All).is_err());
    db.close().unwrap();
}

/// The release-notes repair: rewrite the version record through
/// `EncryptedBackend`, after proving the key on the first document.
fn repair_version_record(path: &Path) {
    let raw: Arc<dyn StorageBackend> = Arc::new(RedbBackend::open(path).unwrap());
    let enc = EncryptedBackend::new(raw, key_for(path));
    assert!(
        enc.begin_read()
            .unwrap()
            .scan_all("docs::secrets")
            .unwrap()
            .into_iter()
            .next()
            .is_some()
    );
    let mut w = enc.begin_write().unwrap();
    w.put(
        META_VERSION_TABLE,
        META_VERSION_KEY,
        &postcard::to_allocvec(&CURRENT_SCHEMA_VERSION).unwrap(),
    )
    .unwrap();
    w.commit().unwrap();
}
