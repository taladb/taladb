//! Integration tests for AES-GCM-256 encryption at rest.
//!
//! Requires `--features encryption` to compile and run.

#![cfg(feature = "encryption")]

use std::sync::Arc;

use taladb_core::crypto::{EncryptedBackend, MIN_PBKDF2_ITERATIONS, decrypt, derive_key, encrypt};
use taladb_core::document::Value;
use taladb_core::engine::RedbBackend;
use taladb_core::{Database, Filter};
use zeroize::Zeroizing;

#[test]
fn public_encrypted_open_round_trips_and_rejects_wrong_passphrase() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secure.redb");
    {
        let db = Database::open_encrypted(&path, "correct horse battery staple").unwrap();
        db.collection("secrets")
            .unwrap()
            .insert(vec![("token".into(), Value::Str("top-secret".into()))])
            .unwrap();
    }
    assert!(path.with_extension("redb.taladb-salt").exists());
    assert!(
        !std::fs::read(&path)
            .unwrap()
            .windows(b"top-secret".len())
            .any(|w| w == b"top-secret")
    );
    assert!(Database::open_encrypted(&path, "wrong passphrase").is_err());
    let db = Database::open_encrypted(&path, "correct horse battery staple").unwrap();
    assert_eq!(
        db.collection("secrets")
            .unwrap()
            .find(Filter::All)
            .unwrap()
            .len(),
        1
    );
}

fn s(v: &str) -> Value {
    Value::Str(v.to_string())
}
fn i(n: i64) -> Value {
    Value::Int(n)
}

// ---------------------------------------------------------------------------
// Low-level encrypt / decrypt
// ---------------------------------------------------------------------------

#[test]
fn encrypt_decrypt_round_trip() {
    let key = Zeroizing::new([0u8; 32]);
    let plaintext = b"Hello, TalaDB encryption!";
    let ciphertext = encrypt(&key, "table", b"key", plaintext).unwrap();
    assert_ne!(
        ciphertext.as_slice(),
        plaintext,
        "ciphertext must differ from plaintext"
    );
    let recovered = decrypt(&key, "table", b"key", &ciphertext).unwrap();
    assert_eq!(recovered.as_slice(), plaintext);
}

#[test]
fn decrypt_with_wrong_key_fails() {
    let key_a = Zeroizing::new([0u8; 32]);
    let key_b = Zeroizing::new([1u8; 32]);
    let ciphertext = encrypt(&key_a, "table", b"key", b"secret data").unwrap();
    let result = decrypt(&key_b, "table", b"key", &ciphertext);
    assert!(result.is_err(), "decryption with wrong key must fail");
}

#[test]
fn decrypt_truncated_ciphertext_fails() {
    let key = Zeroizing::new([0u8; 32]);
    let result = decrypt(&key, "table", b"key", &[0u8; 5]); // shorter than minimum
    assert!(result.is_err(), "truncated ciphertext must return an error");
}

#[test]
fn encrypt_is_nondeterministic() {
    // Each call generates a fresh random nonce — same plaintext produces different ciphertext
    let key = Zeroizing::new([42u8; 32]);
    let plaintext = b"same message";
    let c1 = encrypt(&key, "table", b"key", plaintext).unwrap();
    let c2 = encrypt(&key, "table", b"key", plaintext).unwrap();
    assert_ne!(
        c1, c2,
        "two encryptions of the same plaintext must differ (random nonces)"
    );
}

#[test]
fn aad_table_mismatch_fails() {
    let key = Zeroizing::new([42u8; 32]);
    let ct = encrypt(&key, "table_a", b"key1", b"data").unwrap();
    assert!(
        decrypt(&key, "table_b", b"key1", &ct).is_err(),
        "ciphertext moved to a different table must fail"
    );
}

#[test]
fn aad_key_mismatch_fails() {
    let key = Zeroizing::new([42u8; 32]);
    let ct = encrypt(&key, "table_a", b"key1", b"data").unwrap();
    assert!(
        decrypt(&key, "table_a", b"key2", &ct).is_err(),
        "ciphertext with a different storage key must fail"
    );
}

// ---------------------------------------------------------------------------
// derive_key
// ---------------------------------------------------------------------------

