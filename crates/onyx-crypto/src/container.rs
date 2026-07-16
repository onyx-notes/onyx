//! The `.onyxenc` chunked AEAD container.
//!
//! One format for every encrypted byte Onyx stores: at-rest vault files,
//! E2EE sync blobs, and backup chunks.
//!
//! Layout:
//!
//! ```text
//! magic(8) ‖ chunk_size u32 LE ‖ file_id(16) ‖ chunk₀ ‖ chunk₁ ‖ … ‖ chunkₙ
//! ```
//!
//! Each chunk is `XChaCha20-Poly1305(plaintext[i*cs..])` — ciphertext plus a
//! 16-byte tag. The per-file key is derived from the vault key and the random
//! `file_id`, so no two files ever share a keystream. The 24-byte nonce
//! encodes `chunk_index (8 LE) ‖ is_last (1) ‖ zeros`, and the header is
//! bound as AAD, which makes reordering, truncation (even at a chunk
//! boundary), extension, and cross-file chunk splicing all fail
//! authentication. The final chunk — and only the final chunk — carries the
//! `is_last` flag; an empty file is a single empty last chunk.

use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use zeroize::Zeroize;

use crate::keys::VaultKey;
use crate::{CryptoError, random_bytes};

const MAGIC: &[u8; 8] = b"ONYXENC\x01";
const HEADER_LEN: usize = 8 + 4 + 16;
const TAG_LEN: usize = 16;
const FILE_KEY_CONTEXT: &str = "onyx-crypto 2026-07 per-file container key v1";

/// Default chunk size: 256 KiB — streaming and random access for large
/// files, ≤0.007% tag overhead.
pub const CHUNK_SIZE_DEFAULT: u32 = 256 * 1024;

/// Encrypt `plaintext` into a container with the default chunk size and a
/// fresh random file id.
pub fn encrypt(vault_key: &VaultKey, plaintext: &[u8]) -> Vec<u8> {
    encrypt_with(vault_key, plaintext, CHUNK_SIZE_DEFAULT)
}

/// Encrypt with an explicit chunk size (exposed for tests and for callers
/// with unusual size/latency trade-offs).
pub fn encrypt_with(vault_key: &VaultKey, plaintext: &[u8], chunk_size: u32) -> Vec<u8> {
    assert!(chunk_size > 0, "chunk size must be positive");
    let file_id: [u8; 16] = random_bytes();

    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&chunk_size.to_le_bytes());
    header.extend_from_slice(&file_id);

    let cipher = file_cipher(vault_key, &file_id);

    let chunk_len = chunk_size as usize;
    let chunk_count = plaintext.len().div_ceil(chunk_len).max(1);
    let mut container = Vec::with_capacity(HEADER_LEN + plaintext.len() + chunk_count * TAG_LEN);
    container.extend_from_slice(&header);

    // chunks() yields nothing for an empty input, but an empty file must
    // still produce one authenticated (empty) last chunk.
    let mut chunks = plaintext.chunks(chunk_len);
    for index in 0..chunk_count {
        let chunk = chunks.next().unwrap_or(&[]);
        let is_last = index == chunk_count - 1;
        let sealed = cipher
            .encrypt(
                &nonce_for(index as u64, is_last),
                Payload {
                    msg: chunk,
                    aad: &header,
                },
            )
            .expect("in-memory AEAD encryption cannot fail");
        container.extend_from_slice(&sealed);
    }
    container
}

/// Decrypt a container produced by [`encrypt`] / [`encrypt_with`].
pub fn decrypt(vault_key: &VaultKey, container: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if container.len() < HEADER_LEN || &container[..8] != MAGIC {
        return Err(CryptoError::InvalidFormat("container"));
    }
    let header = &container[..HEADER_LEN];
    let chunk_size = u32::from_le_bytes(header[8..12].try_into().expect("length checked")) as usize;
    if chunk_size == 0 {
        return Err(CryptoError::InvalidFormat("container"));
    }
    let file_id = &header[12..HEADER_LEN];

    let cipher = file_cipher(vault_key, file_id.try_into().expect("length checked"));

    let mut body = &container[HEADER_LEN..];
    // Total length is known, so chunk framing is unambiguous: every chunk
    // except the final one is exactly chunk_size + tag bytes.
    let full_chunk = chunk_size + TAG_LEN;
    let mut plaintext = Vec::with_capacity(body.len());
    let mut index: u64 = 0;

    loop {
        let is_last = body.len() <= full_chunk;
        let take = if is_last { body.len() } else { full_chunk };
        if take < TAG_LEN {
            // No room for a tag: truncated mid-chunk (or an empty body,
            // which is invalid because even empty files have one chunk).
            return Err(CryptoError::InvalidFormat("container"));
        }
        let opened = cipher
            .decrypt(
                &nonce_for(index, is_last),
                Payload {
                    msg: &body[..take],
                    aad: header,
                },
            )
            .map_err(|_| CryptoError::AuthenticationFailed)?;
        plaintext.extend_from_slice(&opened);

        if is_last {
            return Ok(plaintext);
        }
        body = &body[take..];
        index += 1;
    }
}

