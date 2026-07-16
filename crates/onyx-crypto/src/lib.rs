//! Cryptography for Onyx.
//!
//! One deliberately boring toolbox shared by encryption-at-rest, E2EE sync
//! blobs, and backups:
//!
//! - **KDF**: argon2id (passphrase → key-encryption key).
//! - **AEAD**: XChaCha20-Poly1305 everywhere.
//! - **Hash/derive**: BLAKE3 (content hashes, subkey derivation).
//!
//! Key hierarchy (local at-rest slice of it):
//!
//! ```text
//! passphrase ──argon2id──▶ KEK ──unwraps──▶ vault key (random 256-bit)
//!                                             ├─ per-file keys (container)
//!                                             └─ filename keys (filename)
//! ```
//!
//! The vault key is random, so a passphrase change only re-wraps one small
//! keyfile — never the vault.
//!
//! # Security notes
//!
//! - Every ciphertext this crate produces is authenticated; truncation,
//!   reordering, and cross-file chunk splicing are all detected.
//! - Key material is zeroized on drop.
//! - Deterministic filename encryption intentionally leaks name equality
//!   within a vault (required for stable watcher identity); names are
//!   encrypted per-component so directory shape is visible. Documented
//!   trade-off for "folder = vault" ergonomics.

mod container;
mod filename;
mod kdf;
mod keys;

pub use container::{CHUNK_SIZE_DEFAULT, decrypt, encrypt, encrypt_with};
pub use filename::{decrypt_name, encrypt_name};
pub use kdf::KdfParams;
pub use keys::{Keyfile, VaultKey};

/// Errors from cryptographic operations.
///
/// Deliberately coarse: distinguishing "wrong key" from "tampered data" to a
/// caller is exactly the oracle an attacker wants, and users can't act on
/// the difference anyway.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// Data is not a valid Onyx cryptographic artifact (bad magic, version,
    /// or structure).
    #[error("not a valid encrypted {0} (unrecognized or corrupt format)")]
    InvalidFormat(&'static str),
    /// Authentication failed: wrong key or tampered/corrupted data.
    #[error("decryption failed: wrong key or corrupted data")]
    AuthenticationFailed,
    /// Key derivation failed (invalid parameters).
    #[error("key derivation failed: {0}")]
    Kdf(String),
}

pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("OS randomness is required and must be available");
    bytes
}
