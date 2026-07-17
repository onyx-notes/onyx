//! Tauri IPC commands: the JSON control plane. Bulk payloads (note bodies,
//! images) go through the `onyx://` protocol instead — see `protocol.rs`.

use onyx_core::NotePath;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::engine::Engine;
use crate::state::{AppState, spawn_watcher};

/// Command errors cross IPC as strings; the frontend shows them as notices.
type CmdResult<T> = Result<T, String>;

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn parse_path(path: &str) -> CmdResult<NotePath> {
    NotePath::new(path).map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultInfo {
    pub root: String,
    pub note_count: usize,
    pub encrypted: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteInfo {
    pub path: String,
    pub title: String,
    pub is_markdown: bool,
    pub word_count: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub path: String,
    pub score: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagInfo {
    pub tag: String,
    pub count: u64,
}

/// Probe a directory before opening: does it need a passphrase?
#[tauri::command]
pub fn vault_status(path: String) -> CmdResult<String> {
    let root = std::path::PathBuf::from(&path);
    if !root.is_dir() {
        return Ok("missing".into());
    }
    Ok(if crate::engine::is_encrypted(&root) {
        "encrypted".into()
    } else {
        "plain".into()
    })
}

#[tauri::command]
pub async fn open_vault(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    passphrase: Option<String>,
) -> CmdResult<VaultInfo> {
    let root = std::path::PathBuf::from(&path);
    if !root.is_dir() {
        return Err(format!("not a directory: {path}"));
    }
    let engine = match passphrase {
        Some(passphrase) => Engine::open_encrypted(&root, &passphrase).map_err(err)?,
        None => Engine::open(&root).map_err(err)?,
    };
    install_engine(&app, &state, &root, path, engine)
}

/// Create a brand-new encrypted vault and open it.
#[tauri::command]
pub async fn create_encrypted_vault(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    passphrase: String,
) -> CmdResult<VaultInfo> {
    if passphrase.len() < 8 {
        return Err("passphrase must be at least 8 characters".into());
    }
    let root = std::path::PathBuf::from(&path);
    std::fs::create_dir_all(&root).map_err(err)?;
    let engine = Engine::create_encrypted(&root, &passphrase).map_err(err)?;
    install_engine(&app, &state, &root, path, engine)
}

/// Lock/close the current vault (keys zeroize on drop).
#[tauri::command]
pub fn lock_vault(state: State<'_, AppState>) {
    state.lock_vault();
}

fn install_engine(
    app: &AppHandle,
    state: &State<'_, AppState>,
    root: &std::path::Path,
    path: String,
    engine: Engine,
) -> CmdResult<VaultInfo> {
    let note_count = engine.index().note_count().map_err(err)?;
    let encrypted = engine.is_encrypted_vault();
    let sync_config = crate::sync::load_config(engine.vault());

    *state.engine.lock() = Some(engine);
    spawn_watcher(app, state, root).map_err(err)?;

    // Sync was previously enabled for this vault: resume automatically.
    if let Some(config) = sync_config {
        if let Err(error) = start_sync(app, state, &config) {
            tracing::warn!(%error, "sync auto-start failed");
        }
    }

    // Periodic backups, if configured.
    let backup_config =
        state.with_engine(|engine| Ok(crate::backup::load_config(engine.vault())))?;
    if backup_config.auto_interval_hours > 0 && !backup_config.destinations.is_empty() {
        crate::state::spawn_backup_timer(state, backup_config.auto_interval_hours);
    }

    Ok(VaultInfo {
        root: path,
        note_count,
        encrypted,
    })
}

#[tauri::command]
pub fn list_notes(state: State<'_, AppState>) -> CmdResult<Vec<NoteInfo>> {
    state.with_engine(|engine| {
        Ok(engine
            .index()
            .all_notes()
            .map_err(err)?
            .into_iter()
            .map(|record| NoteInfo {
                path: record.path.as_str().to_owned(),
                title: record.title,
                is_markdown: record.is_markdown,
                word_count: record.word_count,
            })
            .collect())
    })
}

#[tauri::command]
pub fn read_note(state: State<'_, AppState>, path: String) -> CmdResult<String> {
    let note = parse_path(&path)?;
    state.with_engine(|engine| engine.vault().read_text(&note).map_err(err))
}

#[tauri::command]
pub fn write_note(state: State<'_, AppState>, path: String, content: String) -> CmdResult<()> {
    let note = parse_path(&path)?;
    state.with_engine(|engine| engine.write_note(&note, &content).map_err(err))
}

#[tauri::command]
pub fn delete_note(state: State<'_, AppState>, path: String) -> CmdResult<()> {
    let note = parse_path(&path)?;
    state.with_engine(|engine| engine.delete_note(&note).map_err(err))
}

#[tauri::command]
pub fn rename_note(state: State<'_, AppState>, from: String, to: String) -> CmdResult<()> {
    let source = parse_path(&from)?;
    let target = parse_path(&to)?;
    state.with_engine(|engine| engine.rename_note(&source, &target).map_err(err))
}

#[tauri::command]
pub fn search_notes(state: State<'_, AppState>, query: String) -> CmdResult<Vec<Hit>> {
    state.with_engine(|engine| {
        // Search reads the last committed state; flush pending edits first
        // so "type then immediately search" finds them.
        engine.commit_search_if_dirty().map_err(err)?;
        Ok(engine
            .search(&query, 50)
            .map_err(err)?
            .into_iter()
            .map(|hit| Hit {
                path: hit.path,
                score: f64::from(hit.score),
            })
            .collect())
    })
}

#[tauri::command]
pub fn quick_open(state: State<'_, AppState>, query: String) -> CmdResult<Vec<Hit>> {
    state.with_engine(|engine| {
        Ok(engine
            .quick()
            .query(&query, 50)
            .into_iter()
            .map(|hit| Hit {
                path: hit.path,
                score: hit.score as f64,
            })
            .collect())
    })
}

#[tauri::command]
pub fn backlinks(state: State<'_, AppState>, path: String) -> CmdResult<Vec<String>> {
    let note = parse_path(&path)?;
    state.with_engine(|engine| {
        let id = engine.vault().note_id(&note);
        let rows = engine.index().backlinks(id).map_err(err)?;
        let mut sources = Vec::with_capacity(rows.len());
        for row in rows {
            if let Some(record) = engine.index().note(row.src).map_err(err)? {
                sources.push(record.path.as_str().to_owned());
            }
        }
        sources.dedup();
        Ok(sources)
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadingInfo {
    pub level: u8,
    pub text: String,
    pub offset: usize,
}

/// Render a note to HTML for embeds/transclusions and reading view.
/// Raw HTML in the note is escaped by the renderer (defense in depth
/// alongside the CSP).
#[tauri::command]
pub fn render_note(state: State<'_, AppState>, path: String) -> CmdResult<String> {
    let note = parse_path(&path)?;
    state.with_engine(|engine| {
        let source = engine.vault().read_text(&note).map_err(err)?;
        Ok(onyx_md::to_html(&source))
    })
}

/// Headings of a note, for the outline panel.
#[tauri::command]
pub fn note_headings(state: State<'_, AppState>, path: String) -> CmdResult<Vec<HeadingInfo>> {
    let note = parse_path(&path)?;
    state.with_engine(|engine| {
        let id = engine.vault().note_id(&note);
        Ok(engine
            .index()
            .headings(id)
            .map_err(err)?
            .into_iter()
            .map(|heading| HeadingInfo {
                level: heading.level,
                text: heading.text,
                offset: heading.span_start,
            })
            .collect())
    })
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> CmdResult<crate::settings::Settings> {
    state.with_engine(|engine| Ok(crate::settings::load(engine.vault())))
}

#[tauri::command]
pub fn update_settings(
    state: State<'_, AppState>,
    settings: crate::settings::Settings,
) -> CmdResult<()> {
    state.with_engine(|engine| crate::settings::save(engine.vault(), &settings))
}

/// Read `.obsidian` config and return the mapped settings + a list of what
/// was found — the frontend shows a review screen before saving anything.
#[tauri::command]
pub fn import_obsidian_settings(
    state: State<'_, AppState>,
) -> CmdResult<crate::settings::ObsidianImport> {
    state.with_engine(|engine| {
        let current = crate::settings::load(engine.vault());
        Ok(crate::settings::import_obsidian(engine.vault(), &current))
    })
}

/// Open (creating if needed) the daily note for `date` (`YYYY-MM-DD`,
/// frontend-local). Returns its vault path.
#[tauri::command]
pub fn daily_note(state: State<'_, AppState>, date: String) -> CmdResult<String> {
    state.with_engine(|engine| {
        let settings = crate::settings::load(engine.vault());
        let path = crate::settings::daily_note_path(&settings, &date)?;
        if !engine.vault().fs().exists(&path) {
            engine
                .write_note(&path, &format!("# {date}\n\n"))
                .map_err(err)?;
        }
        Ok(path.as_str().to_owned())
    })
}

/// Resolve a wikilink target ("Note", "folder/Note", "image.png") to a
/// vault path, exactly like the indexer resolves links. `None` means the
/// note doesn't exist yet — the frontend offers to create it.
#[tauri::command]
pub fn resolve_target(state: State<'_, AppState>, target: String) -> CmdResult<Option<String>> {
    state.with_engine(|engine| {
        let Some(id) = engine.index().resolve(&target).map_err(err)? else {
            return Ok(None);
        };
        Ok(engine
            .index()
            .note(id)
            .map_err(err)?
            .map(|record| record.path.as_str().to_owned()))
    })
}

#[tauri::command]
pub fn vault_tags(state: State<'_, AppState>) -> CmdResult<Vec<TagInfo>> {
    state.with_engine(|engine| {
        Ok(engine
            .index()
            .tags()
            .map_err(err)?
            .into_iter()
            .map(|tag| TagInfo {
                tag: tag.tag,
                count: tag.count,
            })
            .collect())
    })
}

// ---------------------------------------------------------------------------
// Sync commands
// ---------------------------------------------------------------------------

/// Resolve config into (vault_id, op key), wire the engine, start the agent.
pub(crate) fn start_sync(
    app: &AppHandle,
    state: &State<'_, AppState>,
    config: &crate::sync::SyncConfig,
) -> CmdResult<()> {
    use data_encoding::HEXLOWER;

    let device_path = app.path().app_data_dir().map_err(err)?.join("device.key");
    let device = crate::sync::DeviceIdentity::load_or_create(&device_path).map_err(err)?;
    let peer = device.peer();

    let (vault_id, op_key, root) = {
        let mut guard = state.engine.lock();
        let engine = guard.as_mut().ok_or("no vault is open")?;
        let (vault_id, op_key) = match (&config.key, engine.crypto_key()) {
            // Plaintext vault: key from config.
            (Some(key_hex), _) => {
                let key: [u8; 32] = HEXLOWER
                    .decode(key_hex.as_bytes())
                    .ok()
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or("corrupt sync config key")?;
                let vault_id: [u8; 16] = HEXLOWER
                    .decode(config.vault_id.as_bytes())
                    .ok()
                    .and_then(|bytes| bytes.try_into().ok())
                    .ok_or("corrupt sync config vault id")?;
                (vault_id, onyx_crypto::VaultKey::from_bytes(key))
            }
            // Encrypted vault: derive everything from the vault key.
            (None, Some(vault_key)) => crate::sync::derive_encrypted_sync_identity(&vault_key),
            (None, None) => return Err("sync config missing key for plaintext vault".into()),
        };
        let root = engine.root().to_path_buf();
        let store = onyx_sync::SyncStore::open(&root.join(".onyx/sync.db")).map_err(err)?;
        engine.enable_sync(crate::engine::SyncState::new(store, op_key.clone(), peer));
        (vault_id, op_key, root)
    };
    let _ = (op_key, root);

    let client = crate::sync::SyncClient::new(&config.server_url, device).map_err(err)?;
    crate::state::spawn_sync_agent(app, state, client, vault_id);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncEnabled {
    /// Pairing code for plaintext vaults; encrypted vaults pair by
    /// unlocking the same vault (keys derive identically).
    pub code: Option<String>,
}

/// Enable sync on the open vault against `server_url`.
#[tauri::command]
pub fn sync_enable(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
) -> CmdResult<SyncEnabled> {
    use data_encoding::HEXLOWER;

    let (config, code) = {
        let guard = state.engine.lock();
        let engine = guard.as_ref().ok_or("no vault is open")?;
        match engine.crypto_key() {
            Some(vault_key) => {
                let (vault_id, _) = crate::sync::derive_encrypted_sync_identity(&vault_key);
                (
                    crate::sync::SyncConfig {
                        server_url,
                        vault_id: HEXLOWER.encode(&vault_id),
                        key: None,
                    },
                    None,
                )
            }
            None => {
                let mut vault_id = [0u8; 16];
                let mut key = [0u8; 32];
                getrandom::fill(&mut vault_id).map_err(err)?;
                getrandom::fill(&mut key).map_err(err)?;
                (
                    crate::sync::SyncConfig {
                        server_url,
                        vault_id: HEXLOWER.encode(&vault_id),
                        key: Some(HEXLOWER.encode(&key)),
                    },
                    Some(crate::sync::sync_code(vault_id, &key)),
                )
            }
        }
    };

    state.with_engine(|engine| crate::sync::save_config(engine.vault(), &config).map_err(err))?;
    start_sync(&app, &state, &config)?;
    Ok(SyncEnabled { code })
}

/// Join an existing synced vault using a pairing code (plaintext vaults).
#[tauri::command]
pub fn sync_join(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
    code: String,
) -> CmdResult<()> {
    use data_encoding::HEXLOWER;

    let (vault_id, key) = crate::sync::parse_sync_code(&code).map_err(err)?;
    let config = crate::sync::SyncConfig {
        server_url,
        vault_id: HEXLOWER.encode(&vault_id),
        key: Some(HEXLOWER.encode(&key)),
    };
    state.with_engine(|engine| crate::sync::save_config(engine.vault(), &config).map_err(err))?;
    start_sync(&app, &state, &config)
}

#[tauri::command]
pub fn sync_status(state: State<'_, AppState>) -> crate::state::SyncStatusInfo {
    state.sync_status.lock().clone()
}

// ---------------------------------------------------------------------------
// Backup commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn get_backup_config(state: State<'_, AppState>) -> CmdResult<crate::backup::BackupConfig> {
    state.with_engine(|engine| Ok(crate::backup::load_config(engine.vault())))
}

#[tauri::command]
pub fn set_backup_config(
    state: State<'_, AppState>,
    config: crate::backup::BackupConfig,
) -> CmdResult<()> {
    state.with_engine(|engine| crate::backup::save_config(engine.vault(), &config).map_err(err))
}

/// Run a backup to the named destination now. Gathers file content under
/// short engine locks, then encrypts + transfers without holding any lock.
#[tauri::command]
pub async fn backup_now(
    state: State<'_, AppState>,
    destination: String,
) -> CmdResult<crate::backup::BackupReport> {
    let (key, files, dest) = {
        let guard = state.engine.lock();
        let engine = guard.as_ref().ok_or("no vault is open")?;
        let config = crate::backup::load_config(engine.vault());
        let dest = config
            .destinations
            .into_iter()
            .find(|candidate| candidate.name == destination)
            .ok_or_else(|| format!("unknown destination: {destination}"))?;
        let key =
            crate::backup::backup_key(engine.root(), engine.crypto_key().as_ref()).map_err(err)?;
        let mut files = Vec::new();
        for record in engine.index().all_notes().map_err(err)? {
            let content = engine.vault().read_bytes(&record.path).map_err(err)?;
            files.push((record.path.as_str().to_owned(), content));
        }
        (key, files, dest)
    };
    // Encryption + upload run on a blocking worker (no engine lock held).
    tauri::async_runtime::spawn_blocking(move || {
        crate::backup::run_backup(&key, &files, &dest).map_err(err)
    })
    .await
    .map_err(err)?
}

#[tauri::command]
pub async fn list_backup_snapshots(
    state: State<'_, AppState>,
    destination: String,
) -> CmdResult<Vec<u64>> {
    let dest = {
        let guard = state.engine.lock();
        let engine = guard.as_ref().ok_or("no vault is open")?;
        crate::backup::load_config(engine.vault())
            .destinations
            .into_iter()
            .find(|candidate| candidate.name == destination)
            .ok_or_else(|| format!("unknown destination: {destination}"))?
    };
    tauri::async_runtime::spawn_blocking(move || crate::backup::list_snapshots(&dest).map_err(err))
        .await
        .map_err(err)?
}

// ---------------------------------------------------------------------------
// Plugin commands
// ---------------------------------------------------------------------------

#[derive(Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Declared capabilities: "vault:read", "vault:write", "ui:commands".
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    #[serde(flatten)]
    pub manifest: PluginManifest,
    pub enabled: bool,
}

fn plugins_dir(state: &State<'_, AppState>) -> CmdResult<std::path::PathBuf> {
    let guard = state.engine.lock();
    let engine = guard.as_ref().ok_or("no vault is open")?;
    Ok(engine.root().join(".onyx/plugins"))
}

fn disabled_plugins(state: &State<'_, AppState>) -> CmdResult<Vec<String>> {
    state.with_engine(|engine| {
        let path = onyx_core::NotePath::new(".onyx/plugins-disabled.json").expect("static");
        Ok(engine
            .vault()
            .fs()
            .read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default())
    })
}

/// Installed plugins (valid manifests under `.onyx/plugins/*`), with their
/// enabled state.
#[tauri::command]
pub fn list_plugins(state: State<'_, AppState>) -> CmdResult<Vec<PluginInfo>> {
    let dir = plugins_dir(&state)?;
    let disabled = disabled_plugins(&state)?;
    let mut plugins = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(plugins); // no plugins directory yet
    };
    for entry in entries.flatten() {
        let manifest_path = entry.path().join("manifest.json");
        let Ok(bytes) = std::fs::read(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<PluginManifest>(&bytes) else {
            tracing::warn!(path = %manifest_path.display(), "invalid plugin manifest skipped");
            continue;
        };
        // The manifest id must match its directory (path-safety + identity).
        if Some(manifest.id.as_str()) != entry.file_name().to_str() {
            continue;
        }
        if !entry.path().join("main.js").is_file() {
            continue;
        }
        let enabled = !disabled.contains(&manifest.id);
        plugins.push(PluginInfo { manifest, enabled });
    }
    plugins.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(plugins)
}

#[tauri::command]
pub fn set_plugin_enabled(state: State<'_, AppState>, id: String, enabled: bool) -> CmdResult<()> {
    let mut disabled = disabled_plugins(&state)?;
    disabled.retain(|entry| entry != &id);
    if !enabled {
        disabled.push(id);
    }
    state.with_engine(|engine| {
        let path = onyx_core::NotePath::new(".onyx/plugins-disabled.json").expect("static");
        engine
            .vault()
            .fs()
            .write_atomic(&path, &serde_json::to_vec_pretty(&disabled).map_err(err)?)
            .map_err(err)
    })
}
