//! The Onyx sync engine.
//!
//! Core model (see the architecture plan):
//!
//! - **Plain markdown files stay the source of truth.** Every text file has
//!   a CRDT sidecar ([`SyncDoc`], a Loro text document); the invariant is
//!   `materialize(doc) == file bytes`, always.
//! - **Local edits** (from the editor or external tools) are diffed into
//!   the CRDT as minimal splices, attributed to this device's peer id.
//! - **Remote updates** are opaque encrypted blobs on the wire; imported
//!   updates merge (Loro's text CRDT never silently drops concurrent
//!   text) and materialize back to disk through the vault's atomic writer.
//! - Convergence is the property test, not a hope: any interleaving of
//!   edits and update exchanges must end with identical text everywhere.

mod doc;
mod store;

pub use doc::SyncDoc;

/// Reserved doc id for the per-vault manifest (tombstones / liveness).
/// Real note ids are blake3 outputs; the all-zero id cannot collide in
/// practice and is filtered from note-doc iteration.
pub const MANIFEST_DOC_ID: [u8; 16] = [0; 16];
pub use store::SyncStore;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("CRDT error: {0}")]
    Crdt(String),
    #[error("sync store error: {0}")]
    Store(#[from] rusqlite::Error),
    #[error("corrupt sync state: {0}")]
    Corrupt(String),
}
