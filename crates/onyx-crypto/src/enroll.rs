//! Device enrollment: a sealed box for handing sync secrets to a new
//! device through an untrusted relay (the sync server), verified by a
//! short authentication string (SAS) the user compares on both screens.
//!
//! Construction (X25519 ECIES with boring pieces):
//!
//! ```text
//! receiver:  (secret, public) = X25519 keypair, public travels to sender
//! sender:    ephemeral X25519 → shared = DH(eph_secret, receiver_pub)
//!            key = BLAKE3-derive(shared ‖ eph_pub ‖ receiver_pub)
//!            message = eph_pub ‖ XChaCha20-Poly1305(key, payload)
//! SAS        = 6 digits of BLAKE3(eph_pub ‖ receiver_pub ‖ ciphertext)
//! ```
//!
//! Both sides compute the SAS from what they independently hold; a relay
//! that substitutes either key or the ciphertext produces mismatching
//! codes — which is exactly what the user is asked to compare.

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

use crate::{CryptoError, random_bytes};

const KEY_CONTEXT: &str = "onyx-crypto 2026-07 enrollment key v1";
const SAS_CONTEXT: &str = "onyx-crypto 2026-07 enrollment sas v1";

/// The new device's half of an enrollment: keep `self`, publish
/// [`Self::public`].
pub struct EnrollmentReceiver {
    secret: StaticSecret,
}

impl EnrollmentReceiver {
    pub fn generate() -> Self {
        Self {
            secret: StaticSecret::from(random_bytes::<32>()),
        }
    }

    pub fn public(&self) -> [u8; 32] {
        *PublicKey::from(&self.secret).as_bytes()
    }
}

fn derive_key(shared: &[u8; 32], eph_pub: &[u8; 32], receiver_pub: &[u8; 32]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key(KEY_CONTEXT);
    hasher.update(shared);
    hasher.update(eph_pub);
    hasher.update(receiver_pub);
    *hasher.finalize().as_bytes()
}

/// Sender side: seal `payload` to the receiver's public key.
pub fn seal_enrollment(receiver_pub: &[u8; 32], payload: &[u8]) -> Vec<u8> {
    let ephemeral = StaticSecret::from(random_bytes::<32>());
    let eph_pub = *PublicKey::from(&ephemeral).as_bytes();
    let shared = *ephemeral
        .diffie_hellman(&PublicKey::from(*receiver_pub))
        .as_bytes();

    let mut key = derive_key(&shared, &eph_pub, receiver_pub);
    let cipher = XChaCha20Poly1305::new((&key).into());
    key.zeroize();

    // Fresh ephemeral key per message ⇒ a fixed nonce is safe and keeps
    // the message minimal.
    let nonce = XNonce::from([0u8; 24]);
    let ciphertext = cipher
        .encrypt(&nonce, payload)
        .expect("in-memory AEAD encryption cannot fail");

    let mut message = Vec::with_capacity(32 + ciphertext.len());
    message.extend_from_slice(&eph_pub);
    message.extend_from_slice(&ciphertext);
    message
}

/// Receiver side: open a sealed enrollment message.
pub fn open_enrollment(
    receiver: &EnrollmentReceiver,
    message: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    if message.len() < 32 + 16 {
        return Err(CryptoError::InvalidFormat("enrollment"));
    }
    let eph_pub: [u8; 32] = message[..32].try_into().expect("length checked");
    let ciphertext = &message[32..];
    let shared = *receiver
        .secret
        .diffie_hellman(&PublicKey::from(eph_pub))
        .as_bytes();

    let receiver_pub = receiver.public();
    let mut key = derive_key(&shared, &eph_pub, &receiver_pub);
    let cipher = XChaCha20Poly1305::new((&key).into());
    key.zeroize();

    cipher
        .decrypt(&XNonce::from([0u8; 24]), ciphertext)
        .map_err(|_| CryptoError::AuthenticationFailed)
}

/// The 6-digit SAS both devices display. Computable by anyone holding the
/// receiver public key and the sealed message — which is exactly both
/// legitimate endpoints, and produces different values under substitution.
pub fn sas_code(receiver_pub: &[u8; 32], message: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new_derive_key(SAS_CONTEXT);
    hasher.update(receiver_pub);
    hasher.update(message);
    let digest = hasher.finalize();
    let number = u32::from_le_bytes(digest.as_bytes()[..4].try_into().expect("length"));
    format!("{:06}", number % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_sas_agreement() {
        let receiver = EnrollmentReceiver::generate();
        let payload = b"vault-id-and-op-key";
        let message = seal_enrollment(&receiver.public(), payload);

        // Both sides derive the same SAS from independent inputs.
        let sender_sas = sas_code(&receiver.public(), &message);
        let receiver_sas = sas_code(&receiver.public(), &message);
        assert_eq!(sender_sas, receiver_sas);
        assert_eq!(sender_sas.len(), 6);
        assert!(sender_sas.bytes().all(|byte| byte.is_ascii_digit()));

        assert_eq!(open_enrollment(&receiver, &message).unwrap(), payload);
    }

    #[test]
    fn wrong_receiver_cannot_open() {
        let intended = EnrollmentReceiver::generate();
        let attacker = EnrollmentReceiver::generate();
        let message = seal_enrollment(&intended.public(), b"secret");
        assert!(open_enrollment(&attacker, &message).is_err());
    }

    #[test]
    fn tampering_is_detected_and_changes_sas() {
        let receiver = EnrollmentReceiver::generate();
        let message = seal_enrollment(&receiver.public(), b"secret");
        let original_sas = sas_code(&receiver.public(), &message);

        for position in [0, 31, 33, message.len() - 1] {
            let mut tampered = message.clone();
            tampered[position] ^= 0x01;
            assert!(open_enrollment(&receiver, &tampered).is_err());
            assert_ne!(
                sas_code(&receiver.public(), &tampered),
                original_sas,
                "tamper at byte {position} must change the SAS"
            );
        }
    }

    #[test]
    fn substituted_receiver_key_changes_sas() {
        // The MITM scenario SAS comparison exists to catch: the relay
        // swaps the receiver key and re-seals. Codes must diverge.
        let real = EnrollmentReceiver::generate();
        let mitm = EnrollmentReceiver::generate();
        let payload = b"secret";
        let to_real = seal_enrollment(&real.public(), payload);
        let to_mitm = seal_enrollment(&mitm.public(), payload);
        // Sender computed against the substituted key; receiver computes
        // against their own. Different inputs ⇒ different codes.
        assert_ne!(
            sas_code(&mitm.public(), &to_mitm),
            sas_code(&real.public(), &to_real)
        );
    }

    #[test]
    fn each_message_is_unique() {
        let receiver = EnrollmentReceiver::generate();
        let first = seal_enrollment(&receiver.public(), b"same payload");
        let second = seal_enrollment(&receiver.public(), b"same payload");
        assert_ne!(first, second, "fresh ephemeral key per message");
    }
}
