//! Shared app state and the watcher → engine → frontend event pump.

#[cfg(desktop)]
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::RecvTimeoutError;
use onyx_core::VaultWatcher;
#[cfg(desktop)]
use onyx_core::{CoalescerConfig, VaultEvent};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::engine::Engine;

/// Event name the frontend listens on.
pub const VAULT_EVENT: &str = "onyx://vault-event";

/// Debounce for tantivy commits after the last change.
#[cfg(desktop)]
const SEARCH_COMMIT_DEBOUNCE: Duration = Duration::from_millis(500);

/// Sync agent status, surfaced to the UI.
#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatusInfo {
    pub enabled: bool,
    /// "idle" | "syncing" | "error".
    pub state: String,
    pub last_error: Option<String>,
    pub last_synced_epoch_secs: Option<u64>,
}

#[derive(Default)]
pub struct AppState {
    pub engine: Arc<Mutex<Option<Engine>>>,
    /// Kept alive here; dropped (and its thread stopped) on vault switch.
    watcher: Mutex<Option<VaultWatcher>>,
    /// Dropping the sender stops the sync agent loop.
    sync_stop: Mutex<Option<crossbeam_channel::Sender<()>>>,
    pub sync_status: Arc<Mutex<SyncStatusInfo>>,
    /// The active sync configuration, kept so the agent can be resumed
    /// after an app pause (mobile background, laptop sleep) without
    /// re-reading vault state.
    pub active_sync: Mutex<Option<crate::sync::SyncConfig>>,
    /// Dropping the sender stops the auto-backup timer.
    backup_stop: Mutex<Option<crossbeam_channel::Sender<()>>>,
    /// AI request log — the "see exactly what left your machine" surface.
    pub ai_log: Arc<crate::ai::AiLog>,
    /// In-flight device enrollment (new-device side).
    pub pending_enroll: Mutex<Option<PendingEnrollment>>,
    /// Web-clipper server + the token the extension must present.
    clipper: Mutex<Option<crate::clipper::Clipper>>,
    pub clipper_token: Mutex<String>,
}

impl AppState {
    /// Start the clipper once (idempotent); returns the token to show the
    /// user. A random token gates writes so only the paired extension can
    /// post clips.
    pub fn ensure_clipper(&self) -> String {
        let mut token_guard = self.clipper_token.lock();
        if token_guard.is_empty() {
            let mut bytes = [0u8; 18];
            getrandom::fill(&mut bytes).expect("OS randomness must be available");
            *token_guard = data_encoding::BASE64URL_NOPAD.encode(&bytes);
        }
        let token = token_guard.clone();
        drop(token_guard);

        let mut clipper_guard = self.clipper.lock();
        if clipper_guard.is_none() {
            match crate::clipper::spawn(token.clone(), Arc::clone(&self.engine)) {
                Ok(clipper) => *clipper_guard = Some(clipper),
                Err(error) => tracing::warn!(%error, "clipper failed to start"),
            }
        }
        token
    }
}

/// New-device enrollment state between begin → wait → confirm.
pub struct PendingEnrollment {
    pub server_url: String,
    pub code: String,
    pub receiver: onyx_crypto::EnrollmentReceiver,
    pub payload: Option<crate::sync::EnrollmentPayload>,
}

impl AppState {
    /// Close the current vault: sync agent and watcher stop, engine drops
    /// (vault keys zeroize on drop).
    pub fn lock_vault(&self) {
        *self.sync_stop.lock() = None;
        *self.backup_stop.lock() = None;
        *self.watcher.lock() = None;
        *self.engine.lock() = None;
        *self.sync_status.lock() = SyncStatusInfo::default();
        *self.active_sync.lock() = None;
    }

    /// Whether the sync agent is currently running.
    pub fn sync_running(&self) -> bool {
        self.sync_stop.lock().is_some()
    }

    /// Stop the sync agent (app going to background / device suspending)
    /// without forgetting the configuration — `resume` restarts it. The
    /// agent and its WS waker exit within ~2s (stop channel + read
    /// timeouts); the fresh connection on resume is deliberate: sockets
    /// that survived a suspend are often half-open and silently dead.
    pub fn pause_sync(&self) {
        if self.sync_stop.lock().take().is_some() {
            let mut status = self.sync_status.lock();
            if status.enabled {
                status.state = "paused".into();
            }
        }
    }
}

impl AppState {
    /// Run `operation` with the open engine, or fail if no vault is open.
    pub fn with_engine<T>(
        &self,
        operation: impl FnOnce(&mut Engine) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut guard = self.engine.lock();
        let engine = guard.as_mut().ok_or("no vault is open")?;
        operation(engine)
    }
}

