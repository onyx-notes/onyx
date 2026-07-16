//! One synced text document: a Loro CRDT whose single text container IS
//! the file content (frontmatter included, as text).

use loro::{ExportMode, LoroDoc};

use crate::SyncError;

/// The text container id within each document.
const CONTENT: &str = "content";

pub struct SyncDoc {
    doc: LoroDoc,
}

impl SyncDoc {
    /// A fresh, empty document owned by `peer` (the device's stable peer
    /// id — attribution and tie-breaking key).
    pub fn new(peer: u64) -> Self {
        let doc = LoroDoc::new();
        doc.set_peer_id(peer).expect("fresh doc accepts a peer id");
        Self { doc }
    }

    /// Create from existing file content (first sync of an existing note).
    pub fn from_text(peer: u64, text: &str) -> Result<Self, SyncError> {
        let doc = Self::new(peer);
        doc.set_text(text)?;
        Ok(doc)
    }

    /// Restore from a snapshot produced by [`Self::snapshot`].
    pub fn from_snapshot(peer: u64, bytes: &[u8]) -> Result<Self, SyncError> {
        let doc = LoroDoc::new();
        doc.import(bytes)
            .map_err(|error| SyncError::Corrupt(error.to_string()))?;
        doc.set_peer_id(peer)
            .map_err(|error| SyncError::Crdt(error.to_string()))?;
        Ok(Self { doc })
    }

    /// Full state snapshot (persisted in the sidecar store).
    pub fn snapshot(&self) -> Result<Vec<u8>, SyncError> {
        self.doc
            .export(ExportMode::Snapshot)
            .map_err(|error| SyncError::Crdt(error.to_string()))
    }

    /// Materialize: the current text — by invariant, the file's bytes.
    pub fn text(&self) -> String {
        self.doc.get_text(CONTENT).to_string()
    }

    /// Apply a local edit: diff current → `new_text` into minimal CRDT
    /// splices (Loro computes the diff), then commit.
    pub fn set_text(&self, new_text: &str) -> Result<(), SyncError> {
        let text = self.doc.get_text(CONTENT);
        text.update(new_text, loro::UpdateOptions::default())
            .map_err(|error| SyncError::Crdt(error.to_string()))?;
        self.doc.commit();
        Ok(())
    }

    /// Encoded version vector — the "how much have you seen" cursor.
    pub fn version(&self) -> Vec<u8> {
        self.doc.oplog_vv().encode()
    }

    /// Export all ops the peer at `since` hasn't seen (empty `since` =
    /// everything).
    pub fn export_from(&self, since: &[u8]) -> Result<Vec<u8>, SyncError> {
        let from = if since.is_empty() {
            Default::default()
        } else {
            loro::VersionVector::decode(since)
                .map_err(|error| SyncError::Corrupt(error.to_string()))?
        };
        self.doc
            .export(ExportMode::updates(&from))
            .map_err(|error| SyncError::Crdt(error.to_string()))
    }

    /// Merge a remote update (or snapshot) into this document.
    pub fn import(&self, update: &[u8]) -> Result<(), SyncError> {
        self.doc
            .import(update)
            .map_err(|error| SyncError::Corrupt(error.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialize_roundtrip() {
        for text in ["", "hello", "multi\nline\ntext\n", "unicodé 日本語 🚀"] {
            let doc = SyncDoc::from_text(1, text).unwrap();
            assert_eq!(doc.text(), text);
        }
    }

    #[test]
    fn local_edits_diff_into_ops() {
        let doc = SyncDoc::from_text(1, "hello world").unwrap();
        doc.set_text("hello brave world").unwrap();
        assert_eq!(doc.text(), "hello brave world");
        doc.set_text("goodbye world").unwrap();
        assert_eq!(doc.text(), "goodbye world");
    }

    #[test]
    fn two_devices_converge_via_updates() {
        // Device A creates the doc, B receives a full copy.
        let a = SyncDoc::from_text(1, "shared base\n").unwrap();
        let b = SyncDoc::new(2);
        b.import(&a.export_from(&[]).unwrap()).unwrap();
        assert_eq!(b.text(), "shared base\n");

        // Concurrent edits on both sides.
        a.set_text("shared base\nfrom A\n").unwrap();
        b.set_text("intro from B\nshared base\n").unwrap();

        // Exchange deltas (each exports what the other hasn't seen).
        let a_to_b = a.export_from(&b.version()).unwrap();
        let b_to_a = b.export_from(&a.version()).unwrap();
        a.import(&b_to_a).unwrap();
        b.import(&a_to_b).unwrap();

        // Converged, and neither side's text was lost.
        assert_eq!(a.text(), b.text());
        assert!(a.text().contains("from A"));
        assert!(a.text().contains("intro from B"));
        assert!(a.text().contains("shared base"));
    }

    #[test]
    fn duplicate_and_out_of_order_imports_are_idempotent() {
        let a = SyncDoc::from_text(1, "base").unwrap();
        let first = a.export_from(&[]).unwrap();
        a.set_text("base plus more").unwrap();
        let second = a.export_from(&[]).unwrap();

        let b = SyncDoc::new(2);
        // Newer first, older second, then duplicates of both.
        b.import(&second).unwrap();
        b.import(&first).unwrap();
        b.import(&second).unwrap();
        b.import(&first).unwrap();
        assert_eq!(b.text(), "base plus more");
    }

    #[test]
    fn snapshot_restore_preserves_history() {
        let a = SyncDoc::from_text(1, "v1").unwrap();
        a.set_text("v1 then v2").unwrap();
        let snapshot = a.snapshot().unwrap();

        let restored = SyncDoc::from_snapshot(1, &snapshot).unwrap();
        assert_eq!(restored.text(), "v1 then v2");

        // Restored doc still syncs: a third device can catch up from it.
        let c = SyncDoc::new(3);
        c.import(&restored.export_from(&[]).unwrap()).unwrap();
        assert_eq!(c.text(), "v1 then v2");
    }

    #[test]
    fn version_vector_prevents_redundant_transfer() {
        let a = SyncDoc::from_text(1, "content here").unwrap();
        let b = SyncDoc::new(2);
        b.import(&a.export_from(&[]).unwrap()).unwrap();
        // B is caught up: the delta for B's version should be tiny
        // (header-only), far smaller than the full export.
        let full = a.export_from(&[]).unwrap();
        let delta = a.export_from(&b.version()).unwrap();
        assert!(delta.len() < full.len());
    }

    #[test]
    fn corrupt_input_is_an_error_not_a_panic() {
        let doc = SyncDoc::new(1);
        assert!(doc.import(b"garbage bytes").is_err());
        assert!(SyncDoc::from_snapshot(1, b"nope").is_err());
    }
}
