//! Sync client plumbing: device identity, per-vault sync configuration,
//! and the blocking HTTP client that talks to an onyx-server.
//!
//! Pairing model (v1): enabling sync generates a random vault id + op key;
//! the pair is shared to other devices as a "sync code" (like a Syncthing
//! device id). Encrypted vaults reuse their own vault key and derive the
//! id from it, so nothing secret is ever written to disk for them. The
//! QR + SAS + HPKE enrollment flow from the plan supersedes this later.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use data_encoding::HEXLOWER;
use ed25519_dalek::{Signer, SigningKey};
use onyx_core::{NotePath, Vault};
use onyx_crypto::VaultKey;
use serde::{Deserialize, Serialize};

const SYNC_CONFIG_PATH: &str = ".onyx/sync.json";

#[derive(Debug, thiserror::Error)]
pub enum SyncSetupError {
    #[error("sync request failed: {0}")]
    Http(String),
    #[error("server rejected: {0}")]
    Server(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("invalid sync code")]
    BadCode,
    /// The server is unreachable (no network / airplane mode / server
    /// down). Not an error state for the UI — sync resumes when
    /// connectivity returns.
    #[error("server unreachable")]
    Offline,
}

/// Map a transport error to `Offline` when it's a connectivity failure
/// rather than a protocol/server problem. A `.send()` that never produced a
/// response — connection refused, timeout, or a link that dropped mid-flight
/// (a reset surfaces as a request error) — is roaming, not an error.
fn transport_error(error: reqwest::Error) -> SyncSetupError {
    if error.is_connect() || error.is_timeout() || error.is_request() {
        SyncSetupError::Offline
    } else {
        SyncSetupError::Http(error.to_string())
    }
}

// ---------------------------------------------------------------------------
// Per-vault sync configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfig {
    pub server_url: String,
    pub vault_id: String,
    /// Op-encryption key (hex). Absent for encrypted vaults — they reuse
    /// the vault key, which never touches disk unwrapped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

pub fn load_config(vault: &Vault) -> Option<SyncConfig> {
    let path = NotePath::new(SYNC_CONFIG_PATH).ok()?;
    let bytes = vault.fs().read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn save_config(vault: &Vault, config: &SyncConfig) -> Result<(), SyncSetupError> {
    let path = NotePath::new(SYNC_CONFIG_PATH).expect("static path");
    let json = serde_json::to_vec_pretty(config).expect("config serializes");
    vault.fs().write_atomic(&path, &json)?;
    Ok(())
}

/// The shareable pairing code: hex(vault_id ‖ key).
pub fn sync_code(vault_id: [u8; 16], key: &[u8; 32]) -> String {
    let mut joined = Vec::with_capacity(48);
    joined.extend_from_slice(&vault_id);
    joined.extend_from_slice(key);
    HEXLOWER.encode(&joined)
}

pub fn parse_sync_code(code: &str) -> Result<([u8; 16], [u8; 32]), SyncSetupError> {
    let bytes = HEXLOWER
        .decode(code.trim().as_bytes())
        .map_err(|_| SyncSetupError::BadCode)?;
    if bytes.len() != 48 {
        return Err(SyncSetupError::BadCode);
    }
    let vault_id = bytes[..16].try_into().expect("length checked");
    let key = bytes[16..].try_into().expect("length checked");
    Ok((vault_id, key))
}

/// Derive the (vault_id, op key) pair for an encrypted vault from its own
/// key — deterministic, so every unlocked device agrees with zero storage.
pub fn derive_encrypted_sync_identity(vault_key: &VaultKey) -> ([u8; 16], VaultKey) {
    let id_material = vault_key.derive("onyx-sync 2026-07 vault id v1", &[]);
    let vault_id: [u8; 16] = id_material[..16].try_into().expect("length");
    let op_key = VaultKey::from_bytes(vault_key.derive("onyx-sync 2026-07 op key v1", &[]));
    (vault_id, op_key)
}

// ---------------------------------------------------------------------------
// Device identity
// ---------------------------------------------------------------------------

/// Stable per-installation Ed25519 identity, stored in the app data dir
/// (never inside any vault).
pub struct DeviceIdentity {
    signing: SigningKey,
}

impl DeviceIdentity {
    pub fn load_or_create(path: &Path) -> Result<Self, SyncSetupError> {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice()) {
                return Ok(Self {
                    signing: SigningKey::from_bytes(&seed),
                });
            }
        }
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).expect("OS randomness must be available");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, seed)?;
        Ok(Self {
            signing: SigningKey::from_bytes(&seed),
        })
    }

    /// CRDT peer id: stable, derived from the public key.
    pub fn peer(&self) -> u64 {
        let hash = blake3::hash(self.signing.verifying_key().as_bytes());
        u64::from_le_bytes(hash.as_bytes()[..8].try_into().expect("length"))
    }
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