/// What the frontend receives when the vault changes under it.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[cfg_attr(mobile, allow(dead_code))]
enum FrontendEvent {
    Created { path: String },
    Modified { path: String },
    Removed { path: String },
    Bulk,
}

/// Start watching `root`, replacing any previous watcher. Watcher events
/// flow: notify → coalescer → this pump → engine.apply_event → frontend.
/// Desktop-only: on mobile the app is the sole writer.
#[cfg(desktop)]
pub fn spawn_watcher(
    app: &AppHandle,
    state: &AppState,
    root: &Path,
) -> Result<(), onyx_core::VaultError> {
    // Drop the old watcher first: its channel disconnects and its pump
    // thread exits on the next recv.
    *state.watcher.lock() = None;

    // Encrypted vaults need ciphertext → plaintext name translation.
    let translator = state
        .engine
        .lock()
        .as_ref()
        .and_then(Engine::path_translator);

    let (sender, receiver) = crossbeam_channel::unbounded::<VaultEvent>();
    let watcher =
        VaultWatcher::spawn_translated(root, CoalescerConfig::default(), sender, translator)?;
    *state.watcher.lock() = Some(watcher);

    let engine = Arc::clone(&state.engine);
    let app = app.clone();
    std::thread::Builder::new()
        .name("onyx-event-pump".into())
        .spawn(move || {
            loop {
                match receiver.recv_timeout(SEARCH_COMMIT_DEBOUNCE) {
                    Ok(event) => {
                        let mut guard = engine.lock();
                        let Some(engine) = guard.as_mut() else {
                            return;
                        };
                        match engine.apply_event(&event) {
                            Ok(true) => continue, // our own echo — silent
                            Ok(false) => {}
                            Err(error) => {
                                tracing::error!(?error, ?event, "failed to apply vault event");
                                continue;
                            }
                        }
                        drop(guard);

                        let payload = match &event {
                            VaultEvent::Created(path) => FrontendEvent::Created {
                                path: path.as_str().to_owned(),
                            },
                            VaultEvent::Modified(path) => FrontendEvent::Modified {
                                path: path.as_str().to_owned(),
                            },
                            VaultEvent::Removed(path) => FrontendEvent::Removed {
                                path: path.as_str().to_owned(),
                            },
                            VaultEvent::BulkChange => FrontendEvent::Bulk,
                        };
                        if app.emit(VAULT_EVENT, payload).is_err() {
                            return; // app shutting down
                        }
                    }
                    // Quiet period: flush any pending search commit.
                    Err(RecvTimeoutError::Timeout) => {
                        let mut guard = engine.lock();
                        let Some(engine) = guard.as_mut() else {
                            return;
                        };
                        if let Err(error) = engine.commit_search_if_dirty() {
                            tracing::error!(?error, "search commit failed");
                        }
                    }
                    // Watcher replaced or dropped: this pump is done.
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .map_err(|error| onyx_core::VaultError::Watcher(error.to_string()))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Sync agent
// ---------------------------------------------------------------------------

/// Base interval between sync cycles. (The WebSocket live-push lane will
/// replace most of these polls later.)
const SYNC_INTERVAL: Duration = Duration::from_secs(10);

/// Start the background sync agent for the open vault, replacing any
/// previous agent.
pub fn spawn_sync_agent(
    app: &AppHandle,
    state: &AppState,
    mut client: crate::sync::SyncClient,
    vault_id: [u8; 16],
) {
    let (stop_sender, stop_receiver) = crossbeam_channel::bounded::<()>(0);
    *state.sync_stop.lock() = Some(stop_sender);
    {
        let mut status = state.sync_status.lock();
        status.enabled = true;
        status.state = "syncing".into();
    }

    let engine = Arc::clone(&state.engine);
    let status = Arc::clone(&state.sync_status);
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("onyx-sync-agent".into())
        .spawn(move || {
            // Join once (idempotent); failures surface as status and retry.
            loop {
                match client.join(vault_id) {
                    Ok(()) => break,
                    Err(error) => {
                        let mut info = status.lock();
                        info.state = "error".into();
                        info.last_error = Some(error.to_string());
                        drop(info);
                        match stop_receiver.recv_timeout(SYNC_INTERVAL) {
                            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                            _ => return,
                        }
                    }
                }
            }

            // Live push: wake immediately when the server head advances.
            let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let (wake_sender, wake_receiver) = crossbeam_channel::bounded::<()>(1);
            if let Ok(token) = client.ensure_auth() {
                crate::sync::spawn_ws_waker(
                    client.base_url(),
                    vault_id,
                    token,
                    wake_sender,
                    Arc::clone(&alive),
                );
            }

            loop {
                match crate::sync::sync_cycle(&engine, &mut client, vault_id) {
                    Ok(changed) => {
                        let mut info = status.lock();
                        info.state = "idle".into();
                        info.last_error = None;
                        info.last_synced_epoch_secs = Some(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0),
                        );
                        drop(info);
                        for path in changed {
                            let _ = app.emit(VAULT_EVENT, FrontendEvent::Modified { path });
                        }
                    }
                    // Unreachable server is a normal roaming state, not an
                    // error: no auth reset, no scary status; retry next tick.
                    Err(crate::sync::SyncSetupError::Offline) => {
                        let mut info = status.lock();
                        info.state = "offline".into();
                        info.last_error = None;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "sync cycle failed");
                        client.reset_auth(); // token may be stale (server restart)
                        let mut info = status.lock();
                        info.state = "error".into();
                        info.last_error = Some(error.to_string());
                    }
                }
                crossbeam_channel::select! {
                    recv(stop_receiver) -> _ => {
                        // Stopped (or state dropped): agent + waker are done.
                        alive.store(false, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                    recv(wake_receiver) -> message => {
                        // Live-push nudge → immediate cycle. A closed wake
                        // channel (waker died) falls back to interval polling.
                        if message.is_err() {
                            match stop_receiver.recv_timeout(SYNC_INTERVAL) {
                                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                                _ => {
                                    alive.store(false, std::sync::atomic::Ordering::Relaxed);
                                    return;
                                }
                            }
                        }
                    }
                    default(SYNC_INTERVAL) => {}
                }
            }
        });
}

// ---------------------------------------------------------------------------
// Auto-backup timer
// ---------------------------------------------------------------------------

/// Start the periodic backup timer (interval from the vault's backup
/// config; call only when interval > 0). Runs every destination each tick.
pub fn spawn_backup_timer(state: &AppState, interval_hours: u32) {
    let (stop_sender, stop_receiver) = crossbeam_channel::bounded::<()>(0);
    *state.backup_stop.lock() = Some(stop_sender);
    let engine = Arc::clone(&state.engine);
    let interval = Duration::from_secs(u64::from(interval_hours) * 3600);

    let _ = std::thread::Builder::new()
        .name("onyx-backup-timer".into())
        .spawn(move || {
            loop {
                match stop_receiver.recv_timeout(interval) {
                    Err(RecvTimeoutError::Timeout) => {}
                    _ => return, // stopped or vault switched
                }
                let gathered = {
                    let guard = engine.lock();
                    let Some(engine) = guard.as_ref() else { return };
                    let config = crate::backup::load_config(engine.vault());
                    let key = match crate::backup::backup_key(
                        engine.root(),
                        engine.crypto_key().as_ref(),
                    ) {
                        Ok(key) => key,
                        Err(error) => {
                            tracing::warn!(%error, "backup key unavailable");
                            continue;
                        }
                    };
                    let mut files = Vec::new();
                    let records = engine.index().all_notes().unwrap_or_default();
                    for record in records {
                        if let Ok(content) = engine.vault().read_bytes(&record.path) {
                            files.push((record.path.as_str().to_owned(), content));
                        }
                    }
                    (config, key, files)
                };
                let (config, key, files) = gathered;
                for destination in &config.destinations {
                    match crate::backup::run_backup(&key, &files, destination) {
                        Ok(report) => tracing::info!(
                            destination = %destination.name,
                            uploaded = report.uploaded,
                            skipped = report.skipped,
                            "auto-backup complete"
                        ),
                        Err(error) => tracing::warn!(
                            destination = %destination.name,
                            %error,
                            "auto-backup failed"
                        ),
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_is_idempotent_and_resumable_state() {
        let state = AppState::default();
        assert!(!state.sync_running());

        // Simulate a running agent + stored config.
        let (sender, _receiver) = crossbeam_channel::bounded::<()>(0);
        *state.sync_stop.lock() = Some(sender);
        state.sync_status.lock().enabled = true;
        *state.active_sync.lock() = Some(crate::sync::SyncConfig {
            server_url: "http://example.invalid".into(),
            vault_id: "00".repeat(16),
            key: Some("11".repeat(32)),
        });
        assert!(state.sync_running());

        state.pause_sync();
        assert!(!state.sync_running());
        assert_eq!(state.sync_status.lock().state, "paused");
        // Config survives the pause — that's what resume needs.
        assert!(state.active_sync.lock().is_some());
        // Second pause is a no-op (no status churn, no panic).
        state.pause_sync();
        assert!(!state.sync_running());

        // Locking the vault forgets the sync config entirely.
        state.lock_vault();
        assert!(state.active_sync.lock().is_none());
        assert!(!state.sync_status.lock().enabled);
    }
}
