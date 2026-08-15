//! Encryption at rest for TalaDB.
//!
//! Provides an `EncryptedBackend` wrapper that implements `StorageBackend`
//! and transparently encrypts/decrypts all stored values using AES-GCM-256.
//!
//! Binary format (per stored value)
//! ----------------------------------
//! ```text
//! [ version: 1 byte (0x01) ][ nonce: 12 bytes ][ ciphertext + GCM tag (16 B) ]
//! ```
//! The version byte allows future nonce-size changes without silent data
//! corruption — an unknown version byte returns a descriptive error.
//!
//! Authenticated Associated Data (AAD)
//! -------------------------------------
//! Every encryption call binds the value to its storage location by using
//! `"<table>\x00<hex-key>"` as AAD. This prevents a ciphertext from one
//! table/key from being silently moved to another and still decrypting.
//!
//! Thread safety
//! -------------
//! `EncryptedBackend` is `Send + Sync` as long as the inner backend is.
//!
//! Crate features
//! --------------
//! Requires the `encryption` feature flag to pull in `aes-gcm`, `rand`,
//! and `zeroize`. Without it, the module still compiles but `EncryptedBackend`
//! is gated behind a compile error.

use std::ops::Bound;
use std::sync::Arc;

use crate::engine::{KvPairs, ReadTxn, StorageBackend, WriteTxn};
use crate::error::TalaDbError;

// ---------------------------------------------------------------------------
// AES-GCM-256 primitives
// ---------------------------------------------------------------------------

// Encryption primitives require `features = ["encryption"]` in Cargo.toml.
// When enabled, Cargo pulls in: aes-gcm 0.10, rand 0.8, pbkdf2 0.12,
// hmac 0.12, sha2 0.10, zeroize 1.7.

/// Version tag prepended to every encrypted value.
/// Increment this and add a migration path if the format ever changes.
#[cfg(feature = "encryption")]
const CRYPTO_FORMAT_V1: u8 = 0x01;

/// AES-GCM nonce length for format version 1.
#[cfg(feature = "encryption")]
const NONCE_LEN_V1: usize = 12;

/// Minimum PBKDF2-HMAC-SHA256 iteration count (OWASP 2024 recommendation).
pub const MIN_PBKDF2_ITERATIONS: u32 = 600_000;

/// A 256-bit encryption key.
///
/// When the `encryption` feature is enabled, this is a `Zeroizing` wrapper so
/// the key bytes are overwritten on drop, preventing them from lingering in
/// memory.
#[cfg(feature = "encryption")]
pub type EncryptionKey = zeroize::Zeroizing<[u8; 32]>;

/// Stub type used when the `encryption` feature is disabled.
#[cfg(not(feature = "encryption"))]
pub type EncryptionKey = [u8; 32];

// ---------------------------------------------------------------------------
// Internal helper: build AAD from table name + raw key bytes
// ---------------------------------------------------------------------------

/// Construct the Authenticated Associated Data string for a given
/// `(table, key)` pair.  Using AAD prevents a ciphertext from being silently
/// relocated to a different table or key slot.
#[cfg(feature = "encryption")]
fn make_aad(table: &str, key: &[u8]) -> Vec<u8> {
    // Format: "<table>\0<hex-encoded key bytes>"
    // The null byte separator ensures the table name cannot be confused with
    // the start of the hex key (table names are never null-terminated).
    let mut aad = table.as_bytes().to_vec();
    aad.push(0x00);
    for b in key {
        aad.push(b"0123456789abcdef"[(b >> 4) as usize]);
        aad.push(b"0123456789abcdef"[(b & 0xf) as usize]);
    }
    aad
}

// ---------------------------------------------------------------------------
// encrypt / decrypt — public primitives
// ---------------------------------------------------------------------------

