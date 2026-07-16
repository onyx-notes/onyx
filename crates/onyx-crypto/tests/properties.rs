//! Property tests: roundtrips hold for arbitrary data, and corruption is
//! always detected.

use onyx_crypto::{VaultKey, decrypt, decrypt_name, encrypt_name, encrypt_with};
use proptest::prelude::*;

fn key_from(seed: u8) -> VaultKey {
    VaultKey::from_bytes([seed; 32])
}

proptest! {
    /// Container roundtrip for arbitrary payloads and chunk sizes,
    /// including sizes straddling chunk boundaries.
    #[test]
    fn container_roundtrip(
        payload in proptest::collection::vec(any::<u8>(), 0..2048),
        chunk_size in 1u32..512,
        seed in any::<u8>(),
    ) {
        let key = key_from(seed);
        let sealed = encrypt_with(&key, &payload, chunk_size);
        prop_assert_eq!(decrypt(&key, &sealed).unwrap(), payload);
    }

    /// Any single-bit flip anywhere in the container is detected.
    #[test]
    fn container_bitflips_detected(
        payload in proptest::collection::vec(any::<u8>(), 0..256),
        chunk_size in 1u32..64,
        bit in 0usize..8,
        position_seed in any::<u64>(),
    ) {
        let key = key_from(1);
        let mut sealed = encrypt_with(&key, &payload, chunk_size);
        let position = (position_seed as usize) % sealed.len();
        sealed[position] ^= 1 << bit;
        prop_assert!(decrypt(&key, &sealed).is_err());
    }

    /// Filename roundtrip for arbitrary (non-empty) unicode names, and
    /// determinism.
    #[test]
    fn filename_roundtrip_and_deterministic(name in "\\PC{1,80}", seed in any::<u8>()) {
        let key = key_from(seed);
        let token = encrypt_name(&key, &name);
        prop_assert_eq!(decrypt_name(&key, &token).unwrap(), name.clone());
        prop_assert_eq!(token, encrypt_name(&key, &name));
    }

    /// A different key never successfully decrypts a filename token.
    #[test]
    fn filename_wrong_key_rejected(name in "\\PC{1,40}") {
        let token = encrypt_name(&key_from(1), &name);
        prop_assert!(decrypt_name(&key_from(2), &token).is_err());
    }
}