#[test]
fn derive_key_is_deterministic() {
    let k1 = derive_key("passphrase", b"salt1234", MIN_PBKDF2_ITERATIONS).unwrap();
    let k2 = derive_key("passphrase", b"salt1234", MIN_PBKDF2_ITERATIONS).unwrap();
    assert_eq!(*k1, *k2, "same passphrase + salt must derive the same key");
}

#[test]
fn derive_key_differs_with_different_passphrase() {
    let k1 = derive_key("passphrase_a", b"salt", MIN_PBKDF2_ITERATIONS).unwrap();
    let k2 = derive_key("passphrase_b", b"salt", MIN_PBKDF2_ITERATIONS).unwrap();
    assert_ne!(*k1, *k2);
}

#[test]
fn derive_key_differs_with_different_salt() {
    let k1 = derive_key("passphrase", b"salt_a", MIN_PBKDF2_ITERATIONS).unwrap();
    let k2 = derive_key("passphrase", b"salt_b", MIN_PBKDF2_ITERATIONS).unwrap();
    assert_ne!(*k1, *k2);
}

#[test]
fn derive_key_rejects_low_iterations() {
    let err = derive_key("pass", b"salt1234567890ab", 999).unwrap_err();
    assert!(err.to_string().contains("iterations too low"));
}

// ---------------------------------------------------------------------------
// EncryptedBackend — full CRUD through Database
// ---------------------------------------------------------------------------

fn encrypted_db(key: [u8; 32]) -> Database {
    let inner = Arc::new(RedbBackend::open_in_memory().unwrap());
    let enc = Box::new(EncryptedBackend::new(inner, Zeroizing::new(key)));
    Database::open_with_backend(enc).unwrap()
}

#[test]
fn encrypted_backend_insert_and_find() {
    let key = [7u8; 32];
    let db = encrypted_db(key);
    let col = db.collection("secrets").unwrap();

    col.insert(vec![("secret".into(), s("my password"))])
        .unwrap();

    let results = col
        .find(Filter::Eq("secret".into(), s("my password")))
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].get("secret"), Some(&s("my password")));
}

#[test]
fn encrypted_backend_update_and_delete() {
    let key = [8u8; 32];
    let db = encrypted_db(key);
    let col = db.collection("data").unwrap();

    col.insert(vec![("value".into(), i(1))]).unwrap();

    let updated = col
        .update_one(
            Filter::Eq("value".into(), i(1)),
            taladb_core::Update::Set(vec![("value".into(), i(2))]),
        )
        .unwrap();
    assert!(updated);

    let results = col.find(Filter::Eq("value".into(), i(2))).unwrap();
    assert_eq!(results.len(), 1);

    let deleted = col.delete_one(Filter::Eq("value".into(), i(2))).unwrap();
    assert!(deleted);
    assert_eq!(col.find(Filter::All).unwrap().len(), 0);
}

#[test]
fn encrypted_backend_snapshot_round_trip() {
    let key = [9u8; 32];
    let db = encrypted_db(key);
    let col = db.collection("notes").unwrap();

    col.insert(vec![("body".into(), s("encrypted note"))])
        .unwrap();

    let snapshot = db.export_snapshot().unwrap();

    // Restore from snapshot into a fresh encrypted backend
    let inner2 = Arc::new(RedbBackend::open_in_memory().unwrap());
    let _enc2 = Box::new(EncryptedBackend::new(inner2, Zeroizing::new(key)));

    // Restore directly into in-memory (snapshot stores plaintext-after-decrypt)
    let db3 = Database::restore_from_snapshot(&snapshot).unwrap();
    let col3 = db3.collection("notes").unwrap();
    let results = col3.find(Filter::All).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].get("body"), Some(&s("encrypted note")));
}

#[test]
fn encrypted_backend_index_works() {
    let key = [10u8; 32];
    let db = encrypted_db(key);
    let col = db.collection("users").unwrap();

    col.create_index("email").unwrap();
    col.insert(vec![
        ("email".into(), s("alice@example.com")),
        ("age".into(), i(30)),
    ])
    .unwrap();
    col.insert(vec![
        ("email".into(), s("bob@example.com")),
        ("age".into(), i(25)),
    ])
    .unwrap();

    let results = col
        .find(Filter::Eq("email".into(), s("alice@example.com")))
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].get("age"), Some(&i(30)));
}

