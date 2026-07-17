//! Raw-SQL storage. SQLite, single writer behind a mutex — the correct
//! amount of database for a self-hosted single-family server.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS devices (
    id          BLOB PRIMARY KEY,
    ed25519_pub BLOB NOT NULL,
    created_at  INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS challenges (
    nonce      BLOB PRIMARY KEY,
    device_id  BLOB NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS sessions (
    token_hash BLOB PRIMARY KEY,
    device_id  BLOB NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS vaults (
    id         BLOB PRIMARY KEY,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS vault_devices (
    vault_id  BLOB NOT NULL,
    device_id BLOB NOT NULL,
    PRIMARY KEY (vault_id, device_id)
) STRICT;

CREATE TABLE IF NOT EXISTS enrollments (
    code       TEXT PRIMARY KEY,
    request    BLOB NOT NULL,
    response   BLOB,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS shares (
    id         TEXT PRIMARY KEY,
    device_id  BLOB NOT NULL,
    blob       BLOB NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

-- Blobs are stored in chunks so a large attachment transfers (and resumes)
-- one bounded piece at a time. `blobs` is the manifest — `complete` flips to
-- 1 only after every chunk is present and the reassembled ciphertext hashes
-- to `hash`. Incomplete blobs are never served.
CREATE TABLE IF NOT EXISTS blobs (
    vault_id   BLOB NOT NULL,
    hash       TEXT NOT NULL,
    total      INTEGER NOT NULL,
    size       INTEGER NOT NULL,
    complete   INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, hash)
) STRICT;

CREATE TABLE IF NOT EXISTS blob_chunks (
    vault_id BLOB NOT NULL,
    hash     TEXT NOT NULL,
    idx      INTEGER NOT NULL,
    data     BLOB NOT NULL,
    PRIMARY KEY (vault_id, hash, idx)
) STRICT;

CREATE TABLE IF NOT EXISTS ops (
    vault_id   BLOB NOT NULL,
    seq        INTEGER NOT NULL,
    doc_id     BLOB NOT NULL,
    op_id      BLOB NOT NULL,
    device_id  BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    checkpoint INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, seq)
) STRICT;

-- Idempotency: a retried push carries the same op_id and is dropped.
CREATE UNIQUE INDEX IF NOT EXISTS ops_op_id ON ops(vault_id, op_id);
-- Fast per-doc counting (checkpoint hints) and pruning.
CREATE INDEX IF NOT EXISTS ops_vault_doc ON ops(vault_id, doc_id);
";

/// Challenge validity window.
const CHALLENGE_TTL_SECS: i64 = 300;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("reassembled blob does not match its content hash")]
    HashMismatch,
}

/// A doc's op count crossing this threshold makes the server ask a client to
/// checkpoint it (see `docs_over_threshold`). Kept well above the number of
/// ops a normal editing session produces so checkpoints are rare.
pub const CHECKPOINT_THRESHOLD: usize = 256;

pub struct Db {
    conn: Mutex<Connection>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    // ------------------------------------------------------------------
    // Devices & auth
    // ------------------------------------------------------------------

    pub fn register_device(&self, id: [u8; 16], public_key: &[u8; 32]) -> Result<(), DbError> {
        self.conn.lock().execute(
            "INSERT INTO devices (id, ed25519_pub, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO NOTHING",
            params![id, public_key, now()],
        )?;
        Ok(())
    }

    pub fn device_public_key(&self, id: [u8; 16]) -> Result<Option<[u8; 32]>, DbError> {
        let row: Option<Vec<u8>> = self
            .conn
            .lock()
            .query_row(
                "SELECT ed25519_pub FROM devices WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row.and_then(|bytes| bytes.try_into().ok()))
    }

    pub fn store_challenge(&self, nonce: [u8; 32], device_id: [u8; 16]) -> Result<(), DbError> {
        let conn = self.conn.lock();
        // Opportunistic cleanup of expired challenges.
        conn.execute(
            "DELETE FROM challenges WHERE created_at < ?1",
            params![now() - CHALLENGE_TTL_SECS],
        )?;
        conn.execute(
            "INSERT INTO challenges (nonce, device_id, created_at) VALUES (?1, ?2, ?3)",
            params![nonce, device_id, now()],
        )?;
        Ok(())
    }

    /// Consume a challenge (single use). Returns whether it was valid and
    /// unexpired for this device.
    pub fn consume_challenge(&self, nonce: [u8; 32], device_id: [u8; 16]) -> Result<bool, DbError> {
        let conn = self.conn.lock();
        let deleted = conn.execute(
            "DELETE FROM challenges
             WHERE nonce = ?1 AND device_id = ?2 AND created_at >= ?3",
            params![nonce, device_id, now() - CHALLENGE_TTL_SECS],
        )?;
        Ok(deleted == 1)
    }

    pub fn create_session(&self, token_hash: [u8; 32], device_id: [u8; 16]) -> Result<(), DbError> {
        self.conn.lock().execute(
            "INSERT INTO sessions (token_hash, device_id, created_at) VALUES (?1, ?2, ?3)",
            params![token_hash, device_id, now()],
        )?;
        Ok(())
    }

    pub fn session_device(&self, token_hash: [u8; 32]) -> Result<Option<[u8; 16]>, DbError> {
        let row: Option<Vec<u8>> = self
            .conn
            .lock()
            .query_row(
                "SELECT device_id FROM sessions WHERE token_hash = ?1",
                params![token_hash],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row.and_then(|bytes| bytes.try_into().ok()))
    }

    // ------------------------------------------------------------------
    // Vaults & ops
    // ------------------------------------------------------------------

    pub fn join_vault(&self, vault_id: [u8; 16], device_id: [u8; 16]) -> Result<(), DbError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO vaults (id, created_at) VALUES (?1, ?2)
             ON CONFLICT(id) DO NOTHING",
            params![vault_id, now()],
        )?;
        conn.execute(
            "INSERT INTO vault_devices (vault_id, device_id) VALUES (?1, ?2)
             ON CONFLICT DO NOTHING",
            params![vault_id, device_id],
        )?;
        Ok(())
    }

    pub fn is_member(&self, vault_id: [u8; 16], device_id: [u8; 16]) -> Result<bool, DbError> {
        let count: i64 = self.conn.lock().query_row(
            "SELECT COUNT(*) FROM vault_devices WHERE vault_id = ?1 AND device_id = ?2",
            params![vault_id, device_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Append ops atomically, assigning consecutive sequence numbers.
    /// Returns the vault head after the append.
    ///
    /// Idempotent per `op_id`: an op the vault already holds (a retry after
    /// a flaky link dropped the ack) is skipped, not stored again. A
    /// checkpoint op supersedes its doc's earlier ops — they're pruned in
    /// the same transaction, bounding oplog growth.
    pub fn append_ops(
        &self,
        vault_id: [u8; 16],
        device_id: [u8; 16],
        ops: &[onyx_proto::EncOp],
    ) -> Result<u64, DbError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let mut head: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM ops WHERE vault_id = ?1",
                params![vault_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        for op in ops {
            // Already stored under this op_id? A duplicate delivery — skip
            // without burning a seq, so resends can't inflate the log.
            let seen: Option<i64> = tx
                .query_row(
                    "SELECT seq FROM ops WHERE vault_id = ?1 AND op_id = ?2",
                    params![vault_id, op.op_id],
                    |row| row.get(0),
                )
                .optional()?;
            if seen.is_some() {
                continue;
            }
            head += 1;
            tx.execute(
                "INSERT INTO ops
                    (vault_id, seq, doc_id, op_id, device_id, ciphertext, checkpoint, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    vault_id,
                    head,
                    op.doc_id,
                    op.op_id,
                    device_id,
                    op.ciphertext,
                    op.checkpoint as i64,
                    now()
                ],
            )?;
            if op.checkpoint {
                // The checkpoint's full state subsumes every earlier op for
                // this doc; drop them. Peers that hadn't seen those ops get
                // the checkpoint instead and converge (CRDT reseed).
                tx.execute(
                    "DELETE FROM ops WHERE vault_id = ?1 AND doc_id = ?2 AND seq < ?3",
                    params![vault_id, op.doc_id, head],
                )?;
            }
        }
        tx.commit()?;
        Ok(head as u64)
    }

    /// Docs whose stored op count has reached `threshold` — the server hands
    /// these back as checkpoint hints so a client compacts them. Cheap while
    /// checkpointing keeps per-doc counts small (the steady state).
    pub fn docs_over_threshold(
        &self,
        vault_id: [u8; 16],
        threshold: usize,
    ) -> Result<Vec<[u8; 16]>, DbError> {
        let conn = self.conn.lock();
        let mut statement = conn.prepare(
            "SELECT doc_id FROM ops WHERE vault_id = ?1
             GROUP BY doc_id HAVING COUNT(*) >= ?2",
        )?;
        let ids = statement
            .query_map(params![vault_id, threshold as i64], |row| {
                row.get::<_, Vec<u8>>(0)
            })?
            .filter_map(|result| result.ok().and_then(|bytes| bytes.try_into().ok()))
            .collect();
        Ok(ids)
    }

    /// Enrollment relay: opaque request/response blobs under a short-lived
    /// code. TTL keeps the table clean; contents are sealed client-side.
    pub fn enroll_create(&self, code: &str, request: &[u8]) -> Result<bool, DbError> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM enrollments WHERE created_at < ?1",
            params![now() - 600],
        )?;
        let inserted = conn.execute(
            "INSERT INTO enrollments (code, request, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT DO NOTHING",
            params![code, request, now()],
        )?;
        Ok(inserted == 1)
    }

    pub fn enroll_request(&self, code: &str) -> Result<Option<Vec<u8>>, DbError> {
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT request FROM enrollments WHERE code = ?1 AND created_at >= ?2",
                params![code, now() - 600],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn enroll_respond(&self, code: &str, response: &[u8]) -> Result<bool, DbError> {
        let updated = self.conn.lock().execute(
            "UPDATE enrollments SET response = ?2
             WHERE code = ?1 AND response IS NULL AND created_at >= ?3",
            params![code, response, now() - 600],
        )?;
        Ok(updated == 1)
    }

    /// Claim (and delete — single use) the response for a code.
    pub fn enroll_claim(&self, code: &str) -> Result<Option<Vec<u8>>, DbError> {
        let conn = self.conn.lock();
        let response: Option<Option<Vec<u8>>> = conn
            .query_row(
                "SELECT response FROM enrollments WHERE code = ?1",
                params![code],
                |row| row.get(0),
            )
            .optional()?;
        match response.flatten() {
            Some(payload) => {
                conn.execute("DELETE FROM enrollments WHERE code = ?1", params![code])?;
                Ok(Some(payload))
            }
            None => Ok(None),
        }
    }

    pub fn put_share(&self, id: &str, device_id: [u8; 16], blob: &[u8]) -> Result<(), DbError> {
        self.conn.lock().execute(
            "INSERT INTO shares (id, device_id, blob, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET blob = ?3",
            params![id, device_id, blob, now()],
        )?;
        Ok(())
    }

    /// Public read: a share is protected by the key in the link fragment,
    /// which the server never sees, so serving the ciphertext is safe.
    pub fn get_share(&self, id: &str) -> Result<Option<Vec<u8>>, DbError> {
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT blob FROM shares WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Delete a share, but only by the device that created it.
    pub fn delete_share(&self, id: &str, device_id: [u8; 16]) -> Result<bool, DbError> {
        let deleted = self.conn.lock().execute(
            "DELETE FROM shares WHERE id = ?1 AND device_id = ?2",
            params![id, device_id],
        )?;
        Ok(deleted == 1)
    }

    /// Single-shot store for a small (≤ one chunk) blob whose hash the caller
    /// has already verified. Stored as a one-chunk complete blob; any stale
    /// partial upload for the same hash is cleared first.
    pub fn put_blob(&self, vault_id: [u8; 16], hash: &str, data: &[u8]) -> Result<(), DbError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM blob_chunks WHERE vault_id = ?1 AND hash = ?2",
            params![vault_id, hash],
        )?;
        tx.execute(
            "INSERT INTO blobs (vault_id, hash, total, size, complete, created_at)
             VALUES (?1, ?2, 1, ?3, 1, ?4)
             ON CONFLICT(vault_id, hash) DO UPDATE SET total = 1, size = ?3, complete = 1",
            params![vault_id, hash, data.len() as i64, now()],
        )?;
        tx.execute(
            "INSERT INTO blob_chunks (vault_id, hash, idx, data) VALUES (?1, ?2, 0, ?3)",
            params![vault_id, hash, data],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Record the expected shape of a chunked upload (idempotent). Leaves an
    /// already-complete blob untouched.
    pub fn blob_begin(
        &self,
        vault_id: [u8; 16],
        hash: &str,
        total: u32,
        size: u64,
    ) -> Result<(), DbError> {
        self.conn.lock().execute(
            "INSERT INTO blobs (vault_id, hash, total, size, complete, created_at)
             VALUES (?1, ?2, ?3, ?4, 0, ?5)
             ON CONFLICT(vault_id, hash) DO NOTHING",
            params![vault_id, hash, total as i64, size as i64, now()],
        )?;
        Ok(())
    }

    /// Store one chunk (idempotent — re-uploading a chunk is a no-op).
    pub fn blob_put_chunk(
        &self,
        vault_id: [u8; 16],
        hash: &str,
        idx: u32,
        data: &[u8],
    ) -> Result<(), DbError> {
        self.conn.lock().execute(
            "INSERT INTO blob_chunks (vault_id, hash, idx, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(vault_id, hash, idx) DO NOTHING",
            params![vault_id, hash, idx as i64, data],
        )?;
        Ok(())
    }

    /// Which chunks are present (for resume), plus the expected total and
    /// completion flag. `None` if the upload was never begun.
    pub fn blob_status(
        &self,
        vault_id: [u8; 16],
        hash: &str,
    ) -> Result<Option<onyx_proto::BlobStatus>, DbError> {
        let conn = self.conn.lock();
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT total, complete FROM blobs WHERE vault_id = ?1 AND hash = ?2",
                params![vault_id, hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((total, complete)) = row else {
            return Ok(None);
        };
        let mut statement = conn.prepare(
            "SELECT idx FROM blob_chunks WHERE vault_id = ?1 AND hash = ?2 ORDER BY idx",
        )?;
        let present = statement
            .query_map(params![vault_id, hash], |row| row.get::<_, i64>(0))?
            .filter_map(|result| result.ok().map(|idx| idx as u32))
            .collect();
        Ok(Some(onyx_proto::BlobStatus {
            present,
            total: total as u32,
            complete: complete != 0,
        }))
    }

    /// If every chunk has arrived, reassemble (streaming — one chunk in RAM
    /// at a time), verify the content hash, and mark complete. On mismatch
    /// the partial upload is discarded so the client can retry cleanly.
    /// Returns whether the blob is now complete.
    pub fn blob_try_complete(&self, vault_id: [u8; 16], hash: &str) -> Result<bool, DbError> {
        let conn = self.conn.lock();
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT total, complete FROM blobs WHERE vault_id = ?1 AND hash = ?2",
                params![vault_id, hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((total, complete)) = row else {
            return Ok(false);
        };
        if complete != 0 {
            return Ok(true);
        }
        let present: i64 = conn.query_row(
            "SELECT COUNT(*) FROM blob_chunks WHERE vault_id = ?1 AND hash = ?2",
            params![vault_id, hash],
            |row| row.get(0),
        )?;
        if present != total {
            return Ok(false);
        }
        let mut hasher = blake3::Hasher::new();
        {
            let mut statement = conn.prepare(
                "SELECT data FROM blob_chunks WHERE vault_id = ?1 AND hash = ?2 ORDER BY idx",
            )?;
            let mut rows = statement.query(params![vault_id, hash])?;
            while let Some(row) = rows.next()? {
                let data: Vec<u8> = row.get(0)?;
                hasher.update(&data);
            }
        }
        if hasher.finalize().to_hex().as_str() != hash {
            conn.execute(
                "DELETE FROM blob_chunks WHERE vault_id = ?1 AND hash = ?2",
                params![vault_id, hash],
            )?;
            conn.execute(
                "DELETE FROM blobs WHERE vault_id = ?1 AND hash = ?2",
                params![vault_id, hash],
            )?;
            return Err(DbError::HashMismatch);
        }
        conn.execute(
            "UPDATE blobs SET complete = 1 WHERE vault_id = ?1 AND hash = ?2",
            params![vault_id, hash],
        )?;
        Ok(true)
    }

    /// The full ciphertext of a complete blob (assembled from its chunks).
    /// `None` unless complete. For large blobs prefer `blob_read_range`.
    pub fn get_blob(&self, vault_id: [u8; 16], hash: &str) -> Result<Option<Vec<u8>>, DbError> {
        let conn = self.conn.lock();
        let complete: Option<i64> = conn
            .query_row(
                "SELECT complete FROM blobs WHERE vault_id = ?1 AND hash = ?2",
                params![vault_id, hash],
                |row| row.get(0),
            )
            .optional()?;
        if complete != Some(1) {
            return Ok(None);
        }
        let mut statement = conn.prepare(
            "SELECT data FROM blob_chunks WHERE vault_id = ?1 AND hash = ?2 ORDER BY idx",
        )?;
        let mut rows = statement.query(params![vault_id, hash])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let data: Vec<u8> = row.get(0)?;
            out.extend_from_slice(&data);
        }
        Ok(Some(out))
    }

    /// The total size of a complete blob, or `None`.
    pub fn blob_size(&self, vault_id: [u8; 16], hash: &str) -> Result<Option<u64>, DbError> {
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT size FROM blobs WHERE vault_id = ?1 AND hash = ?2 AND complete = 1",
                params![vault_id, hash],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|size| size as u64))
    }

    /// Read `[start, start+len)` of a complete blob, touching only the
    /// overlapping chunks (bounded RAM — this is the download-resume lane).
    /// `None` unless complete; an empty vec if `start` is past the end.
    pub fn blob_read_range(
        &self,
        vault_id: [u8; 16],
        hash: &str,
        start: u64,
        len: u64,
    ) -> Result<Option<Vec<u8>>, DbError> {
        let conn = self.conn.lock();
        let row: Option<(i64, i64)> = conn
            .query_row(
                "SELECT size, complete FROM blobs WHERE vault_id = ?1 AND hash = ?2",
                params![vault_id, hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((size, complete)) = row else {
            return Ok(None);
        };
        if complete == 0 {
            return Ok(None);
        }
        let size = size as u64;
        if start >= size {
            return Ok(Some(Vec::new()));
        }
        let end = start.saturating_add(len).min(size); // exclusive
        let mut statement = conn.prepare(
            "SELECT data FROM blob_chunks WHERE vault_id = ?1 AND hash = ?2 ORDER BY idx",
        )?;
        let mut rows = statement.query(params![vault_id, hash])?;
        let mut out = Vec::with_capacity((end - start) as usize);
        let mut offset: u64 = 0;
        while let Some(row) = rows.next()? {
            let data: Vec<u8> = row.get(0)?;
            let chunk_start = offset;
            let chunk_end = offset + data.len() as u64;
            offset = chunk_end;
            if chunk_end <= start {
                continue;
            }
            if chunk_start >= end {
                break;
            }
            let from = start.saturating_sub(chunk_start) as usize;
            let to = (end.min(chunk_end) - chunk_start) as usize;
            out.extend_from_slice(&data[from..to]);
        }
        Ok(Some(out))
    }

    pub fn has_blob(&self, vault_id: [u8; 16], hash: &str) -> Result<bool, DbError> {
        let count: i64 = self.conn.lock().query_row(
            "SELECT COUNT(*) FROM blobs WHERE vault_id = ?1 AND hash = ?2 AND complete = 1",
            params![vault_id, hash],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Ops after `since`, capped at `limit`, plus the current head.
    pub fn ops_since(
        &self,
        vault_id: [u8; 16],
        since: u64,
        limit: usize,
    ) -> Result<(Vec<onyx_proto::StoredOp>, u64), DbError> {
        let conn = self.conn.lock();
        let head: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM ops WHERE vault_id = ?1",
            params![vault_id],
            |row| row.get(0),
        )?;
        let mut statement = conn.prepare(
            "SELECT seq, doc_id, ciphertext FROM ops
             WHERE vault_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let ops = statement
            .query_map(params![vault_id, since as i64, limit as i64], |row| {
                let doc_id: Vec<u8> = row.get(1)?;
                Ok(onyx_proto::StoredOp {
                    seq: row.get::<_, i64>(0)? as u64,
                    doc_id: doc_id.try_into().unwrap_or([0; 16]),
                    ciphertext: row.get(2)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok((ops, head as u64))
    }
}