/// Blocking client for one server; obtains and caches a bearer token via
/// the Ed25519 challenge–response flow.
pub struct SyncClient {
    http: reqwest::blocking::Client,
    base: String,
    device: DeviceIdentity,
    token: Option<String>,
    /// Where partial large-blob downloads are staged so they survive a
    /// dropped connection (and an app restart). `None` falls back to a
    /// single whole-blob GET — correct, just not resumable.
    blob_cache: Option<PathBuf>,
}

impl SyncClient {
    pub fn new(server_url: &str, device: DeviceIdentity) -> Result<Self, SyncSetupError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|error| SyncSetupError::Http(error.to_string()))?;
        Ok(Self {
            http,
            base: server_url.trim_end_matches('/').to_owned(),
            device,
            token: None,
            blob_cache: None,
        })
    }

    /// Stage resumable large-blob downloads under `dir`. Set once when the
    /// sync agent starts for a vault.
    pub fn set_blob_cache(&mut self, dir: PathBuf) {
        self.blob_cache = Some(dir);
    }

    fn post_json(
        &self,
        path: &str,
        body: serde_json::Value,
        token: Option<&str>,
    ) -> Result<serde_json::Value, SyncSetupError> {
        let mut request = self.http.post(format!("{}{path}", self.base)).json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request.send().map_err(transport_error)?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().unwrap_or_default();
            return Err(SyncSetupError::Server(format!("{status}: {text}")));
        }
        response
            .json()
            .map_err(|error| SyncSetupError::Http(error.to_string()))
    }

    /// Register (idempotent) + challenge + verify → cached bearer token.
    pub fn ensure_auth(&mut self) -> Result<String, SyncSetupError> {
        if let Some(token) = &self.token {
            return Ok(token.clone());
        }
        let public_hex = HEXLOWER.encode(self.device.signing.verifying_key().as_bytes());
        let registered = self.post_json(
            "/v1/devices",
            serde_json::json!({ "public_key": public_hex }),
            None,
        )?;
        let device_id = registered["deviceId"]
            .as_str()
            .ok_or_else(|| SyncSetupError::Server("missing deviceId".into()))?
            .to_owned();

        let challenged = self.post_json(
            "/v1/auth/challenge",
            serde_json::json!({ "deviceId": device_id }),
            None,
        )?;
        let challenge_hex = challenged["challenge"]
            .as_str()
            .ok_or_else(|| SyncSetupError::Server("missing challenge".into()))?;
        let challenge = HEXLOWER
            .decode(challenge_hex.as_bytes())
            .map_err(|_| SyncSetupError::Server("bad challenge".into()))?;
        let signature = HEXLOWER.encode(&self.device.signing.sign(&challenge).to_bytes());

        let verified = self.post_json(
            "/v1/auth/verify",
            serde_json::json!({
                "deviceId": device_id,
                "challenge": challenge_hex,
                "signature": signature,
            }),
            None,
        )?;
        let token = verified["token"]
            .as_str()
            .ok_or_else(|| SyncSetupError::Server("missing token".into()))?
            .to_owned();
        self.token = Some(token.clone());
        Ok(token)
    }

    /// Drop the cached token (e.g. after a 401 — server restarted).
    pub fn reset_auth(&mut self) {
        self.token = None;
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn put_share(&mut self, id: &str, blob: Vec<u8>) -> Result<(), SyncSetupError> {
        let token = self.ensure_auth()?;
        let response = self
            .http
            .put(format!("{}/v1/shares/{id}", self.base))
            .bearer_auth(token)
            .body(blob)
            .send()
            .map_err(transport_error)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(SyncSetupError::Server(response.status().to_string()))
        }
    }

    pub fn delete_share(&mut self, id: &str) -> Result<(), SyncSetupError> {
        let token = self.ensure_auth()?;
        let response = self
            .http
            .delete(format!("{}/v1/shares/{id}", self.base))
            .bearer_auth(token)
            .send()
            .map_err(transport_error)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(SyncSetupError::Server(response.status().to_string()))
        }
    }

    /// POST raw bytes; Ok(true) on 2xx, Ok(false) on 409/404 (caller
    /// semantics), Err otherwise.
    pub(crate) fn http_post_bytes(
        &self,
        path: &str,
        body: Vec<u8>,
        token: &str,
    ) -> Result<bool, SyncSetupError> {
        let response = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(token)
            .body(body)
            .send()
            .map_err(transport_error)?;
        let status = response.status();
        if status.is_success() {
            Ok(true)
        } else if status.as_u16() == 409 || status.as_u16() == 404 {
            Ok(false)
        } else {
            Err(SyncSetupError::Server(status.to_string()))
        }
    }

    pub(crate) fn http_get_bytes(
        &self,
        path: &str,
        token: &str,
    ) -> Result<Vec<u8>, SyncSetupError> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(SyncSetupError::Server(response.status().to_string()));
        }
        response
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|error| SyncSetupError::Http(error.to_string()))
    }

    pub fn join(&mut self, vault_id: [u8; 16]) -> Result<(), SyncSetupError> {
        let token = self.ensure_auth()?;
        self.post_json(
            "/v1/vaults",
            serde_json::json!({ "vaultId": HEXLOWER.encode(&vault_id) }),
            Some(&token),
        )?;
        Ok(())
    }

    pub fn push(
        &mut self,
        vault_id: [u8; 16],
        ops: Vec<onyx_proto::EncOp>,
    ) -> Result<u64, SyncSetupError> {
        let token = self.ensure_auth()?;
        let body = onyx_proto::encode(&onyx_proto::PushOps {
            version: onyx_proto::PROTOCOL_VERSION,
            ops,
        })
        .map_err(|error| SyncSetupError::Http(error.to_string()))?;
        let response = self
            .http
            .post(format!(
                "{}/v1/vaults/{}/ops",
                self.base,
                HEXLOWER.encode(&vault_id)
            ))
            .bearer_auth(token)
            .body(body)
            .send()
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(SyncSetupError::Server(response.status().to_string()));
        }
        let bytes = response
            .bytes()
            .map_err(|error| SyncSetupError::Http(error.to_string()))?;
        let ack: onyx_proto::PushAck = onyx_proto::decode(&bytes)
            .map_err(|error| SyncSetupError::Server(error.to_string()))?;
        Ok(ack.head_seq)
    }

    pub fn has_blob(&mut self, vault_id: [u8; 16], hash: &str) -> Result<bool, SyncSetupError> {
        let token = self.ensure_auth()?;
        let response = self
            .http
            .head(format!(
                "{}/v1/vaults/{}/blobs/{hash}",
                self.base,
                HEXLOWER.encode(&vault_id)
            ))
            .bearer_auth(token)
            .send()
            .map_err(transport_error)?;
        Ok(response.status().is_success())
    }

    /// Upload an encrypted blob. Small blobs go in one request; large ones
    /// are chunked so a dropped connection loses at most one chunk — the
    /// next attempt queries the server and re-sends only what's missing.
    pub fn put_blob(
        &mut self,
        vault_id: [u8; 16],
        hash: &str,
        ciphertext: Vec<u8>,
    ) -> Result<(), SyncSetupError> {
        if ciphertext.len() <= onyx_proto::BLOB_CHUNK_BYTES {
            return self.put_blob_whole(vault_id, hash, ciphertext);
        }
        self.put_blob_chunked(vault_id, hash, &ciphertext)
    }

    fn put_blob_whole(
        &mut self,
        vault_id: [u8; 16],
        hash: &str,
        ciphertext: Vec<u8>,
    ) -> Result<(), SyncSetupError> {
        let token = self.ensure_auth()?;
        let response = self
            .http
            .put(format!(
                "{}/v1/vaults/{}/blobs/{hash}",
                self.base,
                HEXLOWER.encode(&vault_id)
            ))
            .bearer_auth(token)
            .body(ciphertext)
            .send()
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(SyncSetupError::Server(response.status().to_string()));
        }
        Ok(())
    }

    fn put_blob_chunked(
        &mut self,
        vault_id: [u8; 16],
        hash: &str,
        ciphertext: &[u8],
    ) -> Result<(), SyncSetupError> {
        let chunk = onyx_proto::BLOB_CHUNK_BYTES;
        let total = ciphertext.len().div_ceil(chunk) as u32;
        let size = ciphertext.len() as u64;

        // Resume: skip chunks the server already holds (or bail if the whole
        // blob is already there, e.g. a peer uploaded it or a prior attempt
        // finished after its ack was lost).
        let already: HashSet<u32> = match self.blob_status(vault_id, hash)? {
            Some(status) if status.complete => return Ok(()),
            Some(status) => status.present.into_iter().collect(),
            None => HashSet::new(),
        };

        let token = self.ensure_auth()?;
        let vault_hex = HEXLOWER.encode(&vault_id);
        for idx in 0..total {
            if already.contains(&idx) {
                continue;
            }
            let start = idx as usize * chunk;
            let end = (start + chunk).min(ciphertext.len());
            let response = self
                .http
                .put(format!(
                    "{}/v1/vaults/{vault_hex}/blobs/{hash}/chunks/{idx}?total={total}&size={size}",
                    self.base,
                ))
                .bearer_auth(&token)
                .body(ciphertext[start..end].to_vec())
                .send()
                .map_err(transport_error)?;
            if !response.status().is_success() {
                return Err(SyncSetupError::Server(response.status().to_string()));
            }
        }
        // The server completes and hash-verifies on the final chunk; confirm
        // before we let the pointer ship.
        if !self.has_blob(vault_id, hash)? {
            return Err(SyncSetupError::Server("blob did not complete".into()));
        }
        Ok(())
    }

    /// Resume status for a chunked upload; `None` if never begun.
    fn blob_status(
        &mut self,
        vault_id: [u8; 16],
        hash: &str,
    ) -> Result<Option<onyx_proto::BlobStatus>, SyncSetupError> {
        let token = self.ensure_auth()?;
        let response = self
            .http
            .get(format!(
                "{}/v1/vaults/{}/blobs/{hash}/status",
                self.base,
                HEXLOWER.encode(&vault_id)
            ))
            .bearer_auth(token)
            .send()
            .map_err(transport_error)?;
        if response.status().as_u16() == 404 {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(SyncSetupError::Server(response.status().to_string()));
        }
        let bytes = response
            .bytes()
            .map_err(|error| SyncSetupError::Http(error.to_string()))?;
        onyx_proto::decode(&bytes)
            .map(Some)
            .map_err(|error| SyncSetupError::Server(error.to_string()))
    }

    /// The size of a complete blob (HEAD), or `None` if absent/incomplete.
    fn blob_head_size(
        &mut self,
        vault_id: [u8; 16],
        hash: &str,
    ) -> Result<Option<u64>, SyncSetupError> {
        let token = self.ensure_auth()?;
        let response = self
            .http
            .head(format!(
                "{}/v1/vaults/{}/blobs/{hash}",
                self.base,
                HEXLOWER.encode(&vault_id)
            ))
            .bearer_auth(token)
            .send()
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Ok(None);
        }
        Ok(response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.parse().ok()))
    }

    /// Download an encrypted blob. Large blobs stream through range requests
    /// into a `.part` file in the blob cache, so a dropped connection resumes
    /// from the last byte received instead of restarting. The final bytes are
    /// hash-verified before use.
    pub fn get_blob(&mut self, vault_id: [u8; 16], hash: &str) -> Result<Vec<u8>, SyncSetupError> {
        let size = self.blob_head_size(vault_id, hash)?;
        let cache = self.blob_cache.clone();
        match (size, cache) {
            (Some(size), Some(cache)) if size > onyx_proto::BLOB_CHUNK_BYTES as u64 => {
                self.get_blob_resumable(vault_id, hash, size, &cache)
            }
            _ => self.get_blob_whole(vault_id, hash),
        }
    }

    fn get_blob_whole(
        &mut self,
        vault_id: [u8; 16],
        hash: &str,
    ) -> Result<Vec<u8>, SyncSetupError> {
        let token = self.ensure_auth()?;
        let response = self
            .http
            .get(format!(
                "{}/v1/vaults/{}/blobs/{hash}",
                self.base,
                HEXLOWER.encode(&vault_id)
            ))
            .bearer_auth(token)
            .send()
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(SyncSetupError::Server(response.status().to_string()));
        }
        response
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|error| SyncSetupError::Http(error.to_string()))
    }

    fn get_blob_resumable(
        &mut self,
        vault_id: [u8; 16],
        hash: &str,
        size: u64,
        cache: &Path,
    ) -> Result<Vec<u8>, SyncSetupError> {
        let chunk = onyx_proto::BLOB_CHUNK_BYTES as u64;
        std::fs::create_dir_all(cache)?;
        let part = cache.join(format!("{hash}.part"));

        // Resume from whatever's already on disk; a `.part` longer than the
        // blob is stale — start over.
        let mut have = std::fs::metadata(&part).map(|meta| meta.len()).unwrap_or(0);
        if have > size {
            std::fs::remove_file(&part)?;
            have = 0;
        }

        let token = self.ensure_auth()?;
        let vault_hex = HEXLOWER.encode(&vault_id);
        while have < size {
            let last = (have + chunk - 1).min(size - 1);
            let response = self
                .http
                .get(format!(
                    "{}/v1/vaults/{vault_hex}/blobs/{hash}",
                    self.base,
                ))
                .bearer_auth(&token)
                .header(reqwest::header::RANGE, format!("bytes={have}-{last}"))
                .send()
                .map_err(transport_error)?;
            if !response.status().is_success() {
                return Err(SyncSetupError::Server(response.status().to_string()));
            }
            let bytes = response
                .bytes()
                .map_err(|error| SyncSetupError::Http(error.to_string()))?;
            if bytes.is_empty() {
                break;
            }
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&part)?;
            file.write_all(&bytes)?;
            have += bytes.len() as u64;
        }

        let data = std::fs::read(&part)?;
        // Integrity gate: a corrupt/truncated assembly is discarded so the
        // next cycle re-fetches cleanly rather than materializing garbage.
        if blake3::hash(&data).to_hex().as_str() != hash {
            std::fs::remove_file(&part)?;
            return Err(SyncSetupError::Server("blob hash mismatch".into()));
        }
        std::fs::remove_file(&part)?;
        Ok(data)
    }

    pub fn pull(
        &mut self,
        vault_id: [u8; 16],
        since: u64,
    ) -> Result<onyx_proto::OpsBatch, SyncSetupError> {
        let token = self.ensure_auth()?;
        let response = self
            .http
            .get(format!(
                "{}/v1/vaults/{}/ops?since={since}",
                self.base,
                HEXLOWER.encode(&vault_id)
            ))
            .bearer_auth(token)
            .send()
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(SyncSetupError::Server(response.status().to_string()));
        }
        let bytes = response
            .bytes()
            .map_err(|error| SyncSetupError::Http(error.to_string()))?;
        onyx_proto::decode(&bytes).map_err(|error| SyncSetupError::Server(error.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Live-push waker
// ---------------------------------------------------------------------------

/// Connect to the server's live-push WebSocket and signal `wake` whenever
/// the vault head advances. Reconnects with backoff; exits when `alive`
/// clears. Runs on its own thread (blocking tungstenite).
pub fn spawn_ws_waker(
    server_url: &str,
    vault_id: [u8; 16],
    token: String,
    wake: crossbeam_channel::Sender<()>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    use std::sync::atomic::Ordering;

    let ws_base = if let Some(rest) = server_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = server_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{server_url}")
    };
    let url = format!(
        "{}/v1/vaults/{}/ws",
        ws_base.trim_end_matches('/'),
        HEXLOWER.encode(&vault_id)
    );

    let _ = std::thread::Builder::new()
        .name("onyx-ws-waker".into())
        .spawn(move || {
            while alive.load(Ordering::Relaxed) {
                match connect_ws(&url, &token) {
                    Ok(mut socket) => {
                        tracing::debug!("live-push connected");
                        // Liveness: read timeouts alone can't distinguish a
                        // quiet healthy socket from a half-open dead one
                        // (device suspend, network migration). Ping every
                        // PING_INTERVAL; no traffic (incl. pong) within
                        // DEAD_AFTER ⇒ reconnect.
                        const PING_INTERVAL: std::time::Duration =
                            std::time::Duration::from_secs(30);
                        const DEAD_AFTER: std::time::Duration = std::time::Duration::from_secs(90);
                        let mut last_activity = std::time::Instant::now();
                        let mut last_ping = std::time::Instant::now();
                        loop {
                            if !alive.load(Ordering::Relaxed) {
                                let _ = socket.close(None);
                                return;
                            }
                            if last_ping.elapsed() >= PING_INTERVAL {
                                last_ping = std::time::Instant::now();
                                if socket
                                    .send(tungstenite::Message::Ping(Vec::new().into()))
                                    .is_err()
                                {
                                    break; // send failure: reconnect
                                }
                            }
                            if last_activity.elapsed() >= DEAD_AFTER {
                                tracing::debug!("live-push socket silent too long; reconnecting");
                                break;
                            }
                            match socket.read() {
                                Ok(message) if message.is_text() => {
                                    last_activity = std::time::Instant::now();
                                    let _ = wake.try_send(());
                                }
                                Ok(_) => {
                                    // Pongs (and any other frame) prove life.
                                    last_activity = std::time::Instant::now();
                                }
                                Err(tungstenite::Error::Io(error))
                                    if matches!(
                                        error.kind(),
                                        std::io::ErrorKind::WouldBlock
                                            | std::io::ErrorKind::TimedOut
                                    ) =>
                                {
                                    continue; // read timeout: liveness check tick
                                }
                                Err(_) => break, // reconnect
                            }
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%error, "live-push connect failed; will retry");
                    }
                }
                // Backoff before reconnecting, staying responsive to stop.
                for _ in 0..10 {
                    if !alive.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        });
}

type WsSocket = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

fn connect_ws(url: &str, token: &str) -> Result<WsSocket, Box<tungstenite::Error>> {
    use tungstenite::client::IntoClientRequest;

    let mut request = url.into_client_request().map_err(Box::new)?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .expect("token is valid ASCII"),
    );
    let (socket, _response) = tungstenite::connect(request).map_err(Box::new)?;
    // A short read timeout lets the loop notice shutdown promptly.
    match socket.get_ref() {
        tungstenite::stream::MaybeTlsStream::Plain(stream) => {
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
        }
        tungstenite::stream::MaybeTlsStream::Rustls(tls) => {
            let _ = tls
                .get_ref()
                .set_read_timeout(Some(std::time::Duration::from_secs(2)));
        }
        _ => {}
    }
    Ok(socket)
}

// ---------------------------------------------------------------------------
// The sync cycle (shared by the background agent and tests)
// ---------------------------------------------------------------------------

/// Map an engine push to a wire op (carrying its idempotency id and
/// checkpoint flag).
fn pending_to_encop(push: &crate::engine::PendingPush) -> onyx_proto::EncOp {
    onyx_proto::EncOp {
        doc_id: push.doc_id,
        op_id: push.op_id,
        ciphertext: push.ciphertext.clone(),
        checkpoint: push.checkpoint,
    }
}

/// One push/pull round trip. Returns the vault paths changed by remote ops.
pub fn sync_cycle(
    engine: &parking_lot::Mutex<Option<crate::engine::Engine>>,
    client: &mut SyncClient,
    vault_id: [u8; 16],
) -> Result<Vec<String>, SyncSetupError> {
    let engine_err = |error: crate::engine::EngineError| SyncSetupError::Server(error.to_string());

    // Attachment outbox: encrypt under the lock, transfer without it.
    let uploads = {
        let mut guard = engine.lock();
        let Some(engine) = guard.as_mut() else {
            return Ok(Vec::new());
        };
        engine.attachments_to_upload().map_err(engine_err)?
    };
    if !uploads.is_empty() {
        for upload in &uploads {
            if !client.has_blob(vault_id, &upload.blob_hash)? {
                client.put_blob(vault_id, &upload.blob_hash, upload.ciphertext.clone())?;
            }
        }
        if let Some(engine) = engine.lock().as_mut() {
            engine
                .attachments_mark_uploaded(&uploads)
                .map_err(engine_err)?;
        }
    }

    // Outbox: collect under the lock, push over the network WITHOUT it.
    let pushes = {
        let mut guard = engine.lock();
        let Some(engine) = guard.as_mut() else {
            return Ok(Vec::new()); // vault closed mid-cycle
        };
        engine.sync_collect().map_err(engine_err)?
    };
    if !pushes.is_empty() {
        let ops = pushes.iter().map(pending_to_encop).collect();
        client.push(vault_id, ops)?;
        if let Some(engine) = engine.lock().as_mut() {
            engine.sync_mark_pushed(&pushes).map_err(engine_err)?;
        }
    }

    // Inbox.
    let cursor = match engine.lock().as_ref() {
        Some(engine) => engine.sync_cursor(),
        None => return Ok(Vec::new()),
    };
    let batch = client.pull(vault_id, cursor)?;
    let mut changed = Vec::new();
    if let Some(last) = batch.ops.last().map(|op| op.seq) {
        let mut guard = engine.lock();
        let Some(engine) = guard.as_mut() else {
            return Ok(Vec::new());
        };
        changed = engine.sync_apply_remote(&batch.ops).map_err(engine_err)?;
        engine.set_sync_cursor(last).map_err(engine_err)?;
    }

    // Checkpoint pass: the server asks (via pull hints) for a full-state
    // checkpoint of any doc whose op log has grown long, so it can prune the
    // backlog. Push them idempotently; a lost ack just retries next cycle.
    if !batch.checkpoint_hints.is_empty() {
        let checkpoints = {
            let mut guard = engine.lock();
            let Some(engine) = guard.as_mut() else {
                return Ok(changed);
            };
            engine
                .sync_collect_checkpoints(&batch.checkpoint_hints)
                .map_err(engine_err)?
        };
        if !checkpoints.is_empty() {
            let ops = checkpoints.iter().map(pending_to_encop).collect();
            client.push(vault_id, ops)?;
            if let Some(engine) = engine.lock().as_mut() {
                engine.sync_mark_pushed(&checkpoints).map_err(engine_err)?;
            }
        }
    }

    // Attachment inbox: pointer docs merged during apply; fetch winners
    // (and concurrent losers as keep-both copies), then collapse resolved
    // conflicts. Deletions already rode the manifest tombstones.
    let needed = {
        let mut guard = engine.lock();
        let Some(engine) = guard.as_mut() else {
            return Ok(changed);
        };
        engine.attachments_needed().map_err(engine_err)?
    };
    for fetch in needed {
        let ciphertext = match client.get_blob(vault_id, &fetch.winner) {
            Ok(bytes) => bytes,
            Err(error) => {
                // Blob not on the server yet (uploader mid-flight): retry
                // next cycle rather than failing the whole sync.
                tracing::debug!(%error, path = %fetch.path, "blob fetch deferred");
                continue;
            }
        };
        if let Some(engine) = engine.lock().as_mut() {
            match engine.attachment_store(&fetch.path, &fetch.winner, &ciphertext) {
                Ok(paths) => changed.extend(paths),
                Err(error) => {
                    tracing::warn!(%error, path = %fetch.path, "attachment store failed");
                    continue;
                }
            }
        }
        // Keep-both: every concurrent loser materializes as an identical
        // conflict copy on every device, then the doc collapses.
        let mut all_losers_stored = true;
        for loser in &fetch.losers {
            let ciphertext = match client.get_blob(vault_id, loser) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::debug!(%error, "loser blob fetch deferred");
                    all_losers_stored = false;
                    continue;
                }
            };
            if let Some(engine) = engine.lock().as_mut() {
                match engine.attachment_store_conflict(&fetch.path, loser, &ciphertext) {
                    Ok(Some(conflict)) => changed.push(conflict),
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%error, "conflict copy failed");
                        all_losers_stored = false;
                    }
                }
            }
        }
        if !fetch.losers.is_empty() && all_losers_stored {
            if let Some(engine) = engine.lock().as_mut() {
                if let Err(error) = engine.attachment_collapse(&fetch.path, &fetch.winner) {
                    tracing::warn!(%error, "conflict collapse failed");
                }
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_code_roundtrip() {
        let code = sync_code([7; 16], &[9; 32]);
        assert_eq!(code.len(), 96);
        let (vault_id, key) = parse_sync_code(&code).unwrap();
        assert_eq!(vault_id, [7; 16]);
        assert_eq!(key, [9; 32]);
        assert!(parse_sync_code("junk").is_err());
        assert!(parse_sync_code(&"ab".repeat(40)).is_err());
    }

    #[test]
    fn encrypted_identity_is_deterministic_and_distinct() {
        let key = VaultKey::from_bytes([3; 32]);
        let (id_a, _op_a) = derive_encrypted_sync_identity(&key);
        let (id_b, _op_b) = derive_encrypted_sync_identity(&key);
        assert_eq!(id_a, id_b);
        let other = VaultKey::from_bytes([4; 32]);
        let (id_c, _) = derive_encrypted_sync_identity(&other);
        assert_ne!(id_a, id_c);
    }

    #[test]
    fn device_identity_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.key");
        let first = DeviceIdentity::load_or_create(&path).unwrap();
        let second = DeviceIdentity::load_or_create(&path).unwrap();
        assert_eq!(first.peer(), second.peer());
    }
}

