//! Encrypted backups to user-selected destinations (local dir,
//! S3-compatible, WebDAV) via OpenDAL.
//!
//! Model (restic-inspired, simplified to per-file objects for v1):
//!
//! - Every file encrypts **convergently** → same content, same ciphertext →
//!   the content-addressed `chunks/<hash>` object dedupes across snapshots
//!   and unchanged files skip upload entirely.
//! - A snapshot is one encrypted manifest (`snapshots/<unix_ts>.onyxsnap`)
//!   listing `path → chunk hash`. Old snapshots keep working because their
//!   chunks are never rewritten.
//! - Restore is **standalone**: destination + backup key rebuild the vault
//!   with no server and no prior state — the disaster-recovery path.
//!
//! Keys: encrypted vaults derive the backup key from the vault key (nothing
//! stored); plaintext vaults keep a generated key at `.onyx/backup.key`
//! (same trust domain as the notes beside it).

use std::collections::HashMap;
use std::path::Path;

use onyx_core::NotePath;
use onyx_crypto::VaultKey;
use serde::{Deserialize, Serialize};

const BACKUP_CONFIG_PATH: &str = ".onyx/backups.json";
const BACKUP_KEY_PATH: &str = ".onyx/backup.key";

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("backup storage error: {0}")]
    Storage(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt snapshot: {0}")]
    Corrupt(String),
    #[error("unknown destination kind: {0}")]
    UnknownKind(String),
    #[error("destination misconfigured: missing {0}")]
    MissingConfig(&'static str),
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupDestination {
    pub name: String,
    /// "fs" | "s3" | "webdav".
    pub kind: String,
    /// Kind-specific settings (fs: root; s3: bucket, region, endpoint,
    /// accessKeyId, secretAccessKey; webdav: endpoint, username, password).
    #[serde(default)]
    pub config: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BackupConfig {
    pub destinations: Vec<BackupDestination>,
    /// Automatic backup interval in hours; 0 disables.
    pub auto_interval_hours: u32,
}

pub fn load_config(vault: &onyx_core::Vault) -> BackupConfig {
    NotePath::new(BACKUP_CONFIG_PATH)
        .ok()
        .and_then(|path| vault.fs().read(&path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_config(vault: &onyx_core::Vault, config: &BackupConfig) -> Result<(), BackupError> {
    let path = NotePath::new(BACKUP_CONFIG_PATH).expect("static path");
    let json = serde_json::to_vec_pretty(config).expect("config serializes");
    vault.fs().write_atomic(&path, &json)?;
    Ok(())
}

/// The backup encryption key for a vault.
pub fn backup_key(root: &Path, crypto_key: Option<&VaultKey>) -> Result<VaultKey, BackupError> {
    if let Some(vault_key) = crypto_key {
        // Encrypted vault: derive — nothing secret ever stored.
        return Ok(VaultKey::from_bytes(
            vault_key.derive("onyx-backup 2026-07 backup key v1", &[]),
        ));
    }
    let key_path = root.join(BACKUP_KEY_PATH);
    if let Ok(bytes) = std::fs::read(&key_path) {
        if let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice()) {
            return Ok(VaultKey::from_bytes(seed));
        }
    }
    let key = VaultKey::generate();
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let fresh = key.derive("onyx-backup 2026-07 stored key v1", &[]);
    std::fs::write(&key_path, fresh)?;
    Ok(VaultKey::from_bytes(fresh))
}

// ---------------------------------------------------------------------------
// OpenDAL operator
// ---------------------------------------------------------------------------

fn operator(destination: &BackupDestination) -> Result<opendal::Operator, BackupError> {
    let get = |key: &'static str| -> Result<&str, BackupError> {
        destination
            .config
            .get(key)
            .map(String::as_str)
            .ok_or(BackupError::MissingConfig(key))
    };
    let operator = match destination.kind.as_str() {
        "fs" => {
            let builder = opendal::services::Fs::default().root(get("root")?);
            opendal::Operator::new(builder)
                .map_err(|error| BackupError::Storage(error.to_string()))?
                .finish()
        }
        "s3" => {
            let mut builder = opendal::services::S3::default()
                .bucket(get("bucket")?)
                .region(
                    destination
                        .config
                        .get("region")
                        .map_or("auto", String::as_str),
                )
                .access_key_id(get("accessKeyId")?)
                .secret_access_key(get("secretAccessKey")?);
            if let Some(endpoint) = destination.config.get("endpoint") {
                builder = builder.endpoint(endpoint);
            }
            opendal::Operator::new(builder)
                .map_err(|error| BackupError::Storage(error.to_string()))?
                .finish()
        }
        "webdav" => {
            let mut builder = opendal::services::Webdav::default().endpoint(get("endpoint")?);
            if let Some(username) = destination.config.get("username") {
                builder = builder.username(username);
            }
            if let Some(password) = destination.config.get("password") {
                builder = builder.password(password);
            }
            opendal::Operator::new(builder)
                .map_err(|error| BackupError::Storage(error.to_string()))?
                .finish()
        }
        other => return Err(BackupError::UnknownKind(other.to_owned())),
    };
    Ok(operator)
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime builds")
        .block_on(future)
}

// ---------------------------------------------------------------------------
// Snapshot format
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotManifest {
    version: u16,
    created_unix: u64,
    entries: Vec<SnapshotEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotEntry {
    path: String,
    chunk: String,
    size: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupReport {
    pub snapshot_id: u64,
    pub files: usize,
    pub uploaded: usize,
    pub skipped: usize,
    pub bytes_uploaded: u64,
}

// ---------------------------------------------------------------------------
// Backup / list / restore
// ---------------------------------------------------------------------------

/// Run one backup of `files` (path → plaintext content) to `destination`.
/// The caller gathers file content (it owns vault locking); this function
/// owns encryption and transfer.
pub fn run_backup(
    key: &VaultKey,
    files: &[(String, Vec<u8>)],
    destination: &BackupDestination,
) -> Result<BackupReport, BackupError> {
    let operator = operator(destination)?;
    let storage = |error: opendal::Error| BackupError::Storage(error.to_string());

    block_on(async move {
        let mut uploaded = 0usize;
        let mut skipped = 0usize;
        let mut bytes_uploaded = 0u64;
        let mut entries = Vec::with_capacity(files.len());

        for (path, content) in files {
            let ciphertext = onyx_crypto::encrypt_convergent(key, content);
            let chunk = blake3::hash(&ciphertext).to_hex().to_string();
            let object = format!("chunks/{chunk}");
            if operator.exists(&object).await.map_err(storage)? {
                skipped += 1;
            } else {
                bytes_uploaded += ciphertext.len() as u64;
                operator.write(&object, ciphertext).await.map_err(storage)?;
                uploaded += 1;
            }
            entries.push(SnapshotEntry {
                path: path.clone(),
                chunk,
                size: content.len() as u64,
            });
        }

        // Millisecond ids: multiple snapshots per second must not collide.
        let snapshot_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        let manifest = SnapshotManifest {
            version: 1,
            created_unix: snapshot_id,
            entries,
        };
        let manifest_json = serde_json::to_vec(&manifest)
            .map_err(|error| BackupError::Corrupt(error.to_string()))?;
        let manifest_ct = onyx_crypto::encrypt(key, &manifest_json);
        operator
            .write(&format!("snapshots/{snapshot_id}.onyxsnap"), manifest_ct)
            .await
            .map_err(storage)?;

        Ok(BackupReport {
            snapshot_id,
            files: files.len(),
            uploaded,
            skipped,
            bytes_uploaded,
        })
    })
}

/// Snapshot ids available at a destination, newest first.
pub fn list_snapshots(destination: &BackupDestination) -> Result<Vec<u64>, BackupError> {
    let operator = operator(destination)?;
    block_on(async move {
        let listing = operator
            .list("snapshots/")
            .await
            .map_err(|error| BackupError::Storage(error.to_string()))?;
        let mut ids: Vec<u64> = listing
            .iter()
            .filter_map(|entry| {
                entry
                    .name()
                    .strip_suffix(".onyxsnap")
                    .and_then(|stem| stem.parse().ok())
            })
            .collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        Ok(ids)
    })
}

/// Disaster recovery: rebuild a vault from a snapshot into `target_dir`.
/// Needs only the destination and the backup key — no server, no state.
pub fn restore(
    key: &VaultKey,
    destination: &BackupDestination,
    snapshot_id: u64,
    target_dir: &Path,
) -> Result<usize, BackupError> {
    let operator = operator(destination)?;
    let storage = |error: opendal::Error| BackupError::Storage(error.to_string());

    let manifest: SnapshotManifest = block_on(async {
        let ciphertext = operator
            .read(&format!("snapshots/{snapshot_id}.onyxsnap"))
            .await
            .map_err(storage)?
            .to_vec();
        let plaintext = onyx_crypto::decrypt(key, &ciphertext)
            .map_err(|error| BackupError::Corrupt(error.to_string()))?;
        serde_json::from_slice(&plaintext).map_err(|error| BackupError::Corrupt(error.to_string()))
    })?;

    let mut restored = 0usize;
    for entry in &manifest.entries {
        // Snapshot paths were valid vault paths at backup time; re-validate
        // so a tampered manifest can't escape the target directory.
        let Ok(note_path) = NotePath::new(&entry.path) else {
            return Err(BackupError::Corrupt(format!("bad path: {}", entry.path)));
        };
        let content = block_on(async {
            let ciphertext = operator
                .read(&format!("chunks/{}", entry.chunk))
                .await
                .map_err(storage)?
                .to_vec();
            onyx_crypto::decrypt(key, &ciphertext)
                .map_err(|error| BackupError::Corrupt(error.to_string()))
        })?;
        let target = target_dir.join(note_path.as_str());
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, content)?;
        restored += 1;
    }
    Ok(restored)
}