#[test]
fn encrypted_backend_multiple_docs() {
    let key = [11u8; 32];
    let db = encrypted_db(key);
    let col = db.collection("items").unwrap();

    for n in 0..20i64 {
        col.insert(vec![("n".into(), i(n))]).unwrap();
    }

    let all = col.find(Filter::All).unwrap();
    assert_eq!(all.len(), 20);

    let filtered = col.find(Filter::Gte("n".into(), i(10))).unwrap();
    assert_eq!(filtered.len(), 10);
}

// ---------------------------------------------------------------------------
// rekey — key rotation
// ---------------------------------------------------------------------------

#[test]
fn rekey_returns_positive_count_on_non_empty_db() {
    use taladb_core::engine::RedbBackend;
    use taladb_core::rekey;
    use zeroize::Zeroizing;

    let old_key = Zeroizing::new([1u8; 32]);
    let new_key = Zeroizing::new([2u8; 32]);

    let inner = Arc::new(RedbBackend::open_in_memory().unwrap());
    let inner_dyn: Arc<dyn taladb_core::StorageBackend> = inner.clone();
    let enc = Box::new(taladb_core::crypto::EncryptedBackend::new(
        inner_dyn,
        Zeroizing::new([1u8; 32]),
    ));
    let db = taladb_core::Database::open_with_backend(enc).unwrap();
    let col = db.collection("secrets").unwrap();
    col.insert(vec![("v".into(), s("hello"))]).unwrap();
    col.insert(vec![("v".into(), s("world"))]).unwrap();

    // Rekey the raw (non-encrypted) backend that the encrypted backend wraps.
    // We need access to the inner backend — use the inner RedbBackend directly.
    let count = rekey(inner.as_ref(), &old_key, &new_key).unwrap();
    assert!(
        count > 0,
        "rekey must report at least one re-encrypted value"
    );
}

#[test]
fn rekey_wrong_old_key_fails() {
    use taladb_core::engine::RedbBackend;
    use taladb_core::rekey;
    use zeroize::Zeroizing;

    let correct_key = Zeroizing::new([1u8; 32]);
    let wrong_key = Zeroizing::new([99u8; 32]);
    let new_key = Zeroizing::new([2u8; 32]);

    let inner = Arc::new(RedbBackend::open_in_memory().unwrap());
    let inner_dyn: Arc<dyn taladb_core::StorageBackend> = inner.clone();
    let enc = Box::new(taladb_core::crypto::EncryptedBackend::new(
        inner_dyn,
        Zeroizing::new([1u8; 32]),
    ));
    let db = taladb_core::Database::open_with_backend(enc).unwrap();
    db.collection("secrets")
        .unwrap()
        .insert(vec![("v".into(), s("hello"))])
        .unwrap();

    let result = rekey(inner.as_ref(), &wrong_key, &new_key);
    assert!(result.is_err(), "rekey with wrong old_key must fail");
    // Correct key should still work (data untouched after failed rekey)
    let ok = rekey(inner.as_ref(), &correct_key, &new_key);
    assert!(ok.is_ok());
}

#[test]
fn rekey_empty_db_returns_zero() {
    use taladb_core::engine::RedbBackend;
    use taladb_core::rekey;
    use zeroize::Zeroizing;

    let backend = RedbBackend::open_in_memory().unwrap();
    let old_key = Zeroizing::new([1u8; 32]);
    let new_key = Zeroizing::new([2u8; 32]);
    let count = rekey(&backend, &old_key, &new_key).unwrap();
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// Field-level encryption on Collection
// ---------------------------------------------------------------------------

fn field_enc_db() -> (taladb_core::Database, zeroize::Zeroizing<[u8; 32]>) {
    use zeroize::Zeroizing;
    let key = Zeroizing::new([42u8; 32]);
    let db = taladb_core::Database::open_in_memory().unwrap();
    (db, key)
}

#[test]
fn field_encryption_insert_find_round_trip() {
    use zeroize::Zeroizing;
    let (db, key) = field_enc_db();

    let col = db
        .collection("users")
        .unwrap()
        .with_field_encryption(vec!["ssn".into()], Zeroizing::new([42u8; 32]));

    col.insert(vec![
        ("name".into(), s("Alice")),
        ("ssn".into(), s("123-45-6789")),
    ])
    .unwrap();

    // Reading through same handle decrypts transparently
    let docs = col.find(Filter::All).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get("name"), Some(&s("Alice")));
    assert_eq!(docs[0].get("ssn"), Some(&s("123-45-6789")));

    drop(key);
}

