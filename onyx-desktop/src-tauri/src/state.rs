//! Shared app state and the watcher → engine → frontend event pump.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::RecvTimeoutError;
use onyx_core::{CoalescerConfig, VaultEvent, VaultWatcher};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::engine::Engine;

/// Event name the frontend listens on.
pub const VAULT_EVENT: &str = "onyx://vault-event";

/// Debounce for tantivy commits after the last change.
const SEARCH_COMMIT_DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct AppState {
    pub engine: Arc<Mutex<Option<Engine>>>,
    /// Kept alive here; dropped (and its thread stopped) on vault switch.
    watcher: Mutex<Option<VaultWatcher>>,
}

impl AppState {
    /// Close the current vault: watcher stops, engine drops (vault keys
    /// zeroize on drop).
    pub fn lock_vault(&self) {
        *self.watcher.lock() = None;
        *self.engine.lock() = None;
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
enum FrontendEvent {
    Created { path: String },
    Modified { path: String },
    Removed { path: String },
    Bulk,
}

/// Start watching `root`, replacing any previous watcher. Watcher events
/// flow: notify → coalescer → this pump → engine.apply_event → frontend.
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
