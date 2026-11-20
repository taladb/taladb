//! M5.1 probe (untracked): the CHANGELOG 0.13.2 repair recipe, copied
//! verbatim, run against damaged fixtures.
#![cfg(feature = "encryption")]
#![allow(unused_imports)]

use std::path::Path;
use std::sync::Arc;

use taladb::crypto::{EncryptedBackend, MIN_PBKDF2_ITERATIONS, derive_key};
use taladb::engine::{RedbBackend, StorageBackend};
use taladb::index::{META_VERSION_KEY, META_VERSION_TABLE};
use taladb::{CURRENT_SCHEMA_VERSION, Database, TalaDbError};

fn repair(db: &Path, salt_file: &Path, passphrase: &str) -> Result<(), TalaDbError> {
    let salt = std::fs::read(salt_file).map_err(|e| TalaDbError::Storage(e.to_string()))?;
    let key = derive_key(passphrase, &salt, MIN_PBKDF2_ITERATIONS)?;
    let raw: Arc<dyn StorageBackend> = Arc::new(RedbBackend::open(db)?);
    let enc = EncryptedBackend::new(raw, key);

    // 1. Decrypt every document. This proves the passphrase and the salt, and
    //    that no collection holds plaintext documents. On any error, stop:
    //    nothing has been written.
    {
        let txn = enc.begin_read()?;
        for table in txn.list_tables()? {
            if table.starts_with("docs::") {
                txn.scan_all(&table)?;
            }
        }
    }

    // 2. Write the schema version back, encrypted.
    let mut txn = enc.begin_write()?;
    txn.put(META_VERSION_TABLE, META_VERSION_KEY, &postcard::to_allocvec(&CURRENT_SCHEMA_VERSION)?)?;
    txn.commit()?;
    drop(enc);

    // 3. Check that the passphrase opens it again.
    Database::open_encrypted(db, passphrase)?.close()
}

use std::collections::BTreeMap;
use taladb::document::{Document, Value};
use taladb::Filter;

const PASS: &str = "correct horse battery staple";

fn salt(p: &Path) -> std::path::PathBuf { p.with_extension("redb.taladb-salt") }
fn fp(p: &Path) -> BTreeMap<String, Vec<(Vec<u8>, Vec<u8>)>> {
    let b = RedbBackend::open(p).unwrap();
    let t = b.begin_read().unwrap();
    t.list_tables().unwrap().into_iter().map(|n| { let r = t.scan_all(&n).unwrap(); (n, r) }).collect()
}
fn make_damaged(p: &Path, indexed: bool) {
    let db = Database::open_encrypted(p, PASS).unwrap();
    let c = db.collection("secrets").unwrap();
    if indexed { c.create_index("n").unwrap(); }
    for i in 0..5 { c.insert(vec![("n".into(), Value::Int(i))]).unwrap(); }
    db.close().unwrap();
    let raw = RedbBackend::open(p).unwrap();
    let mut w = raw.begin_write().unwrap();
    w.put(META_VERSION_TABLE, META_VERSION_KEY, &postcard::to_allocvec(&CURRENT_SCHEMA_VERSION).unwrap()).unwrap();
    w.commit().unwrap();
}

#[test]
fn recipe_repairs_damaged_file_both_shapes() {
    for indexed in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("app.redb");
        make_damaged(&p, indexed);
        assert!(Database::open_encrypted(&p, PASS).is_err());
        assert!(Database::open(&p).is_err());
        repair(&p, &salt(&p), PASS).expect("recipe repairs");
        let db = Database::open_encrypted(&p, PASS).unwrap();
        let c = db.collection("secrets").unwrap();
        assert_eq!(c.find(Filter::All).unwrap().len(), 5);
        assert_eq!(c.find(Filter::Eq("n".into(), Value::Int(3))).unwrap().len(), 1);
        db.close().unwrap();
        println!("RECIPE indexed={indexed}: repaired, 5 docs readable");
    }
}

#[test]
fn recipe_refuses_mixed_file_and_wrong_passphrase_without_writing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("app.redb");
    make_damaged(&p, false);
    let before = fp(&p);
    let e = repair(&p, &salt(&p), "wrong passphrase").unwrap_err();
    assert_eq!(fp(&p), before);
    println!("RECIPE wrong passphrase -> {e}");
    {
        let raw = RedbBackend::open(&p).unwrap();
        let mut w = raw.begin_write().unwrap();
        let doc = Document::new(vec![("x".into(), Value::Int(1))]);
        w.put("docs::secrets", &doc.id.to_bytes(), &postcard::to_allocvec(&doc).unwrap()).unwrap();
        w.commit().unwrap();
    }
    let before = fp(&p);
    let e = repair(&p, &salt(&p), PASS).unwrap_err();
    assert_eq!(fp(&p), before);
    println!("RECIPE mixed file -> {e}");
}
