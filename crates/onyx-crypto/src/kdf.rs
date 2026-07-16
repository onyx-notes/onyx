//! Passphrase key derivation: argon2id.

use argon2::{Algorithm, Argon2, Params, Version};

use crate::CryptoError;

/// Argon2id parameters, persisted alongside every keyfile so they can be
/// upgraded over time without breaking old vaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub m_cost_kib: u32,
    /// Iterations.
    pub t_cost: u32,
    /// Parallelism.
    pub p_cost: u32,
}

impl KdfParams {
    /// Desktop default: 256 MiB, 3 iterations, 4 lanes (OWASP-plus; unlock
    /// is a rare, user-initiated action so we can afford to be slow).
    pub const DESKTOP: Self = Self {
        m_cost_kib: 256 * 1024,
        t_cost: 3,
        p_cost: 4,
    };

    /// Mobile / constrained default: 64 MiB, 3 iterations, 4 lanes.
    pub const MOBILE: Self = Self {
        m_cost_kib: 64 * 1024,
        t_cost: 3,
        p_cost: 4,
    };

    /// Minimal parameters for tests only — cryptographically weak on purpose.
    #[doc(hidden)]
    pub const INSECURE_TEST: Self = Self {
        m_cost_kib: 8,
        t_cost: 1,
        p_cost: 1,
    };
}

/// Derive a 32-byte key-encryption key from a passphrase.
pub(crate) fn derive_kek(
    passphrase: &str,
    salt: &[u8],
    params: KdfParams,
) -> Result<[u8; 32], CryptoError> {
    let argon_params = Params::new(params.m_cost_kib, params.t_cost, params.p_cost, Some(32))
        .map_err(|error| CryptoError::Kdf(error.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, argon_params);

    let mut kek = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut kek)
        .map_err(|error| CryptoError::Kdf(error.to_string()))?;
    Ok(kek)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_for_same_inputs() {
        let salt = [7u8; 16];
        let first = derive_kek("hunter2", &salt, KdfParams::INSECURE_TEST).unwrap();
        let second = derive_kek("hunter2", &salt, KdfParams::INSECURE_TEST).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn different_passphrase_or_salt_changes_key() {
        let salt = [7u8; 16];
        let base = derive_kek("hunter2", &salt, KdfParams::INSECURE_TEST).unwrap();
        assert_ne!(
            base,
            derive_kek("hunter3", &salt, KdfParams::INSECURE_TEST).unwrap()
        );
        assert_ne!(
            base,
            derive_kek("hunter2", &[8u8; 16], KdfParams::INSECURE_TEST).unwrap()
        );
    }
}
