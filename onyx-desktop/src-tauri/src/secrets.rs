//! OS keychain storage for secrets (AI API keys), with a graceful
//! fallback when no keychain backend exists (headless Linux/CI).
//!
//! The AI config's plaintext fields stay in the app-data JSON for
//! non-secret settings (provider, base URL, model); the API key alone is
//! redirected here so it never sits in a readable file on disk when a
//! keychain is available. Fallback (no Secret Service, etc.) keeps the old
//! behavior so the app still works — documented, not silent.

const SERVICE: &str = "dev.onyx.app";

// Mobile: no keyring backend exists; secrets fall back to file storage
// until the onyx-secrets Keychain/Keystore plugin lands (mobile M4).
// Honest `available() == false` keeps callers on the fallback path.
#[cfg(mobile)]
pub fn set(_key: &str, _value: &str) -> bool {
    false
}
#[cfg(mobile)]
pub fn get(_key: &str) -> Option<String> {
    None
}
#[cfg(mobile)]
pub fn delete(_key: &str) {}
#[cfg(mobile)]
pub fn available() -> bool {
    let _ = SERVICE;
    false
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
