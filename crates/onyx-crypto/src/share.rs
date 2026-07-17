//! Single-note share sealing.
//!
//! Shares use **AES-256-GCM** (not our XChaCha container) for exactly one
//! reason: the recipient decrypts in a browser via WebCrypto, which
//! supports AES-GCM natively and XChaCha not at all. A fresh random key is
//! generated per share and travels only in the URL fragment — the server
//! stores the ciphertext and hosts a viewer, but never receives the key,
//! so a share is end-to-end encrypted just like everything else.
//!
//! Blob layout: `nonce(12) ‖ ciphertext ‖ tag(16)` — the shape the viewer's
//! WebCrypto call expects.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};

use crate::{CryptoError, random_bytes};

/// Seal `plaintext` for sharing. Returns `(key, blob)`; the key goes in
/// the link fragment, the blob to the server.
pub fn share_seal(plaintext: &[u8]) -> ([u8; 32], Vec<u8>) {
    let key: [u8; 32] = random_bytes();
    let nonce_bytes: [u8; 12] = random_bytes();
    let cipher = Aes256Gcm::new((&key).into());
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .expect("in-memory AEAD encryption cannot fail");

    let mut blob = Vec::with_capacity(12 + ciphertext.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    (key, blob)
}

/// Open a share blob (used by tests and any native viewer).
pub fn share_open(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if blob.len() < 12 + 16 {
        return Err(CryptoError::InvalidFormat("share"));
    }
    let (nonce_bytes, ciphertext) = blob.split_at(12);
    let cipher = Aes256Gcm::new(key.into());
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| CryptoError::AuthenticationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let (key, blob) = share_seal(b"<h1>Shared note</h1>");
        assert_eq!(share_open(&key, &blob).unwrap(), b"<h1>Shared note</h1>");
    }

    #[test]
    fn each_share_uses_a_fresh_key_and_nonce() {
        let (key_a, blob_a) = share_seal(b"same");
        let (key_b, blob_b) = share_seal(b"same");
        assert_ne!(key_a, key_b);
        assert_ne!(blob_a, blob_b);
    }

    #[test]
    fn wrong_key_and_tamper_fail() {
        let (_key, blob) = share_seal(b"secret");
        assert!(share_open(&[9; 32], &blob).is_err());
        let mut tampered = blob.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        // The real key still can't open tampered ciphertext.
        let (key, good) = share_seal(b"secret");
        let mut bad = good.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(share_open(&key, &bad).is_err());
        let _ = tampered;
    }

    #[test]
    fn garbage_is_invalid_format() {
        assert!(matches!(
            share_open(&[0; 32], b"short"),
            Err(CryptoError::InvalidFormat(_))
        ));
    }
}
