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

    /// Record the vault path inside the document (a `meta` map container).
    /// This is how a brand-new note arriving from a remote device knows
    /// where to materialize — the path travels with the CRDT.
    pub fn set_path(&self, path: &str) -> Result<(), SyncError> {
        self.doc
            .get_map("meta")
            .insert("path", path)
            .map_err(|error| SyncError::Crdt(error.to_string()))?;
        self.doc.commit();
        Ok(())
    }

    /// The vault path recorded in the document, if any.
    pub fn path(&self) -> Option<String> {
        match self.doc.get_map("meta").get("path") {
            Some(loro::ValueOrContainer::Value(loro::LoroValue::String(path))) => {
                Some(path.to_string())
            }
            _ => None,
        }
    }

    // ------------------------------------------------------------------
    // Manifest use (the per-vault tombstone document)
    //
    // The vault manifest is a SyncDoc whose `files` map holds per-doc
    // liveness: key = hex doc id, value = bool. Loro maps are LWW per key,
    // which is exactly the delete/resurrect semantics the plan calls for.
    // ------------------------------------------------------------------

    /// Mark a document live (`true`) or tombstoned (`false`).
    pub fn set_live(&self, doc_id_hex: &str, live: bool) -> Result<(), SyncError> {
        self.doc
            .get_map("files")
            .insert(doc_id_hex, live)
            .map_err(|error| SyncError::Crdt(error.to_string()))?;
        self.doc.commit();
        Ok(())
    }

    /// Liveness of one document; `None` = never mentioned (implicitly live).
    pub fn is_live(&self, doc_id_hex: &str) -> Option<bool> {
        match self.doc.get_map("files").get(doc_id_hex) {
            Some(loro::ValueOrContainer::Value(loro::LoroValue::Bool(live))) => Some(live),
            _ => None,
        }
    }

    /// All liveness entries (hex doc id → live).
    pub fn liveness(&self) -> Vec<(String, bool)> {
        let mut entries = Vec::new();
        if let loro::LoroValue::Map(map) = self.doc.get_map("files").get_value() {
            for (key, value) in map.iter() {
                if let loro::LoroValue::Bool(live) = value {
                    entries.push((key.clone(), *live));
                }
            }
        }
        entries
    }

    /// Record an attachment's current blob (`""` = deleted). LWW per path.
    pub fn set_attachment(&self, path: &str, blob_hash_hex: &str) -> Result<(), SyncError> {
        self.doc
            .get_map("attachments")
            .insert(path, blob_hash_hex)
            .map_err(|error| SyncError::Crdt(error.to_string()))?;
        self.doc.commit();
        Ok(())
    }

    /// All attachment entries (vault path → blob hash hex, "" = deleted).
    pub fn attachments(&self) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        if let loro::LoroValue::Map(map) = self.doc.get_map("attachments").get_value() {
            for (key, value) in map.iter() {
                if let loro::LoroValue::String(hash) = value {
                    entries.push((key.clone(), hash.to_string()));
                }
            }
        }
        entries
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

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn path_travels_with_the_doc() {
        let a = SyncDoc::from_text(1, "content").unwrap();
        assert_eq!(a.path(), None);
        a.set_path("folder/Note.md").unwrap();
        assert_eq!(a.path().as_deref(), Some("folder/Note.md"));

        // A fresh device importing the update learns the path.
        let b = SyncDoc::new(2);
        b.import(&a.export_from(&[]).unwrap()).unwrap();
        assert_eq!(b.path().as_deref(), Some("folder/Note.md"));
        assert_eq!(b.text(), "content");
    }
}

#[cfg(test)]
mod manifest_tests {
    use super::*;

    #[test]
    fn liveness_roundtrip_and_merge() {
        let a = SyncDoc::new(1);
        a.set_live("aa11", false).unwrap();
        a.set_live("bb22", true).unwrap();
        assert_eq!(a.is_live("aa11"), Some(false));
        assert_eq!(a.is_live("bb22"), Some(true));
        assert_eq!(a.is_live("unknown"), None);

        // Merge to another device.
        let b = SyncDoc::new(2);
        b.import(&a.export_from(&[]).unwrap()).unwrap();
        assert_eq!(b.is_live("aa11"), Some(false));

        // Concurrent tombstone (A) vs resurrect (B) on the same key:
        // LWW picks one winner and BOTH sides agree on it.
        a.set_live("cc33", false).unwrap();
        b.set_live("cc33", true).unwrap();
        let a_to_b = a.export_from(&b.version()).unwrap();
        let b_to_a = b.export_from(&a.version()).unwrap();
        a.import(&b_to_a).unwrap();
        b.import(&a_to_b).unwrap();
        assert_eq!(a.is_live("cc33"), b.is_live("cc33"));

        let entries = a.liveness();
        assert_eq!(entries.len(), 3);
    }
}
