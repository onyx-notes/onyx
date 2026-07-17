//! Local note history — the "time machine". Every save records a snapshot
//! so any past version of a note can be viewed and restored, entirely
//! offline. Obsidian gates per-note history behind paid Sync; here it's
//! free and local.
//!
//! Storage: `.onyx/history.db` (SQLite). Snapshots dedupe by plaintext
//! content hash per note (re-saving identical bytes is free). For
//! encrypted vaults an optional [`VaultKey`] encrypts each snapshot's
//! content at rest while the plaintext hash still drives dedup — history
//! is exactly as private as the notes it mirrors.

use std::path::Path;

use onyx_crypto::VaultKey;
use rusqlite::{Connection, OptionalExtension, params};

use crate::paths::NoteId;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS snapshots (
    note_id     BLOB NOT NULL,
    created_ms  INTEGER NOT NULL,
    hash        BLOB NOT NULL,
    content     BLOB NOT NULL,
    encrypted   INTEGER NOT NULL,
    PRIMARY KEY (note_id, created_ms)
) STRICT;
CREATE INDEX IF NOT EXISTS snapshots_note ON snapshots(note_id);
";

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("history db error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("history I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("snapshot decode failed: {0}")]
    Decode(String),
}

/// One stored version: when it was saved and its plaintext content hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub created_ms: u64,
    pub hash: [u8; 32],
}

pub struct History {
    conn: Connection,
    key: Option<VaultKey>,
    /// Millisecond clock; injectable so tests are deterministic.
    now_ms: Box<dyn Fn() -> u64 + Send>,
}

fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

