//! Vault key and the keyfile that wraps it under a passphrase.

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::kdf::{KdfParams, derive_kek};
use crate::{CryptoError, random_bytes};

/// A vault's random 256-bit master key. All file, filename, and (later)
/// sync keys are derived from it.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct VaultKey([u8; 32]);

impl VaultKey {
    /// Generate a fresh random vault key.
    pub fn generate() -> Self {
        Self(random_bytes())
    }

    /// Reconstruct from raw bytes (e.g. HPKE-unwrapped during device sync).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Export the raw key. Only for handing the key to another sealed
    /// store (enrollment payloads, the mobile biometric keystore) — the
    /// caller owns zeroizing the copy.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// Derive a 32-byte subkey for a labeled purpose. Uses BLAKE3's
    /// derive-key mode: `context` must be a hardcoded, globally unique
    /// string per RFC-style domain separation.
    pub fn derive(&self, context: &str, material: &[u8]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new_derive_key(context);
        hasher.update(&self.0);
        hasher.update(material);
        *hasher.finalize().as_bytes()
    }
}

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material, not even in debug logs.
        formatter.write_str("VaultKey(..)")
    }
}

const KEYFILE_MAGIC: &[u8; 8] = b"ONYXKEY\x01";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const WRAPPED_LEN: usize = 32 + 16; // key + Poly1305 tag
const KEYFILE_LEN: usize = 8 + 12 + SALT_LEN + NONCE_LEN + WRAPPED_LEN;

/// The serialized keyfile: the vault key wrapped under a passphrase-derived
/// KEK, together with the KDF parameters and salt needed to re-derive it.
///
/// Layout: `magic(8) ‖ m_cost(4) ‖ t_cost(4) ‖ p_cost(4) ‖ salt(16) ‖
/// nonce(24) ‖ wrapped_key(48)` — all integers little-endian.
pub struct Keyfile;

impl Keyfile {
    /// Wrap `vault_key` under `passphrase`. Changing the passphrase means
    /// calling this again with the same vault key — nothing else re-encrypts.
    pub fn seal(
        vault_key: &VaultKey,
        passphrase: &str,
        params: KdfParams,
    ) -> Result<Vec<u8>, CryptoError> {
        let salt: [u8; SALT_LEN] = random_bytes();
        let nonce: [u8; NONCE_LEN] = random_bytes();
        let mut kek = derive_kek(passphrase, &salt, params)?;

        let cipher = XChaCha20Poly1305::new((&kek).into());
        let wrapped = cipher
            .encrypt(XNonce::from_slice(&nonce), vault_key.0.as_slice())
            .expect("in-memory AEAD encryption cannot fail");
        kek.zeroize();

        let mut keyfile = Vec::with_capacity(KEYFILE_LEN);
        keyfile.extend_from_slice(KEYFILE_MAGIC);
        keyfile.extend_from_slice(&params.m_cost_kib.to_le_bytes());
        keyfile.extend_from_slice(&params.t_cost.to_le_bytes());
        keyfile.extend_from_slice(&params.p_cost.to_le_bytes());
        keyfile.extend_from_slice(&salt);
        keyfile.extend_from_slice(&nonce);
        keyfile.extend_from_slice(&wrapped);
        Ok(keyfile)
    }

    /// Unwrap the vault key from a keyfile using `passphrase`.
    pub fn open(keyfile: &[u8], passphrase: &str) -> Result<VaultKey, CryptoError> {
        if keyfile.len() != KEYFILE_LEN || &keyfile[..8] != KEYFILE_MAGIC {
            return Err(CryptoError::InvalidFormat("keyfile"));
        }

        let read_u32 =
            |at: usize| u32::from_le_bytes(keyfile[at..at + 4].try_into().expect("length checked"));
        let params = KdfParams {
            m_cost_kib: read_u32(8),
            t_cost: read_u32(12),
            p_cost: read_u32(16),
        };
        let salt = &keyfile[20..20 + SALT_LEN];
        let nonce = &keyfile[36..36 + NONCE_LEN];
        let wrapped = &keyfile[60..];

        let mut kek = derive_kek(passphrase, salt, params)?;
        let cipher = XChaCha20Poly1305::new((&kek).into());
        let unwrapped = cipher
            .decrypt(XNonce::from_slice(nonce), wrapped)
            .map_err(|_| CryptoError::AuthenticationFailed);
        kek.zeroize();

        let mut key_bytes: [u8; 32] = unwrapped?
            .try_into()
            .map_err(|_| CryptoError::InvalidFormat("keyfile"))?;
        let vault_key = VaultKey(key_bytes);
        key_bytes.zeroize();
        Ok(vault_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let key = VaultKey::generate();
        let keyfile = Keyfile::seal(&key, "correct horse", KdfParams::INSECURE_TEST).unwrap();
        let opened = Keyfile::open(&keyfile, "correct horse").unwrap();
        assert_eq!(key.0, opened.0);
    }

    #[test]
    fn wrong_passphrase_fails() {
        let key = VaultKey::generate();
        let keyfile = Keyfile::seal(&key, "right", KdfParams::INSECURE_TEST).unwrap();
        assert!(matches!(
            Keyfile::open(&keyfile, "wrong"),
            Err(CryptoError::AuthenticationFailed)
        ));
    }

    #[test]
    fn tampered_keyfile_fails() {
        let key = VaultKey::generate();
        let mut keyfile = Keyfile::seal(&key, "pw", KdfParams::INSECURE_TEST).unwrap();
        let last = keyfile.len() - 1;
        keyfile[last] ^= 0x01;
        assert!(Keyfile::open(&keyfile, "pw").is_err());
    }

    #[test]
    fn truncated_or_garbage_is_invalid_format() {
        assert!(matches!(
            Keyfile::open(b"garbage", "pw"),
            Err(CryptoError::InvalidFormat(_))
        ));
        let key = VaultKey::generate();
        let keyfile = Keyfile::seal(&key, "pw", KdfParams::INSECURE_TEST).unwrap();
        assert!(matches!(
            Keyfile::open(&keyfile[..keyfile.len() - 1], "pw"),
            Err(CryptoError::InvalidFormat(_))
        ));
    }

    #[test]
    fn passphrase_change_reuses_vault_key() {
        let key = VaultKey::generate();
        let old = Keyfile::seal(&key, "old", KdfParams::INSECURE_TEST).unwrap();
        let new = Keyfile::seal(&key, "new", KdfParams::INSECURE_TEST).unwrap();
        assert_eq!(
            Keyfile::open(&old, "old").unwrap().0,
            Keyfile::open(&new, "new").unwrap().0,
        );
    }

    #[test]
    fn debug_never_leaks_key_material() {
        let key = VaultKey::from_bytes([0xAB; 32]);
        assert_eq!(format!("{key:?}"), "VaultKey(..)");
    }
}
