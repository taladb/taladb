//! Frozen cryptographic fixtures — the on-disk contract for encrypted databases.
//!
//! Requires `--features encryption`.
//!
//! These vectors were produced by `aes-gcm 0.10` / `pbkdf2 0.12` and must keep
//! decoding byte-for-byte across every future dependency bump. An encrypted
//! TalaDB file in the field is only readable if `derive_key` maps the same
//! passphrase and salt to the same key, and `decrypt` accepts a ciphertext this
//! codebase wrote at an earlier version.
//!
//! If a RustCrypto upgrade breaks one of these, the upgrade is not a dependency
//! bump — it is a storage format migration, and it needs a version bump in
//! `CRYPTO_FORMAT_V1` plus a re-encryption path like
//! `migrate_encrypted_v0_to_v1`. Do not "fix" these numbers to match new output.
#![cfg(feature = "encryption")]

use taladb_core::crypto::{MIN_PBKDF2_ITERATIONS, decrypt, derive_key, encrypt};

const PASSPHRASE: &str = "correct horse battery staple";
const SALT: &[u8; 16] = b"0123456789abcdef";
const TABLE: &str = "docs::people";
const DOC_KEY: &[u8] = &[1, 2, 3, 4];
const PLAINTEXT: &[u8] = b"the quick brown fox";

/// PBKDF2-HMAC-SHA256, 600_000 iterations, over PASSPHRASE + SALT.
const EXPECTED_KEY: [u8; 32] = [
    108, 74, 100, 106, 173, 16, 208, 103, 173, 213, 251, 121, 217, 7, 138, 22, 218, 131, 213, 15,
    129, 103, 10, 142, 117, 147, 178, 73, 230, 217, 73, 54,
];

/// `[0x01][12-byte nonce][ciphertext][16-byte GCM tag]`, AAD-bound to
/// `(TABLE, DOC_KEY)`. Written by aes-gcm 0.10.
const FROZEN_CIPHERTEXT: [u8; 48] = [
    1, 222, 13, 4, 207, 86, 125, 94, 6, 253, 173, 226, 181, 112, 198, 30, 228, 28, 197, 69, 196,
    190, 76, 107, 169, 101, 236, 90, 134, 232, 239, 51, 142, 203, 163, 69, 69, 221, 76, 243, 45,
    140, 138, 139, 63, 7, 124, 166,
];

/// If this breaks, every existing encrypted database becomes unopenable: the
/// passphrase no longer derives the key its data was encrypted under.
#[test]
fn derive_key_is_stable_across_dependency_versions() {
    let key = derive_key(PASSPHRASE, SALT, MIN_PBKDF2_ITERATIONS).expect("derive_key");
    assert_eq!(
        &key[..],
        &EXPECTED_KEY[..],
        "PBKDF2 output moved — existing encrypted databases would no longer open"
    );
}

/// If this breaks, data written by an older build cannot be read by a newer one.
#[test]
fn a_ciphertext_written_by_aes_gcm_0_10_still_decrypts() {
    let key = derive_key(PASSPHRASE, SALT, MIN_PBKDF2_ITERATIONS).expect("derive_key");
    let plain = decrypt(&key, TABLE, DOC_KEY, &FROZEN_CIPHERTEXT)
        .expect("a ciphertext from an earlier release must still decrypt");
    assert_eq!(plain, PLAINTEXT);
}

/// The AAD binding must keep rejecting a relocated ciphertext. A dependency bump
/// that silently stopped passing AAD would leave this the only thing to notice.
#[test]
fn the_frozen_ciphertext_is_still_bound_to_its_table_and_key() {
    let key = derive_key(PASSPHRASE, SALT, MIN_PBKDF2_ITERATIONS).expect("derive_key");
    assert!(
        decrypt(&key, "docs::other", DOC_KEY, &FROZEN_CIPHERTEXT).is_err(),
        "a ciphertext must not decrypt under a different table"
    );
    assert!(
        decrypt(&key, TABLE, &[9, 9, 9, 9], &FROZEN_CIPHERTEXT).is_err(),
        "a ciphertext must not decrypt under a different document key"
    );
}

/// Freshly written data must round-trip, and must interoperate with the frozen
/// vector's key — proving encrypt and decrypt did not both shift together.
#[test]
fn new_ciphertext_round_trips_under_the_frozen_key() {
    let key = derive_key(PASSPHRASE, SALT, MIN_PBKDF2_ITERATIONS).expect("derive_key");
    let ct = encrypt(&key, TABLE, DOC_KEY, PLAINTEXT).expect("encrypt");

    // Format envelope: version byte, 12-byte nonce, then ciphertext + tag.
    assert_eq!(ct[0], 1, "format version byte must stay 0x01");
    assert_eq!(
        ct.len(),
        1 + 12 + PLAINTEXT.len() + 16,
        "envelope layout changed"
    );
    // The nonce is random, so a fresh ciphertext differs from the frozen one —
    // that is the point of a nonce, and worth asserting so a stuck RNG shows up.
    assert_ne!(
        ct.as_slice(),
        &FROZEN_CIPHERTEXT[..],
        "a fresh encryption reused the frozen nonce — the RNG is not random"
    );
    assert_eq!(
        decrypt(&key, TABLE, DOC_KEY, &ct).expect("decrypt"),
        PLAINTEXT
    );
}