// ---------------------------------------------------------------------------
// Device enrollment (pairing): sealed-box handoff of the sync identity,
// verified by a SAS the user compares on both screens.
// ---------------------------------------------------------------------------

/// What enrollment transfers: the sync identity only. At-rest encryption
/// stays per-device (each vault has its own local key), so any vault type
/// can enroll any other.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct EnrollmentPayload {
    pub vault_id: [u8; 16],
    pub op_key: [u8; 32],
}

/// New-device side, step 1: publish a request under a fresh short code.
/// Returns `(code, receiver)` — hold the receiver for the claim step.
pub fn enroll_begin(
    client: &mut SyncClient,
) -> Result<(String, onyx_crypto::EnrollmentReceiver), SyncSetupError> {
    let receiver = onyx_crypto::EnrollmentReceiver::generate();
    let mut code_bytes = [0u8; 5];
    getrandom::fill(&mut code_bytes).expect("OS randomness must be available");
    let code = data_encoding::BASE32_NOPAD
        .encode(&code_bytes)
        .to_lowercase();

    let token = client.ensure_auth()?;
    let response = client.http_post_bytes(
        &format!("/v1/enroll/{code}"),
        receiver.public().to_vec(),
        &token,
    )?;
    if !response {
        return Err(SyncSetupError::Server("enrollment code collision".into()));
    }
    Ok((code, receiver))
}