/// Encrypt `plaintext` with AES-GCM-256, binding the result to `(table, key)`.
///
/// Returns `[0x01][nonce (12 B)][ciphertext][GCM tag (16 B)]`.
///
/// # Errors
/// Returns `TalaDbError::Encryption` on failure.
pub fn encrypt(
    enc_key: &EncryptionKey,
    table: &str,
    key: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, TalaDbError> {
    #[cfg(feature = "encryption")]
    {
        use aes_gcm::aead::{Aead, KeyInit, OsRng};
        use aes_gcm::{Aes256Gcm, Nonce};
        use rand::RngCore;

        let cipher = Aes256Gcm::new_from_slice(enc_key.as_ref())
            .map_err(|e| TalaDbError::Encryption(e.to_string()))?;

        let mut nonce_bytes = [0u8; NONCE_LEN_V1];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let aad = make_aad(table, key);
        let payload = aes_gcm::aead::Payload {
            msg: plaintext,
            aad: &aad,
        };
        let ciphertext = cipher
            .encrypt(nonce, payload)
            .map_err(|e| TalaDbError::Encryption(e.to_string()))?;

        let mut out = Vec::with_capacity(1 + NONCE_LEN_V1 + ciphertext.len());
        out.push(CRYPTO_FORMAT_V1);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }
    #[cfg(not(feature = "encryption"))]
    {
        let _ = (enc_key, table, key, plaintext);
        Err(TalaDbError::Encryption(
            "encrypt() called but the `encryption` feature is not enabled; \
             enable it in Cargo.toml: taladb-core = { features = [\"encryption\"] }"
                .into(),
        ))
    }
}

/// Decrypt a value produced by `encrypt`, verifying that it belongs to `(table, key)`.
///
/// # Errors
/// Returns `TalaDbError::Encryption` on authentication failure, unknown version,
/// or bad input.
pub fn decrypt(
    enc_key: &EncryptionKey,
    table: &str,
    key: &[u8],
    data: &[u8],
) -> Result<Vec<u8>, TalaDbError> {
    #[cfg(feature = "encryption")]
    {
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Nonce};

        // Minimum: 1 version byte + 12 nonce bytes + 16 GCM tag bytes = 29
        if data.len() < 1 + NONCE_LEN_V1 + 16 {
            return Err(TalaDbError::Encryption("ciphertext too short".into()));
        }

        let version = data[0];
        if version != CRYPTO_FORMAT_V1 {
            return Err(TalaDbError::Encryption(format!(
                "unsupported encrypted-value format version {version:#04x}; \
                 expected {CRYPTO_FORMAT_V1:#04x}"
            )));
        }

        let nonce_bytes = &data[1..1 + NONCE_LEN_V1];
        let ciphertext = &data[1 + NONCE_LEN_V1..];

        let cipher = Aes256Gcm::new_from_slice(enc_key.as_ref())
            .map_err(|e| TalaDbError::Encryption(e.to_string()))?;
        let nonce = Nonce::from_slice(nonce_bytes);

        let aad = make_aad(table, key);
        let payload = aes_gcm::aead::Payload {
            msg: ciphertext,
            aad: &aad,
        };
        cipher
            .decrypt(nonce, payload)
            .map_err(|e| TalaDbError::Encryption(e.to_string()))
    }
    #[cfg(not(feature = "encryption"))]
    {
        let _ = (enc_key, table, key, data);
        Err(TalaDbError::Encryption(
            "decrypt() called but the `encryption` feature is not enabled; \
             enable it in Cargo.toml: taladb-core = { features = [\"encryption\"] }"
                .into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// EncryptedBackend — wraps any StorageBackend and encrypts all values
// ---------------------------------------------------------------------------

pub struct EncryptedBackend {
    inner: Arc<dyn StorageBackend>,
    key: EncryptionKey,
}

impl EncryptedBackend {
    /// Create an encrypted backend.
    ///
    /// **Requires** the `encryption` feature flag. Without it this function
    /// does not exist, making misconfiguration a compile-time error.
    #[cfg(feature = "encryption")]
    pub fn new(inner: Arc<dyn StorageBackend>, key: EncryptionKey) -> Self {
        Self { inner, key }
    }
}

impl StorageBackend for EncryptedBackend {
    fn begin_write(&self) -> Result<Box<dyn WriteTxn + '_>, TalaDbError> {
        let inner_txn = self.inner.begin_write()?;
        Ok(Box::new(EncryptedWriteTxn {
            inner: inner_txn,
            key: &self.key,
        }))
    }

    fn begin_read(&self) -> Result<Box<dyn ReadTxn + '_>, TalaDbError> {
        let inner_txn = self.inner.begin_read()?;
        Ok(Box::new(EncryptedReadTxn {
            inner: inner_txn,
            key: &self.key,
        }))
    }

    fn set_durability(&self, eventual: bool) {
        self.inner.set_durability(eventual);
    }

    fn flush(&self) -> Result<(), TalaDbError> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// EncryptedWriteTxn
// ---------------------------------------------------------------------------

struct EncryptedWriteTxn<'a> {
    inner: Box<dyn WriteTxn + 'a>,
    key: &'a EncryptionKey,
}

impl<'a> WriteTxn for EncryptedWriteTxn<'a> {
    fn put(&mut self, table: &str, key: &[u8], value: &[u8]) -> Result<(), TalaDbError> {
        let encrypted = encrypt(self.key, table, key, value)?;
        self.inner.put(table, key, &encrypted)
    }

    fn delete(&mut self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        self.inner.delete(table, key)
    }

    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        match self.inner.get(table, key)? {
            Some(data) => Ok(Some(decrypt(self.key, table, key, &data)?)),
            None => Ok(None),
        }
    }

    fn range(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, TalaDbError> {
        let raw = self.inner.range(table, start, end)?;
        raw.into_iter()
            .map(|(k, v)| {
                let plain = decrypt(self.key, table, &k, &v)?;
                Ok((k, plain))
            })
            .collect()
    }

    fn commit(self: Box<Self>) -> Result<(), TalaDbError> {
        self.inner.commit()
    }

    // Table names are not encrypted, so both pass straight through.
    fn list_tables(&self) -> Result<Vec<String>, TalaDbError> {
        self.inner.list_tables()
    }

    fn delete_table(&mut self, table: &str) -> Result<bool, TalaDbError> {
        self.inner.delete_table(table)
    }

    fn get_many(&self, table: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        self.inner
            .get_many(table, keys)?
            .into_iter()
            .zip(keys)
            .map(|(v, k)| match v {
                Some(data) => Ok(Some(decrypt(self.key, table, k, &data)?)),
                None => Ok(None),
            })
            .collect()
    }

    fn apply_batch(
        &mut self,
        table: &str,
        ops: &[crate::engine::KvOp<'_>],
    ) -> Result<(), TalaDbError> {
        use crate::engine::KvOp;
        // Values must be encrypted before reaching the inner backend, and the
        // ciphertext has to outlive the borrowed-slice batch handed down — so
        // it is materialised here rather than borrowed from the caller.
        let mut ciphertexts: Vec<Option<Vec<u8>>> = Vec::with_capacity(ops.len());
        for op in ops {
            match op {
                KvOp::Put(k, v) => ciphertexts.push(Some(encrypt(self.key, table, k, v)?)),
                KvOp::Delete(_) => ciphertexts.push(None),
            }
        }
        let inner_ops: Vec<KvOp<'_>> = ops
            .iter()
            .zip(&ciphertexts)
            .map(|(op, ct)| match (op, ct) {
                (KvOp::Put(k, _), Some(ct)) => KvOp::Put(k, ct.as_slice()),
                (KvOp::Delete(k), _) => KvOp::Delete(k),
                // Unreachable: the loop above pairs every Put with a Some.
                (KvOp::Put(k, v), None) => KvOp::Put(k, v),
            })
            .collect();
        self.inner.apply_batch(table, &inner_ops)
    }
}

// ---------------------------------------------------------------------------
// EncryptedReadTxn
// ---------------------------------------------------------------------------

struct EncryptedReadTxn<'a> {
    inner: Box<dyn ReadTxn + 'a>,
    key: &'a EncryptionKey,
}

impl<'a> ReadTxn for EncryptedReadTxn<'a> {
    fn get(&self, table: &str, key: &[u8]) -> Result<Option<Vec<u8>>, TalaDbError> {
        match self.inner.get(table, key)? {
            Some(data) => Ok(Some(decrypt(self.key, table, key, &data)?)),
            None => Ok(None),
        }
    }

    fn range(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
    ) -> Result<KvPairs, TalaDbError> {
        let raw = self.inner.range(table, start, end)?;
        raw.into_iter()
            .map(|(k, v)| {
                let plain = decrypt(self.key, table, &k, &v)?;
                Ok((k, plain))
            })
            .collect()
    }

    fn scan_all(&self, table: &str) -> Result<KvPairs, TalaDbError> {
        let raw = self.inner.scan_all(table)?;
        raw.into_iter()
            .map(|(k, v)| {
                let plain = decrypt(self.key, table, &k, &v)?;
                Ok((k, plain))
            })
            .collect()
    }

    fn list_tables(&self) -> Result<Vec<String>, TalaDbError> {
        self.inner.list_tables()
    }

    fn count_entries(&self, table: &str) -> Result<u64, TalaDbError> {
        self.inner.count_entries(table)
    }

    fn get_many(&self, table: &str, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>, TalaDbError> {
        self.inner
            .get_many(table, keys)?
            .into_iter()
            .zip(keys)
            .map(|(v, k)| match v {
                Some(data) => Ok(Some(decrypt(self.key, table, k, &data)?)),
                None => Ok(None),
            })
            .collect()
    }

    fn scan(
        &self,
        table: &str,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
        f: crate::engine::ScanFn<'_>,
    ) -> Result<(), TalaDbError> {
        // Values arrive encrypted, so this layer cannot hand the caller a
        // borrowed slice from storage — each value is decrypted into a buffer
        // first. Keys stay borrowed, and the caller still avoids materialising
        // the whole range.
        let mut err = None;
        self.inner.scan(table, start, end, &mut |k, v| {
            match decrypt(self.key, table, k, v) {
                Ok(plain) => f(k, &plain),
                Err(e) => {
                    err = Some(e);
                    Ok(crate::engine::ScanFlow::Stop)
                }
            }
        })?;
        match err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Key derivation helpers
// ---------------------------------------------------------------------------

/// Derive a 256-bit key from a passphrase using PBKDF2-HMAC-SHA256.
///
/// `salt` should be at least 16 random bytes, unique per database.
/// `iterations` must be at least [`MIN_PBKDF2_ITERATIONS`] (600 000).
///
/// Returns a zeroizing 32-byte key suitable for `EncryptedBackend::new`.
///
/// **Requires** the `encryption` feature flag.
pub fn derive_key(
    passphrase: &str,
    salt: &[u8],
    iterations: u32,
) -> Result<EncryptionKey, TalaDbError> {
    #[cfg(feature = "encryption")]
    {
        use pbkdf2::pbkdf2_hmac;
        use sha2::Sha256;

        if iterations < MIN_PBKDF2_ITERATIONS {
            return Err(TalaDbError::Config(format!(
                "PBKDF2 iterations too low ({iterations}); \
                 minimum is {MIN_PBKDF2_ITERATIONS}"
            )));
        }

        let mut key = zeroize::Zeroizing::new([0u8; 32]);
        pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, iterations, key.as_mut());
        Ok(key)
    }
    #[cfg(not(feature = "encryption"))]
    {
        let _ = (passphrase, salt, iterations);
        Err(TalaDbError::Encryption(
            "derive_key() called but the `encryption` feature is not enabled; \
             enable it in Cargo.toml: taladb-core = { features = [\"encryption\"] }"
                .into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Migration: v0 → v1
// ---------------------------------------------------------------------------

/// Migrate a database encrypted with `< 0.6.2` to the current v1 format.
///
/// The `0.6.2` release changed the AES-GCM encrypted-value format by adding:
/// - A 1-byte version prefix (`0x01`).
/// - Authenticated Associated Data (AAD) binding each ciphertext to its
///   storage `(table, key)` location.
///
/// Any database encrypted by an older build cannot be decrypted by `>= 0.6.2`
/// because the AAD check will fail.  Call this function **once** after
/// upgrading to re-encrypt all stored values.
///
/// # What it does
/// For every table in the backend:
/// 1. Reads every `(key, value)` pair in a read transaction.
/// 2. Attempts to decrypt each value using the old 2-argument format
///    (`[12-byte nonce][ciphertext]`, no version byte, no AAD).
/// 3. Re-encrypts the plaintext using the new 4-argument `encrypt(key, table,
///    raw_key, plain)` call which prepends the version byte and binds AAD.
/// 4. Writes all updates for the table in a single atomic write transaction.
///
/// Returns the total number of values that were re-encrypted.
///
/// # Errors
/// Returns `TalaDbError::Encryption` if any value cannot be decrypted with the
/// provided key (wrong key or already-migrated data).  The database is left
/// unchanged for that table when an error occurs.
///
/// **Requires** the `encryption` feature flag.
#[cfg(feature = "encryption")]
pub fn migrate_encrypted_v0_to_v1(
    backend: &dyn crate::engine::StorageBackend,
    key: &EncryptionKey,
) -> Result<usize, TalaDbError> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};

    let rtxn = backend.begin_read()?;
    let tables = rtxn.list_tables()?;
    drop(rtxn);

    let mut total = 0usize;

    for table in &tables {
        // Collect all kv pairs from this table in a read-only transaction.
        let rtxn = backend.begin_read()?;
        let pairs = rtxn.scan_all(table)?;
        drop(rtxn);

        if pairs.is_empty() {
            continue;
        }

        let cipher = Aes256Gcm::new_from_slice(key.as_ref())
            .map_err(|e| TalaDbError::Encryption(e.to_string()))?;

        let mut updates: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

        for (raw_key, raw_val) in &pairs {
            // Values that *look* v1 (leading 0x01 + plausible length) must be
            // verified by an actual v1 decrypt: a v0 ciphertext starts with a
            // random nonce, so ~1/256 of them begin with 0x01 by chance, and
            // skipping those here would leave them permanently undecryptable
            // once the rest of the table is migrated.
            if raw_val.first() == Some(&CRYPTO_FORMAT_V1)
                && raw_val.len() >= 1 + NONCE_LEN_V1 + 16
                && decrypt(key, table, raw_key, raw_val).is_ok()
            {
                continue; // genuinely already migrated
            }

            // Old format: [12-byte nonce][ciphertext+tag] — no version prefix.
            if raw_val.len() < NONCE_LEN_V1 + 16 {
                return Err(TalaDbError::Encryption(format!(
                    "migrate v0→v1: value in table \"{table}\" is too short \
                     to be a valid v0 ciphertext ({} bytes)",
                    raw_val.len()
                )));
            }

            let nonce_bytes = &raw_val[..NONCE_LEN_V1];
            let ciphertext = &raw_val[NONCE_LEN_V1..];
            let nonce = Nonce::from_slice(nonce_bytes);

            // Old format used no AAD — pass empty slice.
            let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|e| {
                TalaDbError::Encryption(format!(
                    "migrate v0→v1: failed to decrypt value in table \"{table}\": {e}"
                ))
            })?;

            // Re-encrypt in v1 format with AAD.
            let new_val = encrypt(key, table, raw_key, &plaintext)?;
            updates.push((raw_key.clone(), new_val));
        }

        if updates.is_empty() {
            continue;
        }

        // Write all re-encrypted values for this table atomically.
        let mut wtxn = backend.begin_write()?;
        for (k, v) in &updates {
            wtxn.put(table, k, v)?;
        }
        wtxn.commit()?;

        total += updates.len();
    }

    Ok(total)
}

// ---------------------------------------------------------------------------
// Field-level encryption
// ---------------------------------------------------------------------------

/// Configuration for per-field encryption on a [`crate::collection::Collection`].
///
/// Stores the set of field names to encrypt and the encryption key.
/// Created via [`Collection::with_field_encryption`] and held inside the
/// `Collection` struct (behind the `encryption` feature flag).
#[cfg(feature = "encryption")]
#[derive(Clone)]
pub struct FieldEncryptionConfig {
    /// Sorted list of field names whose values should be encrypted.
    pub fields: Vec<String>,
    pub key: EncryptionKey,
}

/// Encrypt the nominated fields in `doc` in-place.
///
/// Each target field value is serialized to bytes with `postcard`, encrypted
/// with AES-GCM-256 using AAD `"field:<doc_id>:<field_name>"`, and stored back
/// as `Value::Bytes`.  Fields not in the list are left unchanged.
///
/// Binding the AAD to both the document ID and the field name means a
/// ciphertext cannot be transplanted to a different field ("ssn" → "token")
/// *or* to the same field of a different document (doc A's "ssn" → doc B's
/// "ssn") without failing authentication on read.
#[cfg(feature = "encryption")]
pub fn encrypt_fields(
    doc: &mut crate::document::Document,
    config: &FieldEncryptionConfig,
) -> Result<(), crate::error::TalaDbError> {
    let doc_id = doc.id;
    for (name, val) in &mut doc.fields {
        if config.fields.iter().any(|f| f == name) {
            let aad_key = field_aad(doc_id, name);
            let plain = postcard::to_allocvec(val)
                .map_err(|e| crate::error::TalaDbError::Serialization(e.to_string()))?;
            let ciphertext = encrypt(&config.key, "field", &aad_key, &plain)?;
            *val = crate::document::Value::Bytes(ciphertext);
        }
    }
    Ok(())
}

/// AAD for a field-level ciphertext: bound to the owning document and field.
#[cfg(feature = "encryption")]
fn field_aad(doc_id: ulid::Ulid, field: &str) -> Vec<u8> {
    format!("field:{doc_id}:{field}").into_bytes()
}

/// Legacy (pre-0.8.1) field AAD: bound to the field name only. Kept so data
/// encrypted by older builds remains readable; values are upgraded to the
/// doc-bound AAD the next time they are re-encrypted (any update).
#[cfg(feature = "encryption")]
fn legacy_field_aad(field: &str) -> Vec<u8> {
    format!("field:{field}").into_bytes()
}

/// Decrypt the nominated fields in `doc` in-place.
///
/// Reverses [`encrypt_fields`]: finds `Value::Bytes` entries for the listed
/// fields, decrypts them, and deserializes back to the original `Value`.
///
/// Tries the doc-bound AAD first, then falls back to the legacy field-only
/// AAD for values written by older builds. A transplanted ciphertext fails
/// both checks: under the doc-bound scheme the document IDs differ, and a
/// doc-bound ciphertext never authenticates under the legacy AAD.
#[cfg(feature = "encryption")]
pub fn decrypt_fields(
    doc: &mut crate::document::Document,
    config: &FieldEncryptionConfig,
) -> Result<(), crate::error::TalaDbError> {
    let doc_id = doc.id;
    for (name, val) in &mut doc.fields {
        if config.fields.iter().any(|f| f == name)
            && let crate::document::Value::Bytes(ciphertext) = val
        {
            let plain = match decrypt(&config.key, "field", &field_aad(doc_id, name), ciphertext) {
                Ok(p) => p,
                Err(_) => decrypt(&config.key, "field", &legacy_field_aad(name), ciphertext)?,
            };
            let original: crate::document::Value = postcard::from_bytes(&plain)
                .map_err(|e| crate::error::TalaDbError::Serialization(e.to_string()))?;
            *val = original;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Key rotation
// ---------------------------------------------------------------------------

/// Re-encrypt all values in `backend` under a new key in a single atomic
/// transaction per table.
///
/// `old_key` is used to decrypt existing values; `new_key` is used to
/// re-encrypt them.  On failure the database is left unchanged for the
/// affected table (each table is written in its own transaction).
///
/// Returns the total number of values re-encrypted.
///
/// # When to use
/// Call this function once after an `old_key` is suspected to be compromised.
/// The database must be opened with `EncryptedBackend` wrapping a **raw**
/// (unencrypted) backend so the function can operate on the raw ciphertext
/// bytes directly.  Pass the same raw backend that was passed to
/// `EncryptedBackend::new`.
///
/// **Requires** the `encryption` feature flag.
#[cfg(feature = "encryption")]
pub fn rekey(
    backend: &dyn crate::engine::StorageBackend,
    old_key: &EncryptionKey,
    new_key: &EncryptionKey,
) -> Result<usize, TalaDbError> {
    let rtxn = backend.begin_read()?;
    let tables = rtxn.list_tables()?;
    drop(rtxn);

    let mut total = 0usize;

    for table in &tables {
        let rtxn = backend.begin_read()?;
        let pairs = rtxn.scan_all(table)?;
        drop(rtxn);

        if pairs.is_empty() {
            continue;
        }

        let mut updates: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

        for (raw_key, raw_val) in &pairs {
            // Decrypt with old key.
            let plaintext = match decrypt(old_key, table, raw_key, raw_val) {
                Ok(p) => p,
                Err(e) => {
                    // Each table commits in its own transaction, so an
                    // interrupted earlier run leaves some tables already on
                    // the new key. Verify before failing so `rekey` can be
                    // re-run to completion instead of erroring on its own
                    // partial progress.
                    if decrypt(new_key, table, raw_key, raw_val).is_ok() {
                        continue; // already re-encrypted under the new key
                    }
                    return Err(e);
                }
            };
            // Re-encrypt with new key.
            let new_val = encrypt(new_key, table, raw_key, &plaintext)?;
            updates.push((raw_key.clone(), new_val));
        }

        // Write all re-encrypted values for this table atomically.
        let mut wtxn = backend.begin_write()?;
        for (k, v) in &updates {
            wtxn.put(table, k, v)?;
        }
        wtxn.commit()?;

        total += updates.len();
    }

    Ok(total)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "encryption"))]
mod tests {
    use super::*;

    fn test_key() -> EncryptionKey {
        zeroize::Zeroizing::new([0x42u8; 32])
    }

    #[test]
    fn round_trip() {
        let key = test_key();
        let plaintext = b"hello world";
        let ciphertext = encrypt(&key, "my_table", b"doc_id_1", plaintext).unwrap();
        let decrypted = decrypt(&key, "my_table", b"doc_id_1", &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let key1 = test_key();
        let key2 = zeroize::Zeroizing::new([0x99u8; 32]);
        let ct = encrypt(&key1, "t", b"k", b"secret").unwrap();
        assert!(decrypt(&key2, "t", b"k", &ct).is_err());
    }

    #[test]
    fn aad_table_mismatch_fails() {
        let key = test_key();
        let ct = encrypt(&key, "table_a", b"key1", b"data").unwrap();
        // Moving ciphertext to a different table must fail authentication.
        assert!(decrypt(&key, "table_b", b"key1", &ct).is_err());
    }

    #[test]
    fn aad_key_mismatch_fails() {
        let key = test_key();
        let ct = encrypt(&key, "table_a", b"key1", b"data").unwrap();
        // Same table, different storage key — must fail authentication.
        assert!(decrypt(&key, "table_a", b"key2", &ct).is_err());
    }

    #[test]
    fn unknown_version_fails() {
        let key = test_key();
        let mut ct = encrypt(&key, "t", b"k", b"data").unwrap();
        ct[0] = 0xFF; // corrupt version byte
        let err = decrypt(&key, "t", b"k", &ct).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported encrypted-value format version")
        );
    }

    #[test]
    fn truncated_ciphertext_fails() {
        let key = test_key();
        assert!(decrypt(&key, "t", b"k", &[]).is_err());
        assert!(decrypt(&key, "t", b"k", &[CRYPTO_FORMAT_V1; 10]).is_err());
    }

    #[test]
    fn derive_key_enforces_min_iterations() {
        let err = derive_key("pass", b"salt1234567890ab", 999).unwrap_err();
        assert!(err.to_string().contains("iterations too low"));
    }

    #[test]
    fn derive_key_at_min_iterations_succeeds() {
        let key = derive_key("pass", b"salt1234567890ab", MIN_PBKDF2_ITERATIONS).unwrap();
        assert_eq!(key.len(), 32);
    }

    #[test]
    fn field_encryption_round_trip_and_doc_binding() {
        use crate::document::{Document, Value};

        let config = FieldEncryptionConfig {
            fields: vec!["ssn".into()],
            key: test_key(),
        };

        let mut doc_a = Document::new(vec![("ssn".into(), Value::Str("111-22-3333".into()))]);
        let mut doc_b = Document::new(vec![("ssn".into(), Value::Str("999-88-7777".into()))]);
        encrypt_fields(&mut doc_a, &config).unwrap();
        encrypt_fields(&mut doc_b, &config).unwrap();

        // Transplant doc A's ciphertext into doc B: decryption must fail.
        let stolen = doc_a.get("ssn").unwrap().clone();
        doc_b.set("ssn", stolen);
        assert!(
            decrypt_fields(&mut doc_b, &config).is_err(),
            "ciphertext moved between documents must fail authentication"
        );

        // The untouched document round-trips.
        decrypt_fields(&mut doc_a, &config).unwrap();
        assert_eq!(doc_a.get("ssn"), Some(&Value::Str("111-22-3333".into())));
    }

    #[test]
    fn field_encryption_decrypts_legacy_field_only_aad() {
        use crate::document::{Document, Value};

        let config = FieldEncryptionConfig {
            fields: vec!["token".into()],
            key: test_key(),
        };

        // Simulate a value written by a pre-doc-binding build: AAD is the
        // legacy "field:<name>" form with no document ID.
        let mut doc = Document::new(vec![("token".into(), Value::Str("secret".into()))]);
        let plain = postcard::to_allocvec(doc.get("token").unwrap()).unwrap();
        let legacy_ct = encrypt(&config.key, "field", b"field:token", &plain).unwrap();
        doc.set("token", Value::Bytes(legacy_ct));

        decrypt_fields(&mut doc, &config).unwrap();
        assert_eq!(doc.get("token"), Some(&Value::Str("secret".into())));
    }

    #[test]
    fn rekey_round_trip() {
        use crate::engine::{RedbBackend, StorageBackend};

        let old_key: EncryptionKey = zeroize::Zeroizing::new([0x11u8; 32]);
        let new_key: EncryptionKey = zeroize::Zeroizing::new([0x22u8; 32]);

        // Open a raw in-memory backend and write some encrypted values.
        let raw = std::sync::Arc::new(RedbBackend::open_in_memory().unwrap());
        let enc = EncryptedBackend::new(raw.clone(), old_key.clone());

        let mut wtxn = enc.begin_write().unwrap();
        wtxn.put("docs::test", b"k1", b"hello").unwrap();
        wtxn.put("docs::test", b"k2", b"world").unwrap();
        wtxn.commit().unwrap();

        // Rotate the key on the raw backend.
        let count = super::rekey(raw.as_ref(), &old_key, &new_key).unwrap();
        assert_eq!(count, 2);

        // Old key must no longer decrypt.
        let enc_old = EncryptedBackend::new(raw.clone(), old_key);
        let rtxn_old = enc_old.begin_read().unwrap();
        assert!(rtxn_old.get("docs::test", b"k1").is_err());

        // New key must decrypt correctly.
        let enc_new = EncryptedBackend::new(raw, new_key);
        let rtxn_new = enc_new.begin_read().unwrap();
        assert_eq!(
            rtxn_new.get("docs::test", b"k1").unwrap().unwrap(),
            b"hello"
        );
        assert_eq!(
            rtxn_new.get("docs::test", b"k2").unwrap().unwrap(),
            b"world"
        );
    }

    #[test]
    fn rekey_wrong_old_key_fails() {
        use crate::engine::{RedbBackend, StorageBackend};

        let old_key: EncryptionKey = zeroize::Zeroizing::new([0x11u8; 32]);
        let wrong_key: EncryptionKey = zeroize::Zeroizing::new([0xFFu8; 32]);
        let new_key: EncryptionKey = zeroize::Zeroizing::new([0x22u8; 32]);

        let raw = std::sync::Arc::new(RedbBackend::open_in_memory().unwrap());
        let enc = EncryptedBackend::new(raw.clone(), old_key);
        let mut wtxn = enc.begin_write().unwrap();
        wtxn.put("t", b"k", b"v").unwrap();
        wtxn.commit().unwrap();

        // Passing the wrong old key must return an encryption error.
        assert!(super::rekey(raw.as_ref(), &wrong_key, &new_key).is_err());
    }
}
