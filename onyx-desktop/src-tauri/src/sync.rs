//! Sync client plumbing: device identity, per-vault sync configuration,
//! and the blocking HTTP client that talks to an onyx-server.
//!
//! Pairing model (v1): enabling sync generates a random vault id + op key;
//! the pair is shared to other devices as a "sync code" (like a Syncthing
//! device id). Encrypted vaults reuse their own vault key and derive the
//! id from it, so nothing secret is ever written to disk for them. The
//! QR + SAS + HPKE enrollment flow from the plan supersedes this later.

use std::path::Path;

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
        })
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
        let response = request
            .send()
            .map_err(|error| SyncSetupError::Http(error.to_string()))?;
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
            .map_err(|error| SyncSetupError::Http(error.to_string()))?;
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
            .map_err(|error| SyncSetupError::Http(error.to_string()))?;
        Ok(response.status().is_success())
    }

    pub fn put_blob(
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
            .map_err(|error| SyncSetupError::Http(error.to_string()))?;
        if !response.status().is_success() {
            return Err(SyncSetupError::Server(response.status().to_string()));
        }
        Ok(())
    }

    pub fn get_blob(&mut self, vault_id: [u8; 16], hash: &str) -> Result<Vec<u8>, SyncSetupError> {
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
            .map_err(|error| SyncSetupError::Http(error.to_string()))?;
        if !response.status().is_success() {
            return Err(SyncSetupError::Server(response.status().to_string()));
        }
        response
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|error| SyncSetupError::Http(error.to_string()))
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
            .map_err(|error| SyncSetupError::Http(error.to_string()))?;
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
                        loop {
                            if !alive.load(Ordering::Relaxed) {
                                let _ = socket.close(None);
                                return;
                            }
                            match socket.read() {
                                Ok(message) if message.is_text() => {
                                    let _ = wake.try_send(());
                                }
                                Ok(_) => {}
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
        let ops = pushes
            .iter()
            .map(|push| onyx_proto::EncOp {
                doc_id: push.doc_id,
                ciphertext: push.ciphertext.clone(),
            })
            .collect();
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

    // Attachment inbox: deletions from the merged manifest, then missing
    // blobs (downloaded without the lock, stored under it).
    let needed = {
        let mut guard = engine.lock();
        let Some(engine) = guard.as_mut() else {
            return Ok(changed);
        };
        changed.extend(engine.apply_attachment_deletes().map_err(engine_err)?);
        engine.attachments_needed().map_err(engine_err)?
    };
    for (path, blob_hash) in needed {
        let ciphertext = match client.get_blob(vault_id, &blob_hash) {
            Ok(bytes) => bytes,
            Err(error) => {
                // Blob not on the server yet (uploader mid-flight): retry
                // next cycle rather than failing the whole sync.
                tracing::debug!(%error, %path, "blob fetch deferred");
                continue;
            }
        };
        if let Some(engine) = engine.lock().as_mut() {
            match engine.attachment_store(&path, &blob_hash, &ciphertext) {
                Ok(paths) => changed.extend(paths),
                Err(error) => tracing::warn!(%error, %path, "attachment store failed"),
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
