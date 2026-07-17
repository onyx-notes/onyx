//! OS keychain storage for secrets (AI API keys), with a graceful
//! fallback when no keychain backend exists (headless Linux/CI).
//!
//! The AI config's plaintext fields stay in the app-data JSON for
//! non-secret settings (provider, base URL, model); the API key alone is
//! redirected here so it never sits in a readable file on disk when a
//! keychain is available. Fallback (no Secret Service, etc.) keeps the old
//! behavior so the app still works — documented, not silent.

const SERVICE: &str = "dev.onyx.app";

// Mobile: iOS Keychain / Android Keystore via the onyx-secrets plugin.
// The facade is free functions (callers predate the plugin), so the app
// handle is stashed once at setup.
#[cfg(mobile)]
mod mobile {
    use std::sync::OnceLock;

    use tauri_plugin_onyx_secrets::OnyxSecretsExt;

    static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

    /// Called once from the app's setup hook.
    pub fn init(app: tauri::AppHandle) {
        let _ = APP.set(app);
    }

    pub(super) fn with_store<T>(
        callback: impl FnOnce(&tauri_plugin_onyx_secrets::OnyxSecrets<tauri::Wry>) -> T,
    ) -> Option<T> {
        APP.get().map(|app| callback(app.onyx_secrets()))
    }
}

#[cfg(mobile)]
pub use mobile::init;

#[cfg(mobile)]
pub fn set(key: &str, value: &str) -> bool {
    mobile::with_store(|store| store.set(key, value).is_ok()).unwrap_or(false)
}
#[cfg(mobile)]
pub fn get(key: &str) -> Option<String> {
    mobile::with_store(|store| store.get(key).ok().flatten()).flatten()
}
#[cfg(mobile)]
pub fn delete(key: &str) {
    let _ = mobile::with_store(|store| store.delete(key));
}
#[cfg(mobile)]
pub fn available() -> bool {
    let _ = SERVICE;
    mobile::with_store(|store| store.availability().secure).unwrap_or(false)
}

/// Can this device enroll biometric-bound secrets?
#[cfg(mobile)]
pub fn biometric_available() -> bool {
    mobile::with_store(|store| store.availability().biometric).unwrap_or(false)
}

/// Store a biometric-bound secret; prompts the user (storing is consent).
/// Blocking — call from a blocking-capable thread.
#[cfg(mobile)]
pub fn set_protected(key: &str, value: &str, reason: &str) -> Result<(), String> {
    mobile::with_store(|store| {
        store
            .set_protected(key, value, reason)
            .map_err(|error| error.to_string())
    })
    .unwrap_or_else(|| Err("secret store not initialized".into()))
}

/// Read a biometric-bound secret; triggers the OS biometric prompt.
/// `Ok(None)` means never enrolled. Blocking.
#[cfg(mobile)]
pub fn get_protected(key: &str, reason: &str) -> Result<Option<String>, String> {
    mobile::with_store(|store| {
        store
            .get_protected(key, reason)
            .map_err(|error| error.to_string())
    })
    .unwrap_or_else(|| Err("secret store not initialized".into()))
}

/// Remove a biometric-bound secret (idempotent).
#[cfg(mobile)]
pub fn delete_protected(key: &str) {
    let _ = mobile::with_store(|store| store.delete_protected(key));
}

/// Store a secret under `key`. Returns whether the OS keychain accepted it
/// (false → caller should fall back to file storage).
#[cfg(desktop)]
pub fn set(key: &str, value: &str) -> bool {
    match keyring::Entry::new(SERVICE, key) {
        Ok(entry) => entry.set_password(value).is_ok(),
        Err(_) => false,
    }
}

/// Retrieve a secret, or `None` if absent / no keychain.
#[cfg(desktop)]
pub fn get(key: &str) -> Option<String> {
    keyring::Entry::new(SERVICE, key)
        .ok()
        .and_then(|entry| entry.get_password().ok())
}

/// Remove a secret (best-effort).
#[cfg(desktop)]
pub fn delete(key: &str) {
    if let Ok(entry) = keyring::Entry::new(SERVICE, key) {
        let _ = entry.delete_credential();
    }
}

/// Is a real keychain backend available on this machine?
#[cfg(desktop)]
pub fn available() -> bool {
    // Probe with a throwaway entry; a missing backend errors on construct
    // or on the operation.
    match keyring::Entry::new(SERVICE, "__onyx_probe__") {
        Ok(entry) => {
            // get on a likely-absent entry returns NoEntry (backend works)
            // vs a platform error (no backend).
            !matches!(
                entry.get_password(),
                Err(keyring::Error::PlatformFailure(_)) | Err(keyring::Error::NoStorageAccess(_))
            )
        }
        Err(_) => false,
    }
}
