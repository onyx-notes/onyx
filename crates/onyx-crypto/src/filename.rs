//! Deterministic filename encryption.
//!
//! Zero-knowledge is hollow if note titles leak, so encrypted vaults also
//! encrypt file and directory names. The scheme must be *deterministic*:
//! the file watcher sees ciphertext names on disk and must map the same
//! name to the same note identity every time.
//!
//! SIV-style construction:
//!
//! ```text
//! mac_key, enc_key = derive(vault_key)
//! siv   = keyed-BLAKE3(mac_key, name)[..16]            // synthetic IV
//! ct    = XChaCha20(enc_key, nonce = siv ‖ 0⁸) ⊕ name
//! token = base32(siv ‖ ct)
//! ```
//!
//! Decryption recomputes the SIV from the decrypted name and verifies it
//! against the stored one, authenticating the name (128-bit tag).
//! Determinism leaks only name-equality within one vault — the accepted
//! trade-off that keeps rename/watch semantics sane.

use chacha20::XChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher};
use data_encoding::BASE32_NOPAD;
use zeroize::Zeroize;

use crate::CryptoError;
use crate::keys::VaultKey;

const MAC_CONTEXT: &str = "onyx-crypto 2026-07 filename mac key v1";
const ENC_CONTEXT: &str = "onyx-crypto 2026-07 filename enc key v1";
const SIV_LEN: usize = 24;
const SIV_STORED: usize = 16;

/// Encrypt one path component to a filesystem-safe token.
///
/// Tokens are uppercase base32 without padding: safe on case-insensitive
/// filesystems and free of path separators.
pub fn encrypt_name(vault_key: &VaultKey, name: &str) -> String {
    let siv = synthetic_iv(vault_key, name);

    let mut data = name.as_bytes().to_vec();
    apply_stream(vault_key, &siv, &mut data);

    let mut token_bytes = Vec::with_capacity(SIV_STORED + data.len());
    token_bytes.extend_from_slice(&siv[..SIV_STORED]);
    token_bytes.extend_from_slice(&data);
    BASE32_NOPAD.encode(&token_bytes)
}

/// Decrypt and authenticate a token produced by [`encrypt_name`].
pub fn decrypt_name(vault_key: &VaultKey, token: &str) -> Result<String, CryptoError> {
    let token_bytes = BASE32_NOPAD
        .decode(token.as_bytes())
        .map_err(|_| CryptoError::InvalidFormat("filename"))?;
    if token_bytes.len() < SIV_STORED {
        return Err(CryptoError::InvalidFormat("filename"));
    }
    let (stored_siv, ciphertext) = token_bytes.split_at(SIV_STORED);

    // The stream nonce is the 16-byte SIV zero-extended to XChaCha's 24.
    let mut nonce = [0u8; SIV_LEN];
    nonce[..SIV_STORED].copy_from_slice(stored_siv);

    let mut plaintext = ciphertext.to_vec();
    apply_stream(vault_key, &nonce, &mut plaintext);

    let name = String::from_utf8(plaintext).map_err(|_| CryptoError::AuthenticationFailed)?;

    // Authenticate: the SIV recomputed from the plaintext must match the
    // stored one (this is the MAC verification of the SIV construction).
    let expected = synthetic_iv(vault_key, &name);
    if expected[..SIV_STORED] != *stored_siv {
        return Err(CryptoError::AuthenticationFailed);
    }
    Ok(name)
}

/// The synthetic IV: 16 MAC bytes zero-extended to XChaCha's 24-byte nonce.
fn synthetic_iv(vault_key: &VaultKey, name: &str) -> [u8; SIV_LEN] {
    let mut mac_key = vault_key.derive(MAC_CONTEXT, &[]);
    let mac = blake3::keyed_hash(&mac_key, name.as_bytes());
    mac_key.zeroize();
    let mut siv = [0u8; SIV_LEN];
    siv[..SIV_STORED].copy_from_slice(&mac.as_bytes()[..SIV_STORED]);
    siv
}

fn apply_stream(vault_key: &VaultKey, nonce: &[u8; SIV_LEN], data: &mut [u8]) {
    let mut enc_key = vault_key.derive(ENC_CONTEXT, &[]);
    let mut cipher = XChaCha20::new((&enc_key).into(), nonce.into());
    cipher.apply_keystream(data);
    enc_key.zeroize();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> VaultKey {
        VaultKey::from_bytes([0x11; 32])
    }

    #[test]
    fn roundtrip() {
        for name in [
            "note.md",
            "Meeting Notes 2026-07-16.md",
            "日本語ノート.md",
            "a",
            "with – dash — and emoji 🚀.md",
        ] {
            let token = encrypt_name(&key(), name);
            assert_eq!(decrypt_name(&key(), &token).unwrap(), name);
        }
    }

    #[test]
    fn deterministic_same_name_same_token() {
        assert_eq!(encrypt_name(&key(), "x.md"), encrypt_name(&key(), "x.md"));
    }

    #[test]
    fn different_names_different_tokens() {
        assert_ne!(encrypt_name(&key(), "a.md"), encrypt_name(&key(), "b.md"));
    }

    #[test]
    fn tokens_are_filesystem_safe() {
        let token = encrypt_name(&key(), "weird / name ⧸ with sep.md");
        assert!(token.bytes().all(|byte| byte.is_ascii_alphanumeric()));
    }

    #[test]
    fn wrong_key_fails() {
        let token = encrypt_name(&key(), "secret.md");
        let other = VaultKey::from_bytes([0x22; 32]);
        assert!(decrypt_name(&other, &token).is_err());
    }

    #[test]
    fn tampered_token_fails() {
        let token = encrypt_name(&key(), "secret.md");
        let mut chars: Vec<char> = token.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert!(decrypt_name(&key(), &tampered).is_err());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(decrypt_name(&key(), "not base32 !!!").is_err());
        assert!(decrypt_name(&key(), "").is_err());
        assert!(decrypt_name(&key(), "AAAA").is_err());
    }
}
