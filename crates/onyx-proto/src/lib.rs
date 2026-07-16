//! The Onyx wire protocol: versioned message types shared by every client
//! and the server.
//!
//! Encoding is postcard (compact binary over serde). The plan originally
//! called for protobuf "for cross-language clients", but every Onyx client
//! embeds the Rust sync core (Tauri desktop and mobile alike), so
//! cross-language codegen has no consumer; postcard keeps the dependency
//! surface tiny. The envelope is versioned so this decision stays cheap to
//! revisit — the encoding is isolated to this crate.
//!
//! Zero-knowledge discipline: every `ciphertext` field is opaque to the
//! server. Nothing in this protocol carries plaintext note content or
//! names.

use serde::{Deserialize, Serialize};

/// Bump on breaking wire changes; the server rejects unknown majors.
pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("failed to decode message: {0}")]
    Decode(String),
    #[error("failed to encode message: {0}")]
    Encode(String),
}

/// One encrypted CRDT update pushed by a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncOp {
    /// Opaque per-document id (a salted hash on the client; meaningless to
    /// the server).
    pub doc_id: [u8; 16],
    /// Encrypted, compressed Loro update (an `onyx-crypto` container).
    pub ciphertext: Vec<u8>,
}

/// An op as stored and served back: the server assigns a per-vault,
/// monotonically increasing sequence number — a delivery cursor, never an
/// ordering authority (ordering is causal, inside the ciphertext).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredOp {
    pub seq: u64,
    pub doc_id: [u8; 16],
    pub ciphertext: Vec<u8>,
}

/// Push request body: `POST /v1/vaults/{vault}/ops`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushOps {
    pub version: u16,
    pub ops: Vec<EncOp>,
}

/// Push response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushAck {
    /// The vault's head sequence after this push.
    pub head_seq: u64,
}

/// Pull response body: `GET /v1/vaults/{vault}/ops?since=N`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpsBatch {
    pub version: u16,
    pub ops: Vec<StoredOp>,
    /// The vault head at response time; `since == head_seq` means caught up.
    pub head_seq: u64,
}

pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, ProtoError> {
    postcard::to_allocvec(message).map_err(|error| ProtoError::Encode(error.to_string()))
}

pub fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, ProtoError> {
    postcard::from_bytes(bytes).map_err(|error| ProtoError::Decode(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_push_and_batch() {
        let push = PushOps {
            version: PROTOCOL_VERSION,
            ops: vec![
                EncOp {
                    doc_id: [1; 16],
                    ciphertext: vec![9, 9, 9],
                },
                EncOp {
                    doc_id: [2; 16],
                    ciphertext: Vec::new(),
                },
            ],
        };
        let decoded: PushOps = decode(&encode(&push).unwrap()).unwrap();
        assert_eq!(decoded, push);

        let batch = OpsBatch {
            version: PROTOCOL_VERSION,
            ops: vec![StoredOp {
                seq: 42,
                doc_id: [3; 16],
                ciphertext: vec![1, 2, 3],
            }],
            head_seq: 42,
        };
        let decoded: OpsBatch = decode(&encode(&batch).unwrap()).unwrap();
        assert_eq!(decoded, batch);
    }

    #[test]
    fn garbage_decodes_to_error_not_panic() {
        assert!(decode::<PushOps>(b"garbage").is_err());
        assert!(decode::<OpsBatch>(&[]).is_err());
    }
}