/// Existing-device side: fetch the request, seal our sync identity to it,
/// publish the response. Returns the SAS to display.
pub fn enroll_approve(
    client: &mut SyncClient,
    code: &str,
    vault_id: [u8; 16],
    op_key: &[u8; 32],
) -> Result<String, SyncSetupError> {
    let token = client.ensure_auth()?;
    let receiver_pub_bytes = client.http_get_bytes(
        &format!("/v1/enroll/{}", code.trim().to_lowercase()),
        &token,
    )?;
    let receiver_pub: [u8; 32] = receiver_pub_bytes
        .try_into()
        .map_err(|_| SyncSetupError::Server("malformed enrollment request".into()))?;

    let payload = postcard::to_allocvec(&EnrollmentPayload {
        vault_id,
        op_key: *op_key,
    })
    .map_err(|error| SyncSetupError::Server(error.to_string()))?;
    let message = onyx_crypto::seal_enrollment(&receiver_pub, &payload);
    let sas = onyx_crypto::sas_code(&receiver_pub, &message);

    let stored = client.http_post_bytes(
        &format!("/v1/enroll/{}/response", code.trim().to_lowercase()),
        message,
        &token,
    )?;
    if !stored {
        return Err(SyncSetupError::Server("enrollment already answered".into()));
    }
    Ok(sas)
}

