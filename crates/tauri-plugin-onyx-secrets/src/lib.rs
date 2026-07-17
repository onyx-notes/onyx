//! Mobile secret storage behind the platform's hardware-backed keystore.
//!
//! iOS: Keychain with `AfterFirstUnlockThisDeviceOnly` (plain entries) and
//! `SecAccessControl(.biometryCurrentSet)` (protected entries). Android:
//! an `AndroidKeyStore` AES-GCM key encrypting a private prefs file (plain)
//! and per-entry auth-required keys unlocked via `BiometricPrompt`
//! (protected). Protected entries are invalidated when the biometric
//! enrollment changes — by design: a new fingerprint must not unlock old
//! vault keys.
//!
//! Rust is the only caller (`run_mobile_plugin`); no command is exposed to
//! the webview, so scripts can never read key material.
//!
//! On desktop the plugin compiles to an honest "unavailable" stub — the
//! desktop app keeps using the OS keychain via `keyring` instead.

use serde::{Deserialize, Serialize};
use tauri::Manager;
use tauri::Runtime;
use tauri::plugin::{Builder, TauriPlugin};

#[cfg(mobile)]
use tauri::plugin::PluginHandle;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[cfg(mobile)]
    #[error(transparent)]
    PluginInvoke(#[from] tauri::plugin::mobile::PluginInvokeError),
    #[error("secret storage is not available on this platform")]
    Unavailable,
    /// The user dismissed the biometric prompt, or the entry was
    /// invalidated by a biometric enrollment change.
    #[error("biometric authentication failed: {0}")]
    Biometric(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// What the platform offers: `secure` = hardware-backed storage works at
/// all; `biometric` = protected (biometric-bound) entries are enrollable.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Availability {
    pub secure: bool,
    pub biometric: bool,
}

#[cfg(mobile)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KeyArgs<'a> {
    key: &'a str,
}

#[cfg(mobile)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SetArgs<'a> {
    key: &'a str,
    value: &'a str,
}

#[cfg(mobile)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProtectedGetArgs<'a> {
    key: &'a str,
    /// Shown in the OS biometric prompt ("Unlock vault …").
    reason: &'a str,
}

#[cfg(mobile)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProtectedSetArgs<'a> {
    key: &'a str,
    value: &'a str,
    reason: &'a str,
}

#[cfg(mobile)]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ValueResponse {
    value: Option<String>,
}

/// Handle to the platform implementation, managed in Tauri state.
pub struct OnyxSecrets<R: Runtime> {
    #[cfg(mobile)]
    handle: PluginHandle<R>,
    #[cfg(not(mobile))]
    _marker: std::marker::PhantomData<fn() -> R>,
}

impl<R: Runtime> OnyxSecrets<R> {
    /// Probe what this device supports.
    pub fn availability(&self) -> Availability {
        #[cfg(mobile)]
        {
            self.handle
                .run_mobile_plugin::<Availability>("available", ())
                .unwrap_or_default()
        }
        #[cfg(not(mobile))]
        Availability::default()
    }

    /// Store a secret (keystore-encrypted, no user interaction).
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        #[cfg(mobile)]
        {
            self.handle
                .run_mobile_plugin::<()>("set", SetArgs { key, value })?;
            Ok(())
        }
        #[cfg(not(mobile))]
        {
            let _ = (key, value);
            Err(Error::Unavailable)
        }
    }

    /// Read a secret, `None` if absent.
    pub fn get(&self, key: &str) -> Result<Option<String>> {
        #[cfg(mobile)]
        {
            let response: ValueResponse =
                self.handle.run_mobile_plugin("get", KeyArgs { key })?;
            Ok(response.value)
        }
        #[cfg(not(mobile))]
        {
            let _ = key;
            Err(Error::Unavailable)
        }
    }

    /// Remove a secret (idempotent).
    pub fn delete(&self, key: &str) -> Result<()> {
        #[cfg(mobile)]
        {
            self.handle
                .run_mobile_plugin::<()>("delete", KeyArgs { key })?;
            Ok(())
        }
        #[cfg(not(mobile))]
        {
            let _ = key;
            Err(Error::Unavailable)
        }
    }

    /// Store a biometric-bound secret. Prompts for biometric authentication
    /// (storing is consent). Blocks until the prompt resolves — call from a
    /// blocking-capable thread, never the main/event thread.
    pub fn set_protected(&self, key: &str, value: &str, reason: &str) -> Result<()> {
        #[cfg(mobile)]
        {
            self.handle
                .run_mobile_plugin::<()>("setProtected", ProtectedSetArgs { key, value, reason })
                .map_err(biometric_err)?;
            Ok(())
        }
        #[cfg(not(mobile))]
        {
            let _ = (key, value, reason);
            Err(Error::Unavailable)
        }
    }

    /// Read a biometric-bound secret; triggers the OS biometric prompt.
    /// `None` if never enrolled. Blocking — same caveat as `set_protected`.
    pub fn get_protected(&self, key: &str, reason: &str) -> Result<Option<String>> {
        #[cfg(mobile)]
        {
            let response: ValueResponse = self
                .handle
                .run_mobile_plugin("getProtected", ProtectedGetArgs { key, reason })
                .map_err(biometric_err)?;
            Ok(response.value)
        }
        #[cfg(not(mobile))]
        {
            let _ = (key, reason);
            Err(Error::Unavailable)
        }
    }

    /// Remove a biometric-bound secret and its keystore key (idempotent).
    pub fn delete_protected(&self, key: &str) -> Result<()> {
        #[cfg(mobile)]
        {
            self.handle
                .run_mobile_plugin::<()>("deleteProtected", KeyArgs { key })?;
            Ok(())
        }
        #[cfg(not(mobile))]
        {
            let _ = key;
            Err(Error::Unavailable)
        }
    }
}

#[cfg(mobile)]
fn biometric_err(error: tauri::plugin::mobile::PluginInvokeError) -> Error {
    Error::Biometric(error.to_string())
}

/// Access the secrets store from any `Manager` (AppHandle, Window, …).
pub trait OnyxSecretsExt<R: Runtime> {
    fn onyx_secrets(&self) -> &OnyxSecrets<R>;
}

impl<R: Runtime, T: Manager<R>> OnyxSecretsExt<R> for T {
    fn onyx_secrets(&self) -> &OnyxSecrets<R> {
        self.state::<OnyxSecrets<R>>().inner()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("onyx-secrets")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            let handle =
                api.register_android_plugin("app.onyx.plugins.secrets", "SecretsPlugin")?;
            #[cfg(target_os = "ios")]
            let handle = api.register_ios_plugin(init_plugin_onyx_secrets)?;
            #[cfg(mobile)]
            app.manage(OnyxSecrets { handle });
            #[cfg(not(mobile))]
            {
                let _ = api;
                app.manage(OnyxSecrets::<R> {
                    _marker: std::marker::PhantomData,
                });
            }
            Ok(())
        })
        .build()
}

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_onyx_secrets);
