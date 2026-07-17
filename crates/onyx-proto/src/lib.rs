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
///
/// v2 added: a per-op idempotency id (`EncOp::op_id`) so a re-push after a
/// lost ack is deduplicated instead of stored twice; a `checkpoint` flag
/// that lets a full-state op supersede (and prune) a doc's earlier ops; and
/// `OpsBatch::checkpoint_hints`, the server asking clients to checkpoint
/// docs whose op history has grown long. Chunked/resumable blob transfer
/// lands on the same version.
pub const PROTOCOL_VERSION: u16 = 2;

/// Blob transfer chunk size. Large attachments upload/download one chunk at
/// a time so a dropped connection loses at most one chunk (not the whole
/// file), and neither peer ever buffers more than a chunk. Content-addressed
/// blobs ≤ this size take the single-shot lane.
pub const BLOB_CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// Derive an op's idempotency id from the document and its *plaintext*
/// update bytes. Deriving from plaintext (not ciphertext, whose nonce is
/// random each encryption) makes a resend of the same logical delta carry
/// the same id, so the server dedupes it. Scoped by `doc_id` so two
/// documents can never collide.
pub fn derive_op_id(doc_id: &[u8; 16], plaintext_update: &[u8]) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(doc_id);
    hasher.update(plaintext_update);
    let digest = hasher.finalize();
    digest.as_bytes()[..16].try_into().expect("16 bytes")
}

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
    /// Idempotency key: the server assigns a seq only the first time it
    /// sees an `(vault, op_id)`. A retry after a lost ack carries the same
    /// id and is dropped, so a flaky link can't inflate the oplog.
    pub op_id: [u8; 16],
    /// Encrypted, compressed Loro update (an `onyx-crypto` container).
    pub ciphertext: Vec<u8>,
    /// A checkpoint carries a doc's *full* state (not an incremental
    /// delta). The server may prune that doc's earlier ops once it lands,
    /// bounding oplog growth. Merging it is a no-op for up-to-date peers
    /// (CRDT idempotency) and reseeds peers whose early ops were pruned.
    pub checkpoint: bool,
}

impl EncOp {
    /// An incremental op. `op_id` is derived from the plaintext update so a
    /// resend of the identical delta dedupes server-side; pass the same
    /// plaintext you encrypted into `ciphertext`.
    pub fn incremental(doc_id: [u8; 16], plaintext_update: &[u8], ciphertext: Vec<u8>) -> Self {
        Self {
            doc_id,
            op_id: derive_op_id(&doc_id, plaintext_update),
            ciphertext,
            checkpoint: false,
        }
    }

    /// A full-state checkpoint op (see [`EncOp::checkpoint`]).
    pub fn checkpoint(doc_id: [u8; 16], plaintext_state: &[u8], ciphertext: Vec<u8>) -> Self {
        Self {
            doc_id,
            op_id: derive_op_id(&doc_id, plaintext_state),
            ciphertext,
            checkpoint: true,
        }
    }
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
    /// Docs whose op history has grown past the checkpoint threshold. A
    /// client that holds one of these should push a full-state checkpoint
    /// (see [`EncOp::checkpoint`]) so the server can prune the backlog.
    #[serde(default)]
    pub checkpoint_hints: Vec<[u8; 16]>,
}

/// Resume status for a chunked blob upload: which chunk indices the server
/// already holds, the expected total, and whether the blob is complete.
/// A client queries this before (re)uploading so it skips finished chunks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobStatus {
    pub present: Vec<u32>,
    pub total: u32,
    pub complete: bool,
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
                EncOp::incremental([1; 16], b"update-a", vec![9, 9, 9]),
                EncOp::checkpoint([2; 16], b"full-state", vec![7]),
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
            checkpoint_hints: vec![[3; 16]],
        };
        let decoded: OpsBatch = decode(&encode(&batch).unwrap()).unwrap();
        assert_eq!(decoded, batch);

        let status = BlobStatus {
            present: vec![0, 1, 3],
            total: 5,
            complete: false,
        };
        assert_eq!(
            decode::<BlobStatus>(&encode(&status).unwrap()).unwrap(),
            status
        );
    }

    #[test]
    fn op_id_is_deterministic_from_plaintext_and_scoped_by_doc() {
        // Same doc + same plaintext ⇒ same id (a resend dedupes).
        let a = EncOp::incremental([1; 16], b"delta", vec![0xAA]);
        let b = EncOp::incremental([1; 16], b"delta", vec![0xBB]); // different ciphertext
        assert_eq!(a.op_id, b.op_id, "id must ignore ciphertext (nonce)");
        // Different doc ⇒ different id.
        let c = EncOp::incremental([2; 16], b"delta", vec![0xAA]);
        assert_ne!(a.op_id, c.op_id);
        // Different plaintext ⇒ different id.
        let d = EncOp::incremental([1; 16], b"delta2", vec![0xAA]);
        assert_ne!(a.op_id, d.op_id);
        // Incremental ops are not checkpoints; the constructors set the flag.
        assert!(!a.checkpoint);
        assert!(EncOp::checkpoint([1; 16], b"s", vec![]).checkpoint);
    }

    #[test]
    fn garbage_decodes_to_error_not_panic() {
        assert!(decode::<PushOps>(b"garbage").is_err());
        assert!(decode::<OpsBatch>(&[]).is_err());
    }
}