#[test]
fn encrypted_field_not_queryable_by_plaintext_value() {
    use zeroize::Zeroizing;
    let (db, _key) = field_enc_db();

    let col = db
        .collection("records")
        .unwrap()
        .with_field_encryption(vec!["secret".into()], Zeroizing::new([42u8; 32]));

    col.insert(vec![("secret".into(), s("topsecret"))]).unwrap();

    // Querying by plaintext value on an encrypted field should return nothing
    // (the stored value is ciphertext bytes, not a string)
    let found = col
        .find(Filter::Eq("secret".into(), s("topsecret")))
        .unwrap();
    assert!(
        found.is_empty(),
        "encrypted field must not be queryable by plaintext value"
    );
}

#[test]
fn non_encrypted_field_remains_indexable() {
    use zeroize::Zeroizing;
    let (db, _key) = field_enc_db();

    let col = db
        .collection("users")
        .unwrap()
        .with_field_encryption(vec!["ssn".into()], Zeroizing::new([42u8; 32]));
    col.create_index("email").unwrap();

    col.insert(vec![
        ("email".into(), s("alice@example.com")),
        ("ssn".into(), s("111-22-3333")),
    ])
    .unwrap();
    col.insert(vec![
        ("email".into(), s("bob@example.com")),
        ("ssn".into(), s("444-55-6666")),
    ])
    .unwrap();

    let found = col
        .find(Filter::Eq("email".into(), s("alice@example.com")))
        .unwrap();
    assert_eq!(found.len(), 1);
    // Encrypted field still decrypts on this handle
    assert_eq!(found[0].get("ssn"), Some(&s("111-22-3333")));
}

#[test]
fn field_encryption_multiple_docs() {
    use zeroize::Zeroizing;
    let (db, _key) = field_enc_db();

    let col = db
        .collection("items")
        .unwrap()
        .with_field_encryption(vec!["pin".into()], Zeroizing::new([42u8; 32]));

    for n in 0..10i64 {
        col.insert(vec![
            ("n".into(), i(n)),
            ("pin".into(), s(&format!("pin-{n}"))),
        ])
        .unwrap();
    }

    let docs = col.find(Filter::All).unwrap();
    assert_eq!(docs.len(), 10);
    for doc in &docs {
        let pin = doc.get("pin").unwrap();
        if let Value::Str(p) = pin {
            assert!(p.starts_with("pin-"), "pin must decrypt correctly: {p}");
        } else {
            panic!("pin field must decrypt to a string, got {pin:?}");
        }
    }
}

#[test]
fn field_encryption_survives_snapshot_round_trip() {
    use zeroize::Zeroizing;
    let (db, _key) = field_enc_db();

    let col = db
        .collection("secrets")
        .unwrap()
        .with_field_encryption(vec!["token".into()], Zeroizing::new([42u8; 32]));
    col.insert(vec![
        ("name".into(), s("Alice")),
        ("token".into(), s("abc123")),
    ])
    .unwrap();

    let bytes = db.export_snapshot().unwrap();
    let db2 = taladb_core::Database::restore_from_snapshot(&bytes).unwrap();

    // Open with same field encryption config on the restored db
    let col2 = db2
        .collection("secrets")
        .unwrap()
        .with_field_encryption(vec!["token".into()], Zeroizing::new([42u8; 32]));

    let docs = col2.find(Filter::All).unwrap();
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].get("name"), Some(&s("Alice")));
    assert_eq!(docs[0].get("token"), Some(&s("abc123")));
}
