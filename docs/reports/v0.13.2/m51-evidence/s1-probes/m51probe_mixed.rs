//! M5.1 probe (untracked): which S1 mixtures does #141's first-document check
//! catch? Prints observations instead of asserting where the answer is the
//! question.
#![cfg(feature = "encryption")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use taladb::crypto::{EncryptedBackend, EncryptionKey, MIN_PBKDF2_ITERATIONS, derive_key};
use taladb::document::{Document, Value, derive_doc_id};
use taladb::engine::{RedbBackend, StorageBackend};
use taladb::index::{META_VERSION_KEY, META_VERSION_TABLE};
use taladb::{CURRENT_SCHEMA_VERSION, Database, Filter};
use ulid::Ulid;

const PASS: &str = "correct horse battery staple";

fn salt_path(p: &Path) -> PathBuf {
    p.with_extension("redb.taladb-salt")
}
fn key_for(p: &Path) -> EncryptionKey {
    derive_key(PASS, &std::fs::read(salt_path(p)).unwrap(), MIN_PBKDF2_ITERATIONS).unwrap()
}
fn damage(p: &Path) {
    let raw = RedbBackend::open(p).unwrap();
    let mut w = raw.begin_write().unwrap();
    w.put(META_VERSION_TABLE, META_VERSION_KEY, &postcard::to_allocvec(&CURRENT_SCHEMA_VERSION).unwrap())
        .unwrap();
    w.commit().unwrap();
}
fn raw_plain_doc(p: &Path, table: &str, doc: &Document) {
    let raw = RedbBackend::open(p).unwrap();
    let mut w = raw.begin_write().unwrap();
    w.put(table, &doc.id.to_bytes(), &postcard::to_allocvec(doc).unwrap()).unwrap();
    w.commit().unwrap();
}
fn encrypted(p: &Path, n: i64) {
    let db = Database::open_encrypted(p, PASS).unwrap();
    let c = db.collection("secrets").unwrap();
    for i in 0..n {
        c.insert(vec![("n".into(), Value::Int(i))]).unwrap();
    }
    db.close().unwrap();
}
/// Release-notes repair, strict form: every docs:: table must decrypt fully
/// through EncryptedBackend before the version record is rewritten.
fn strict_repair(p: &Path) -> Result<(), String> {
    let raw: Arc<dyn StorageBackend> = Arc::new(RedbBackend::open(p).unwrap());
    let enc = EncryptedBackend::new(raw, key_for(p));
    {
        let r = enc.begin_read().unwrap();
        for t in r.list_tables().unwrap() {
            if t.starts_with("docs::") {
                r.scan_all(&t).map_err(|e| format!("{t}: {e}"))?;
            }
        }
    }
    let mut w = enc.begin_write().unwrap();
    w.put(META_VERSION_TABLE, META_VERSION_KEY, &postcard::to_allocvec(&CURRENT_SCHEMA_VERSION).unwrap())
        .unwrap();
    w.commit().unwrap();
    Ok(())
}
fn report(case: &str, p: &Path) {
    let plain = Database::open(p).map(|db| db.close());
    println!("PROBE {case}: plain open -> {}", match &plain {
        Ok(_) => "OPENED".to_string(),
        Err(e) => format!("refused: {e}"),
    });
    let enc = Database::open_encrypted(p, PASS)
        .and_then(|db| {
            let n = db.collection("secrets")?.find(Filter::All).map(|v| v.len());
            db.close()?;
            Ok(n)
        });
    println!("PROBE {case}: passphrase open before repair -> {enc:?}");
    println!("PROBE {case}: strict repair -> {:?}", strict_repair(p));
    let after = Database::open_encrypted(p, PASS).and_then(|db| {
        let n = db.collection("secrets")?.find(Filter::All).map(|v| v.len());
        db.close()?;
        Ok(n)
    });
    println!("PROBE {case}: passphrase open after repair -> {after:?}");
}

#[test]
fn a_same_collection_plaintext_sorts_after() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db.redb");
    encrypted(&p, 5);
    damage(&p);
    std::thread::sleep(std::time::Duration::from_millis(5));
    raw_plain_doc(&p, "docs::secrets", &Document::new(vec![("x".into(), Value::Int(1))]));
    report("A same-collection, plaintext AFTER ciphertext (new ULID)", &p);
}

#[test]
fn b_same_collection_plaintext_sorts_first() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db.redb");
    encrypted(&p, 5);
    damage(&p);
    // A caller-supplied / synced id older than every encrypted document.
    let old = Ulid::from_parts(1, 7);
    raw_plain_doc(&p, "docs::secrets", &Document::with_id(old, vec![("x".into(), Value::Int(1))]));
    report("B same-collection, plaintext FIRST (supplied old ULID)", &p);
}

#[test]
fn c_same_collection_derived_ids() {
    // derive_doc_id ids are hash-ordered; count how often a plaintext doc with
    // a derived id sorts before the encrypted docs (plain open then succeeds).
    let d = tempfile::tempdir().unwrap();
    let base = d.path().join("base.redb");
    encrypted(&base, 3);
    damage(&base);
    let mut opened = 0;
    let trials = 64;
    for i in 0..trials {
        let p = d.path().join(format!("db{i}.redb"));
        std::fs::copy(&base, &p).unwrap();
        let id = derive_doc_id("secrets", &format!("sku-{i}"));
        raw_plain_doc(&p, "docs::secrets", &Document::with_id(id, vec![("x".into(), Value::Int(1))]));
        if let Ok(db) = Database::open(&p) {
            opened += 1;
            db.close().unwrap();
        }
    }
    println!("PROBE C derived-id plaintext doc in encrypted collection: plain open OPENED in {opened}/{trials} trials");
}

#[test]
fn d_empty_encrypted_collection_with_index() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db.redb");
    {
        let db = Database::open_encrypted(&p, PASS).unwrap();
        db.collection("secrets").unwrap().create_index("n").unwrap();
        db.close().unwrap();
    }
    damage(&p);
    report("D encrypted DB, zero documents (index only)", &p);
}

#[test]
fn e_all_documents_deleted() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db.redb");
    encrypted(&p, 3);
    {
        let db = Database::open_encrypted(&p, PASS).unwrap();
        db.collection("secrets").unwrap().delete_many(Filter::All).unwrap();
        db.close().unwrap();
    }
    damage(&p);
    report("E encrypted DB, all documents deleted", &p);
}

#[test]
fn f_b_then_plain_writes_grow_damage() {
    // After B's plain open succeeds, does a plaintext insert through the API
    // land, and does the strict repair then refuse?
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("db.redb");
    encrypted(&p, 5);
    damage(&p);
    raw_plain_doc(
        &p,
        "docs::secrets",
        &Document::with_id(Ulid::from_parts(1, 7), vec![("x".into(), Value::Int(1))]),
    );
    match Database::open(&p) {
        Ok(db) => {
            let r = db.collection("secrets").unwrap().insert(vec![("y".into(), Value::Int(2))]);
            println!("PROBE F plain insert after B -> {:?}", r.map(|_| "inserted"));
            db.close().unwrap();
        }
        Err(e) => println!("PROBE F plain open refused: {e}"),
    }
    println!("PROBE F strict repair -> {:?}", strict_repair(&p));
}
