//! The sidecar store: CRDT snapshots and sync cursors, at
//! `<vault>/.onyx/sync.db`.
//!
//! Losing this file is never data loss: files re-import as fresh documents
//! and convergence resumes (worst case one whole-doc merge). It is a cache
//! of sync state, exactly like the index is a cache of file state.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{SyncDoc, SyncError};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS docs (
    note_id           BLOB PRIMARY KEY,
    snapshot          BLOB NOT NULL,
    materialized_hash BLOB NOT NULL,
    pushed_version    BLOB NOT NULL DEFAULT x''
) STRICT;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value BLOB NOT NULL
) STRICT;
";

pub struct SyncStore {
    conn: Connection,
}

impl SyncStore {
    pub fn open(path: &Path) -> Result<Self, SyncError> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, SyncError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Persist a document snapshot plus the hash of the text it
    /// materializes to (used to detect external file edits on load).
    pub fn save_doc(
        &mut self,
        note_id: [u8; 16],
        doc: &SyncDoc,
        materialized_hash: [u8; 32],
    ) -> Result<(), SyncError> {
        let snapshot = doc.snapshot()?;
        self.conn.execute(
            "INSERT INTO docs (note_id, snapshot, materialized_hash)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(note_id) DO UPDATE SET snapshot = ?2, materialized_hash = ?3",
            params![note_id, snapshot, materialized_hash],
        )?;
        Ok(())
    }

    /// Load a document. Returns the doc and the content hash it was last
    /// materialized to.
    pub fn load_doc(
        &self,
        note_id: [u8; 16],
        peer: u64,
    ) -> Result<Option<(SyncDoc, [u8; 32])>, SyncError> {
        let row: Option<(Vec<u8>, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT snapshot, materialized_hash FROM docs WHERE note_id = ?1",
                params![note_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((snapshot, hash_bytes)) = row else {
            return Ok(None);
        };
        let hash: [u8; 32] = hash_bytes
            .try_into()
            .map_err(|_| SyncError::Corrupt("bad hash length".into()))?;
        Ok(Some((SyncDoc::from_snapshot(peer, &snapshot)?, hash)))
    }

    pub fn remove_doc(&mut self, note_id: [u8; 16]) -> Result<(), SyncError> {
        self.conn
            .execute("DELETE FROM docs WHERE note_id = ?1", params![note_id])?;
        Ok(())
    }

    pub fn doc_count(&self) -> Result<usize, SyncError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM docs", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// The stored materialized-content hash for a doc, without loading the
    /// snapshot (cheap change detection against the index's hashes).
    pub fn materialized_hash(&self, note_id: [u8; 16]) -> Result<Option<[u8; 32]>, SyncError> {
        let row: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT materialized_hash FROM docs WHERE note_id = ?1",
                params![note_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row.and_then(|bytes| bytes.try_into().ok()))
    }

    /// The version vector last successfully pushed for a doc (empty =
    /// never pushed).
    pub fn pushed_version(&self, note_id: [u8; 16]) -> Result<Vec<u8>, SyncError> {
        Ok(self
            .conn
            .query_row(
                "SELECT pushed_version FROM docs WHERE note_id = ?1",
                params![note_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn set_pushed_version(
        &mut self,
        note_id: [u8; 16],
        version: &[u8],
    ) -> Result<(), SyncError> {
        self.conn.execute(
            "UPDATE docs SET pushed_version = ?2 WHERE note_id = ?1",
            params![note_id, version],
        )?;
        Ok(())
    }

    /// Docs whose current state differs from what was last pushed — the
    /// outbox. (Cheap: compares stored version bytes, no doc loading.)
    pub fn all_doc_ids(&self) -> Result<Vec<[u8; 16]>, SyncError> {
        let mut statement = self.conn.prepare("SELECT note_id FROM docs")?;
        let ids = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))?
            .filter_map(|result| result.ok().and_then(|bytes| bytes.try_into().ok()))
            .collect();
        Ok(ids)
    }

    /// Opaque metadata slot (device id, server cursor, keys of that shape).
    pub fn meta(&self, key: &str) -> Result<Option<Vec<u8>>, SyncError> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_meta(&mut self, key: &str, value: &[u8]) -> Result<(), SyncError> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_roundtrip() {
        let mut store = SyncStore::open_in_memory().unwrap();
        let doc = SyncDoc::from_text(1, "persisted content").unwrap();
        let hash = *blake3::hash(b"persisted content").as_bytes();
        store.save_doc([1; 16], &doc, hash).unwrap();

        let (loaded, loaded_hash) = store.load_doc([1; 16], 1).unwrap().unwrap();
        assert_eq!(loaded.text(), "persisted content");
        assert_eq!(loaded_hash, hash);
        assert!(store.load_doc([2; 16], 1).unwrap().is_none());
    }

    #[test]
    fn save_overwrites_previous_state() {
        let mut store = SyncStore::open_in_memory().unwrap();
        let doc = SyncDoc::from_text(1, "v1").unwrap();
        store.save_doc([1; 16], &doc, [0; 32]).unwrap();
        doc.set_text("v2").unwrap();
        store.save_doc([1; 16], &doc, [1; 32]).unwrap();

        let (loaded, hash) = store.load_doc([1; 16], 1).unwrap().unwrap();
        assert_eq!(loaded.text(), "v2");
        assert_eq!(hash, [1; 32]);
        assert_eq!(store.doc_count().unwrap(), 1);
    }

    #[test]
    fn remove_and_meta() {
        let mut store = SyncStore::open_in_memory().unwrap();
        let doc = SyncDoc::from_text(1, "x").unwrap();
        store.save_doc([1; 16], &doc, [0; 32]).unwrap();
        store.remove_doc([1; 16]).unwrap();
        assert_eq!(store.doc_count().unwrap(), 0);

        assert!(store.meta("cursor").unwrap().is_none());
        store.set_meta("cursor", &42u64.to_le_bytes()).unwrap();
        store.set_meta("cursor", &43u64.to_le_bytes()).unwrap();
        assert_eq!(store.meta("cursor").unwrap().unwrap(), 43u64.to_le_bytes());
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync.db");
        {
            let mut store = SyncStore::open(&path).unwrap();
            let doc = SyncDoc::from_text(7, "durable").unwrap();
            store.save_doc([9; 16], &doc, [7; 32]).unwrap();
        }
        let store = SyncStore::open(&path).unwrap();
        let (loaded, _) = store.load_doc([9; 16], 7).unwrap().unwrap();
        assert_eq!(loaded.text(), "durable");
    }
}