/// New-device side, step 2: poll for the sealed response; on arrival,
/// open it and return `(payload, sas)` — the caller shows the SAS and
/// applies the config only after the user confirms it matches.
pub fn enroll_claim(
    client: &mut SyncClient,
    code: &str,
    receiver: &onyx_crypto::EnrollmentReceiver,
    timeout: std::time::Duration,
) -> Result<(EnrollmentPayload, String), SyncSetupError> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let token = client.ensure_auth()?;
        match client.http_get_bytes(&format!("/v1/enroll/{code}/response"), &token) {
            Ok(message) => {
                let sas = onyx_crypto::sas_code(&receiver.public(), &message);
                let payload_bytes = onyx_crypto::open_enrollment(receiver, &message)
                    .map_err(|error| SyncSetupError::Server(error.to_string()))?;
                let payload: EnrollmentPayload = postcard::from_bytes(&payload_bytes)
                    .map_err(|error| SyncSetupError::Server(error.to_string()))?;
                return Ok((payload, sas));
            }
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod offline_tests {
    use super::*;

    /// An unreachable server must classify as Offline (roaming state), not
    /// as an error the UI alarms on.
    #[test]
    fn unreachable_server_is_offline_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::load_or_create(&dir.path().join("device.key")).unwrap();
        // Port 9 (discard) on localhost: connection refused immediately.
        let mut client = SyncClient::new("http://127.0.0.1:9", identity).unwrap();
        match client.pull([0; 16], 0) {
            Err(SyncSetupError::Offline) => {}
            other => panic!("expected Offline, got {other:?}"),
        }
        match client.ensure_auth() {
            Err(SyncSetupError::Offline) => {}
            other => panic!("expected Offline, got {:?}", other.map(|_| "token")),
        }
    }
}