fn file_cipher(vault_key: &VaultKey, file_id: &[u8; 16]) -> XChaCha20Poly1305 {
    let mut file_key = vault_key.derive(FILE_KEY_CONTEXT, file_id);
    let cipher = XChaCha20Poly1305::new((&file_key).into());
    file_key.zeroize();
    cipher
}

fn nonce_for(index: u64, is_last: bool) -> XNonce {
    let mut nonce = [0u8; 24];
    nonce[..8].copy_from_slice(&index.to_le_bytes());
    nonce[8] = is_last as u8;
    XNonce::from(nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> VaultKey {
        VaultKey::from_bytes([0x42; 32])
    }

    #[test]
    fn roundtrip_various_sizes_across_chunk_boundaries() {
        const CHUNK: u32 = 64;
        for size in [
            0usize,
            1,
            63,
            64,
            65,
            127,
            128,
            129,
            64 * 3,
            64 * 3 + 1,
            10_000,
        ] {
            let plaintext: Vec<u8> = (0..size).map(|byte| (byte % 251) as u8).collect();
            let sealed = encrypt_with(&key(), &plaintext, CHUNK);
            let opened = decrypt(&key(), &sealed).expect("roundtrip");
            assert_eq!(opened, plaintext, "size {size}");
        }
    }

    #[test]
    fn empty_file_is_authenticated() {
        let sealed = encrypt_with(&key(), b"", 64);
        assert_eq!(decrypt(&key(), &sealed).unwrap(), b"");
        // Header alone (missing the empty last chunk's tag) must fail.
        assert!(decrypt(&key(), &sealed[..HEADER_LEN]).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let sealed = encrypt_with(&key(), b"secret", 64);
        let other = VaultKey::from_bytes([0x43; 32]);
        assert!(matches!(
            decrypt(&other, &sealed),
            Err(CryptoError::AuthenticationFailed)
        ));
    }

    #[test]
    fn any_flipped_bit_fails() {
        let sealed = encrypt_with(&key(), b"attack at dawn", 8);
        for position in 0..sealed.len() {
            let mut tampered = sealed.clone();
            tampered[position] ^= 0x01;
            assert!(
                decrypt(&key(), &tampered).is_err(),
                "flip at byte {position} was not detected"
            );
        }
    }

    #[test]
    fn truncation_at_chunk_boundary_fails() {
        // 3 chunks of 8 bytes; cutting cleanly after chunk 2 must fail
        // because chunk 2 was not sealed with the last-chunk flag.
        let sealed = encrypt_with(&key(), &[7u8; 24], 8);
        let two_chunks = HEADER_LEN + 2 * (8 + TAG_LEN);
        assert!(matches!(
            decrypt(&key(), &sealed[..two_chunks]),
            Err(CryptoError::AuthenticationFailed)
        ));
    }

    #[test]
    fn extension_fails() {
        let mut sealed = encrypt_with(&key(), &[7u8; 24], 8);
        sealed.extend_from_slice(&[0u8; 24]);
        assert!(decrypt(&key(), &sealed).is_err());
    }

    #[test]
    fn chunk_reordering_fails() {
        let sealed = encrypt_with(&key(), &[7u8; 24], 8);
        let chunk = 8 + TAG_LEN;
        let mut swapped = sealed.clone();
        swapped.copy_within(HEADER_LEN..HEADER_LEN + chunk, HEADER_LEN + chunk);
        swapped[HEADER_LEN..HEADER_LEN + chunk]
            .copy_from_slice(&sealed[HEADER_LEN + chunk..HEADER_LEN + 2 * chunk]);
        assert!(decrypt(&key(), &swapped).is_err());
    }

    #[test]
    fn chunks_cannot_be_spliced_across_files() {
        // Same key, same plaintext: different random file_id ⇒ different
        // ciphertext, and chunks from one file fail in the other.
        let first = encrypt_with(&key(), &[9u8; 16], 8);
        let second = encrypt_with(&key(), &[9u8; 16], 8);
        assert_ne!(first, second);

        let mut franken = first.clone();
        franken[HEADER_LEN..].copy_from_slice(&second[HEADER_LEN..]);
        assert!(decrypt(&key(), &franken).is_err());
    }

    #[test]
    fn garbage_is_invalid_format() {
        assert!(matches!(
            decrypt(&key(), b"not a container"),
            Err(CryptoError::InvalidFormat(_))
        ));
        assert!(matches!(
            decrypt(&key(), b""),
            Err(CryptoError::InvalidFormat(_))
        ));
    }

    #[test]
    fn default_chunk_size_roundtrip() {
        let plaintext = vec![1u8; CHUNK_SIZE_DEFAULT as usize + 17];
        let sealed = encrypt(&key(), &plaintext);
        assert_eq!(decrypt(&key(), &sealed).unwrap(), plaintext);
    }
}
