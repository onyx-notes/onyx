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

CREATE TABLE IF NOT EXISTS blobs (
    vault_id   BLOB NOT NULL,
    hash       TEXT NOT NULL,
    data       BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, hash)
) STRICT;

CREATE TABLE IF NOT EXISTS ops (
    vault_id   BLOB NOT NULL,
    seq        INTEGER NOT NULL,
    doc_id     BLOB NOT NULL,
    device_id  BLOB NOT NULL,
    ciphertext BLOB NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (vault_id, seq)
) STRICT;
";

/// Challenge validity window.
const CHALLENGE_TTL_SECS: i64 = 300;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

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
            head += 1;
            tx.execute(
                "INSERT INTO ops (vault_id, seq, doc_id, device_id, ciphertext, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![vault_id, head, op.doc_id, device_id, op.ciphertext, now()],
            )?;
        }
        tx.commit()?;
        Ok(head as u64)
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

    pub fn put_blob(&self, vault_id: [u8; 16], hash: &str, data: &[u8]) -> Result<(), DbError> {
        self.conn.lock().execute(
            "INSERT INTO blobs (vault_id, hash, data, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT DO NOTHING",
            params![vault_id, hash, data, now()],
        )?;
        Ok(())
    }

    pub fn get_blob(&self, vault_id: [u8; 16], hash: &str) -> Result<Option<Vec<u8>>, DbError> {
        Ok(self
            .conn
            .lock()
            .query_row(
                "SELECT data FROM blobs WHERE vault_id = ?1 AND hash = ?2",
                params![vault_id, hash],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn has_blob(&self, vault_id: [u8; 16], hash: &str) -> Result<bool, DbError> {
        let count: i64 = self.conn.lock().query_row(
            "SELECT COUNT(*) FROM blobs WHERE vault_id = ?1 AND hash = ?2",
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