impl History {
    pub fn open(path: &Path, key: Option<VaultKey>) -> Result<Self, HistoryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn,
            key,
            now_ms: Box::new(system_now_ms),
        })
    }

    pub fn open_in_memory(key: Option<VaultKey>) -> Result<Self, HistoryError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn,
            key,
            now_ms: Box::new(system_now_ms),
        })
    }

    #[cfg(test)]
    fn with_clock(mut self, clock: impl Fn() -> u64 + Send + 'static) -> Self {
        self.now_ms = Box::new(clock);
        self
    }

    /// Record a snapshot of `content` for `note_id`. A no-op if the latest
    /// stored version already has this exact content (dedup by plaintext
    /// hash). Returns whether a new snapshot was written.
    pub fn record(&self, note_id: NoteId, content: &[u8]) -> Result<bool, HistoryError> {
        let hash = *blake3::hash(content).as_bytes();

        let latest: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT hash FROM snapshots WHERE note_id = ?1
                 ORDER BY created_ms DESC LIMIT 1",
                params![note_id.as_bytes()],
                |row| row.get(0),
            )
            .optional()?;
        if latest.as_deref() == Some(&hash) {
            return Ok(false);
        }

        let (stored, encrypted) = match &self.key {
            Some(key) => (onyx_crypto::encrypt(key, content), 1),
            None => (content.to_vec(), 0),
        };
        // Monotonic timestamp: never collide with an existing row for this
        // note (multiple saves within a millisecond still each store).
        let mut created = (self.now_ms)();
        while self.exists_at(note_id, created)? {
            created += 1;
        }
        self.conn.execute(
            "INSERT INTO snapshots (note_id, created_ms, hash, content, encrypted)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![note_id.as_bytes(), created as i64, hash, stored, encrypted],
        )?;
        Ok(true)
    }

    fn exists_at(&self, note_id: NoteId, created_ms: u64) -> Result<bool, HistoryError> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM snapshots WHERE note_id = ?1 AND created_ms = ?2",
                params![note_id.as_bytes(), created_ms as i64],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Versions for a note, newest first.
    pub fn versions(&self, note_id: NoteId) -> Result<Vec<Version>, HistoryError> {
        let mut statement = self.conn.prepare(
            "SELECT created_ms, hash FROM snapshots WHERE note_id = ?1
             ORDER BY created_ms DESC",
        )?;
        let rows = statement
            .query_map(params![note_id.as_bytes()], |row| {
                let hash: Vec<u8> = row.get(1)?;
                Ok(Version {
                    created_ms: row.get::<_, i64>(0)? as u64,
                    hash: hash.try_into().unwrap_or([0; 32]),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Retrieve a specific version's plaintext content.
    pub fn get(&self, note_id: NoteId, created_ms: u64) -> Result<Option<String>, HistoryError> {
        let row: Option<(Vec<u8>, i64)> = self
            .conn
            .query_row(
                "SELECT content, encrypted FROM snapshots
                 WHERE note_id = ?1 AND created_ms = ?2",
                params![note_id.as_bytes(), created_ms as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((stored, encrypted)) = row else {
            return Ok(None);
        };
        let plaintext = if encrypted != 0 {
            let key = self
                .key
                .as_ref()
                .ok_or_else(|| HistoryError::Decode("no key for encrypted snapshot".into()))?;
            onyx_crypto::decrypt(key, &stored)
                .map_err(|error| HistoryError::Decode(error.to_string()))?
        } else {
            stored
        };
        Ok(Some(String::from_utf8_lossy(&plaintext).into_owned()))
    }

    /// Keep the newest `keep` versions per note, delete the rest. Returns
    /// how many were pruned.
    pub fn prune(&self, note_id: NoteId, keep: usize) -> Result<usize, HistoryError> {
        let deleted = self.conn.execute(
            "DELETE FROM snapshots WHERE note_id = ?1 AND created_ms NOT IN (
                 SELECT created_ms FROM snapshots WHERE note_id = ?1
                 ORDER BY created_ms DESC LIMIT ?2
             )",
            params![note_id.as_bytes(), keep as i64],
        )?;
        Ok(deleted)
    }

    /// Drop all history for a note (on delete, if the user chooses).
    pub fn forget(&self, note_id: NoteId) -> Result<(), HistoryError> {
        self.conn.execute(
            "DELETE FROM snapshots WHERE note_id = ?1",
            params![note_id.as_bytes()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn id(byte: u8) -> NoteId {
        NoteId::from_bytes([byte; 16])
    }

    /// A clock the test drives forward explicitly.
    fn ticking() -> (Arc<AtomicU64>, impl Fn() -> u64 + Send + 'static) {
        let clock = Arc::new(AtomicU64::new(1000));
        let handle = Arc::clone(&clock);
        (clock, move || handle.load(Ordering::Relaxed))
    }

    #[test]
    fn records_versions_and_dedupes() {
        let (clock, tick) = ticking();
        let history = History::open_in_memory(None).unwrap().with_clock(tick);

        assert!(history.record(id(1), b"v1").unwrap());
        clock.store(2000, Ordering::Relaxed);
        assert!(history.record(id(1), b"v2").unwrap());
        // Identical to the latest → not stored again.
        clock.store(3000, Ordering::Relaxed);
        assert!(!history.record(id(1), b"v2").unwrap());

        let versions = history.versions(id(1)).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].created_ms, 2000); // newest first
        assert_eq!(versions[1].created_ms, 1000);

        assert_eq!(history.get(id(1), 1000).unwrap().as_deref(), Some("v1"));
        assert_eq!(history.get(id(1), 2000).unwrap().as_deref(), Some("v2"));
        assert_eq!(history.get(id(1), 9999).unwrap(), None);
    }

    #[test]
    fn same_content_dedupes_but_returning_to_it_records_again() {
        let (clock, tick) = ticking();
        let history = History::open_in_memory(None).unwrap().with_clock(tick);
        history.record(id(1), b"A").unwrap();
        clock.store(2000, Ordering::Relaxed);
        history.record(id(1), b"B").unwrap();
        clock.store(3000, Ordering::Relaxed);
        // Back to A — different from the *latest* (B), so it records.
        assert!(history.record(id(1), b"A").unwrap());
        assert_eq!(history.versions(id(1)).unwrap().len(), 3);
    }

    #[test]
    fn sub_millisecond_saves_do_not_collide() {
        let history = History::open_in_memory(None).unwrap().with_clock(|| 5000); // clock frozen
        assert!(history.record(id(1), b"a").unwrap());
        assert!(history.record(id(1), b"b").unwrap());
        assert!(history.record(id(1), b"c").unwrap());
        let versions = history.versions(id(1)).unwrap();
        assert_eq!(versions.len(), 3);
        // Distinct, monotonically bumped timestamps.
        assert_eq!(versions[0].created_ms, 5002);
        assert_eq!(versions[2].created_ms, 5000);
    }

    #[test]
    fn encrypted_history_is_opaque_but_dedupes_by_plaintext() {
        let key = VaultKey::from_bytes([7; 32]);
        let history = History::open_in_memory(Some(key)).unwrap().with_clock(|| 1);
        history.record(id(1), b"secret content").unwrap();
        // Round-trips through decryption.
        assert_eq!(
            history.get(id(1), 1).unwrap().as_deref(),
            Some("secret content")
        );
        // Dedup still works on plaintext despite randomized ciphertext.
        assert!(!history.record(id(1), b"secret content").unwrap());

        // Raw stored bytes contain no plaintext.
        let raw: Vec<u8> = history
            .conn
            .query_row(
                "SELECT content FROM snapshots WHERE note_id = ?1",
                params![id(1).as_bytes()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!raw.windows(6).any(|w| w == b"secret"));
    }

    #[test]
    fn prune_keeps_newest() {
        let (clock, tick) = ticking();
        let history = History::open_in_memory(None).unwrap().with_clock(tick);
        for (index, content) in ["v1", "v2", "v3", "v4"].iter().enumerate() {
            clock.store(1000 + index as u64 * 1000, Ordering::Relaxed);
            history.record(id(1), content.as_bytes()).unwrap();
        }
        assert_eq!(history.prune(id(1), 2).unwrap(), 2);
        let versions = history.versions(id(1)).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(history.get(id(1), 4000).unwrap().as_deref(), Some("v4"));
        assert_eq!(history.get(id(1), 3000).unwrap().as_deref(), Some("v3"));
        assert!(history.get(id(1), 1000).unwrap().is_none());
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        {
            let history = History::open(&path, None).unwrap().with_clock(|| 1);
            history.record(id(9), b"durable").unwrap();
        }
        let history = History::open(&path, None).unwrap();
        assert_eq!(history.get(id(9), 1).unwrap().as_deref(), Some("durable"));
    }

    #[test]
    fn forget_removes_all() {
        let history = History::open_in_memory(None).unwrap().with_clock(|| 1);
        history.record(id(1), b"x").unwrap();
        history.forget(id(1)).unwrap();
        assert!(history.versions(id(1)).unwrap().is_empty());
    }
}
