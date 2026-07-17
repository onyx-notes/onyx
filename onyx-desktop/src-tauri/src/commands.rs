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

// ---------------------------------------------------------------------------
// Vault Insights
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStats {
    pub note_count: usize,
    pub attachment_count: usize,
    pub total_words: u64,
    pub link_count: usize,
    pub orphan_count: usize,
    pub unresolved_count: usize,
    pub most_linked: Vec<LinkedNote>,
    pub top_tags: Vec<TagInfo>,
    pub longest_notes: Vec<LinkedNote>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedNote {
    pub path: String,
    pub count: u64,
}

/// All-local analytics: computed from the index + link graph, nothing ever
/// leaves the machine.
#[tauri::command]
pub fn vault_stats(state: State<'_, AppState>) -> CmdResult<VaultStats> {
    state.with_engine(|engine| {
        let records = engine.index().all_notes().map_err(err)?;
        let graph = onyx_core::LinkGraph::build(engine.index()).map_err(err)?;

        let by_id: std::collections::HashMap<_, _> =
            records.iter().map(|record| (record.id, record)).collect();
        let path_of = |id: onyx_core::NoteId| {
            by_id
                .get(&id)
                .map(|record| record.path.as_str().to_owned())
                .unwrap_or_default()
        };

        let note_count = records.iter().filter(|record| record.is_markdown).count();
        let mut longest: Vec<LinkedNote> = records
            .iter()
            .filter(|record| record.is_markdown)
            .map(|record| LinkedNote {
                path: record.path.as_str().to_owned(),
                count: record.word_count.unwrap_or(0),
            })
            .collect();
        longest.sort_by_key(|entry| std::cmp::Reverse(entry.count));
        longest.truncate(5);

        Ok(VaultStats {
            note_count,
            attachment_count: records.len() - note_count,
            total_words: records.iter().filter_map(|record| record.word_count).sum(),
            link_count: graph.edge_count(),
            orphan_count: graph.orphans().len(),
            unresolved_count: engine.index().unresolved_targets().map_err(err)?.len(),
            most_linked: graph
                .most_linked(5)
                .into_iter()
                .map(|(id, count)| LinkedNote {
                    path: path_of(id),
                    count: count as u64,
                })
                .collect(),
            top_tags: engine
                .index()
                .tags()
                .map_err(err)?
                .into_iter()
                .take(8)
                .map(|tag| TagInfo {
                    tag: tag.tag,
                    count: tag.count,
                })
                .collect(),
            longest_notes: longest,
        })
    })
}

// ---------------------------------------------------------------------------
// Graph view data
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphPayload {
    pub nodes: Vec<GraphNodeInfo>,
    /// Edges as index pairs into `nodes`.
    pub edges: Vec<[u32; 2]>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphNodeInfo {
    pub path: String,
    pub title: String,
    pub degree: u32,
}

#[tauri::command]
pub fn graph_payload(state: State<'_, AppState>) -> CmdResult<GraphPayload> {
    state.with_engine(|engine| {
        let records = engine.index().all_notes().map_err(err)?;
        let graph = onyx_core::LinkGraph::build(engine.index()).map_err(err)?;

        let mut position_of = std::collections::HashMap::new();
        let mut nodes = Vec::with_capacity(records.len());
        for record in &records {
            if !record.is_markdown {
                continue;
            }
            position_of.insert(record.id, nodes.len() as u32);
            nodes.push(GraphNodeInfo {
                path: record.path.as_str().to_owned(),
                title: record.title.clone(),
                degree: 0,
            });
        }

        let mut edges = Vec::new();
        for record in &records {
            let Some(&source) = position_of.get(&record.id) else {
                continue;
            };
            for target_id in graph.outgoing(record.id) {
                if let Some(&target) = position_of.get(&target_id) {
                    edges.push([source, target]);
                    nodes[source as usize].degree += 1;
                    nodes[target as usize].degree += 1;
                }
            }
        }
        Ok(GraphPayload { nodes, edges })
    })
}

// ---------------------------------------------------------------------------
// AI commands
// ---------------------------------------------------------------------------

fn app_data(app: &AppHandle) -> CmdResult<std::path::PathBuf> {
    app.path().app_data_dir().map_err(err)
}

#[tauri::command]
pub fn get_ai_config(app: AppHandle) -> CmdResult<crate::ai::AiConfig> {
    Ok(crate::ai::load_config(&app_data(&app)?))
}

#[tauri::command]
pub fn set_ai_config(app: AppHandle, config: crate::ai::AiConfig) -> CmdResult<()> {
    crate::ai::save_config(&app_data(&app)?, &config)
}

#[tauri::command]
pub fn ai_request_log(state: State<'_, AppState>) -> Vec<crate::ai::AiLogEntry> {
    state.ai_log.snapshot()
}

/// Chat with the configured provider. `contextPath` optionally includes a
/// note's content as system context (visible in the request log like
/// everything else).
#[tauri::command]
pub async fn ai_chat(
    app: AppHandle,
    state: State<'_, AppState>,
    messages: Vec<crate::ai::ChatMessage>,
    context_path: Option<String>,
) -> CmdResult<String> {
    let config = crate::ai::load_config(&app_data(&app)?);
    let system = match context_path {
        Some(path) => {
            let note = parse_path(&path)?;
            let content =
                state.with_engine(|engine| engine.vault().read_text(&note).map_err(err))?;
            Some(format!(
                "You are the AI assistant inside the Onyx note-taking app. \
                 The user is currently viewing this note:\n\n---\n{content}\n---\n\
                 Answer with reference to it when relevant."
            ))
        }
        None => Some("You are the AI assistant inside the Onyx note-taking app.".to_owned()),
    };
    let log = std::sync::Arc::clone(&state.ai_log);
    tauri::async_runtime::spawn_blocking(move || {
        crate::ai::chat(&config, system.as_deref(), &messages, &log)
    })
    .await
    .map_err(err)?
}

// ---------------------------------------------------------------------------
// Device enrollment commands
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollStart {
    pub code: String,
}

/// New-device side, step 1: publish an enrollment request. Show the code
/// to the user (they type it on the existing device).
#[tauri::command]
pub fn enroll_start(
    app: AppHandle,
    state: State<'_, AppState>,
    server_url: String,
) -> CmdResult<EnrollStart> {
    let device_path = app_data(&app)?.join("device.key");
    let device = crate::sync::DeviceIdentity::load_or_create(&device_path).map_err(err)?;
    let mut client = crate::sync::SyncClient::new(&server_url, device).map_err(err)?;
    let (code, receiver) = crate::sync::enroll_begin(&mut client).map_err(err)?;
    *state.pending_enroll.lock() = Some(crate::state::PendingEnrollment {
        server_url,
        code: code.clone(),
        receiver,
        payload: None,
    });
    Ok(EnrollStart { code })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollWaitResult {
    pub sas: String,
}

/// New-device side, step 2: wait for the existing device to respond.
/// Returns the SAS for the user to compare; nothing is applied yet.
#[tauri::command]
pub async fn enroll_wait(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<EnrollWaitResult> {
    let (server_url, code) = {
        let guard = state.pending_enroll.lock();
        let pending = guard.as_ref().ok_or("no enrollment in progress")?;
        (pending.server_url.clone(), pending.code.clone())
    };
    let device_path = app_data(&app)?.join("device.key");

    let (payload, sas) = {
        let receiver = {
            let mut guard = state.pending_enroll.lock();
            let pending = guard.as_mut().ok_or("no enrollment in progress")?;
            // The receiver moves into the blocking task; keep the slot.
            std::mem::replace(
                &mut pending.receiver,
                onyx_crypto::EnrollmentReceiver::generate(),
            )
        };
        tauri::async_runtime::spawn_blocking(move || {
            let device = crate::sync::DeviceIdentity::load_or_create(&device_path).map_err(err)?;
            let mut client = crate::sync::SyncClient::new(&server_url, device).map_err(err)?;
            crate::sync::enroll_claim(
                &mut client,
                &code,
                &receiver,
                std::time::Duration::from_secs(180),
            )
            .map_err(err)
        })
        .await
        .map_err(err)??
    };

    let mut guard = state.pending_enroll.lock();
    if let Some(pending) = guard.as_mut() {
        pending.payload = Some(payload);
    }
    Ok(EnrollWaitResult { sas })
}

/// New-device side, step 3: the user confirmed the SAS matches — apply
/// the received sync identity to the open vault and start syncing.
#[tauri::command]
pub fn enroll_confirm(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    use data_encoding::HEXLOWER;

    let (server_url, payload) = {
        let mut guard = state.pending_enroll.lock();
        let pending = guard.take().ok_or("no enrollment in progress")?;
        let payload = pending.payload.ok_or("enrollment response not received")?;
        (pending.server_url, payload)
    };
    let config = crate::sync::SyncConfig {
        server_url,
        vault_id: HEXLOWER.encode(&payload.vault_id),
        key: Some(HEXLOWER.encode(&payload.op_key)),
    };
    state.with_engine(|engine| crate::sync::save_config(engine.vault(), &config).map_err(err))?;
    start_sync(&app, &state, &config)
}

/// Abort a pending enrollment (SAS mismatch or user cancel).
#[tauri::command]
pub fn enroll_cancel(state: State<'_, AppState>) {
    *state.pending_enroll.lock() = None;
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollApproveResult {
    pub sas: String,
}

/// Existing-device side: approve a new device by its code. Requires sync
/// to be enabled on this vault (we hand over its sync identity).
#[tauri::command]
pub fn enroll_approve_device(
    app: AppHandle,
    state: State<'_, AppState>,
    code: String,
) -> CmdResult<EnrollApproveResult> {
    use data_encoding::HEXLOWER;

    let (config, crypto_key) = {
        let guard = state.engine.lock();
        let engine = guard.as_ref().ok_or("no vault is open")?;
        let config =
            crate::sync::load_config(engine.vault()).ok_or("sync is not enabled on this vault")?;
        (config, engine.crypto_key())
    };
    let (vault_id, op_key) = match (&config.key, crypto_key) {
        (Some(key_hex), _) => {
            let key: [u8; 32] = HEXLOWER
                .decode(key_hex.as_bytes())
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or("corrupt sync config")?;
            let vault_id: [u8; 16] = HEXLOWER
                .decode(config.vault_id.as_bytes())
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or("corrupt sync config")?;
            (vault_id, key)
        }
        (None, Some(vault_key)) => {
            // MUST match derive_encrypted_sync_identity's derivation
            // exactly — the enrollee gets the same op key this vault uses.
            let (vault_id, _) = crate::sync::derive_encrypted_sync_identity(&vault_key);
            let op_key = vault_key.derive("onyx-sync 2026-07 op key v1", &[]);
            (vault_id, op_key)
        }
        (None, None) => return Err("sync config missing key".into()),
    };

    let device_path = app_data(&app)?.join("device.key");
    let device = crate::sync::DeviceIdentity::load_or_create(&device_path).map_err(err)?;
    let mut client = crate::sync::SyncClient::new(&config.server_url, device).map_err(err)?;
    let sas = crate::sync::enroll_approve(&mut client, &code, vault_id, &op_key).map_err(err)?;
    Ok(EnrollApproveResult { sas })
}

// ---------------------------------------------------------------------------
// RAG: local semantic index over the vault
// ---------------------------------------------------------------------------

const EMBEDDINGS_PATH: &str = ".onyx/embeddings.json";
const CHUNK_TARGET_CHARS: usize = 1200;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RagStatus {
    pub configured: bool,
    pub indexed_chunks: usize,
}

fn load_vector_store(state: &State<'_, AppState>) -> crate::rag::VectorStore {
    state
        .with_engine(|engine| {
            let path = onyx_core::NotePath::new(EMBEDDINGS_PATH).expect("static");
            Ok(engine
                .vault()
                .fs()
                .read(&path)
                .ok()
                .and_then(|bytes| crate::rag::VectorStore::from_bytes(&bytes).ok())
                .unwrap_or_default())
        })
        .unwrap_or_default()
}

#[tauri::command]
pub fn rag_status(app: AppHandle, state: State<'_, AppState>) -> CmdResult<RagStatus> {
    let config = crate::ai::load_config(&app_data(&app)?);
    Ok(RagStatus {
        configured: !config.embed_model.is_empty() && !config.base_url.is_empty(),
        indexed_chunks: load_vector_store(&state).len(),
    })
}

/// (Re)build the semantic index: chunk every markdown note, embed changed
/// notes via the configured endpoint, persist. Incremental — unchanged
/// notes keep their vectors.
#[tauri::command]
pub async fn rag_reindex(app: AppHandle, state: State<'_, AppState>) -> CmdResult<RagStatus> {
    let config = crate::ai::load_config(&app_data(&app)?);
    if config.embed_model.is_empty() || config.base_url.is_empty() {
        return Err("configure an embedding model in AI settings first".into());
    }

    // Gather note contents + current index under a short lock.
    let (notes, mut store): (Vec<(String, String)>, crate::rag::VectorStore) = {
        let guard = state.engine.lock();
        let engine = guard.as_ref().ok_or("no vault is open")?;
        let store = load_vector_store(&state);
        let mut notes = Vec::new();
        for record in engine.index().all_notes().map_err(err)? {
            if record.is_markdown {
                let content = engine.vault().read_text(&record.path).map_err(err)?;
                notes.push((record.path.as_str().to_owned(), content));
            }
        }
        (notes, store)
    };

    // Prune notes that no longer exist.
    let live: std::collections::HashSet<&str> =
        notes.iter().map(|(path, _)| path.as_str()).collect();
    for path in store.indexed_paths() {
        if !live.contains(path.as_str()) {
            store.remove_note(&path);
        }
    }

    let base = config.base_url.clone();
    let key = config.api_key.clone();
    let model = config.embed_model.clone();

    // Embed note-by-note off the lock (network-bound).
    let store = tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
        let already = store.indexed_paths();
        let mut store = store;
        for (path, content) in &notes {
            // Skip notes already indexed this session (full incremental
            // hashing lands with the background indexer; re-embedding an
            // unchanged note is only wasted network, never wrong).
            if already.contains(path) {
                continue;
            }
            let chunks = crate::rag::chunk_note(path, content, CHUNK_TARGET_CHARS);
            if chunks.is_empty() {
                continue;
            }
            let texts: Vec<String> = chunks.iter().map(|chunk| chunk.text.clone()).collect();
            let vectors = crate::rag::embed_texts(&base, &key, &model, &texts)?;
            let embedded = chunks
                .into_iter()
                .zip(vectors)
                .map(|(chunk, vector)| crate::rag::Embedded { chunk, vector })
                .collect();
            store.set_note(path, embedded);
        }
        Ok(store)
    })
    .await
    .map_err(err)??;

    // Persist.
    let indexed_chunks = store.len();
    let bytes = store.to_bytes().map_err(err)?;
    state.with_engine(|engine| {
        let path = onyx_core::NotePath::new(EMBEDDINGS_PATH).expect("static");
        engine.vault().fs().write_atomic(&path, &bytes).map_err(err)
    })?;

    Ok(RagStatus {
        configured: true,
        indexed_chunks,
    })
}

// ---------------------------------------------------------------------------
// Vault Assistant agent
// ---------------------------------------------------------------------------

/// Max tool iterations before we force a finish (runaway guard).
const AGENT_MAX_STEPS: usize = 16;
/// Cap search/list output fed back to the model.
const AGENT_MAX_RESULTS: usize = 40;

/// Run the agent loop for `goal`. Read/search tools execute immediately;
/// propose_* tools ONLY accumulate — nothing is written. Returns the
/// changeset for the user to review and apply.
#[tauri::command]
pub async fn agent_run(
    app: AppHandle,
    state: State<'_, AppState>,
    goal: String,
) -> CmdResult<crate::agent::Changeset> {
    let config = crate::ai::load_config(&app_data(&app)?);
    if config.base_url.is_empty() || config.model.is_empty() {
        return Err("configure an AI model in settings first".into());
    }

    let system = format!("{}\n\nUser goal: {goal}", crate::agent::TOOL_SPEC);
    let mut messages = vec![crate::ai::ChatMessage {
        role: "user".into(),
        content: "Begin. Respond with your first tool call as JSON.".into(),
    }];
    let mut changeset = crate::agent::Changeset::default();
    let log = std::sync::Arc::clone(&state.ai_log);

    for _ in 0..AGENT_MAX_STEPS {
        // One model turn (blocking HTTP off the async runtime).
        let reply = {
            let config = config.clone();
            let system = system.clone();
            let turn = messages.clone();
            let log = std::sync::Arc::clone(&log);
            tauri::async_runtime::spawn_blocking(move || {
                crate::ai::chat(&config, Some(&system), &turn, &log)
            })
            .await
            .map_err(err)??
        };
        messages.push(crate::ai::ChatMessage {
            role: "assistant".into(),
            content: reply.clone(),
        });

        let tool = match crate::agent::parse_tool_call(&reply) {
            Ok(tool) => tool,
            Err(parse_error) => {
                // Nudge the model back to protocol instead of failing.
                messages.push(crate::ai::ChatMessage {
                    role: "user".into(),
                    content: format!("{parse_error}. Reply with ONE JSON tool-call object only."),
                });
                continue;
            }
        };

        let feedback = match tool {
            crate::agent::ToolCall::Finish { message } => {
                changeset.finished = Some(message);
                return Ok(changeset);
            }
            crate::agent::ToolCall::ProposeWrite { path, content } => {
                changeset.log.push(format!("propose write {path}"));
                changeset.add(crate::agent::Proposal::Write {
                    path: path.clone(),
                    content,
                });
                format!("proposal recorded for {path}")
            }
            crate::agent::ToolCall::ProposeDelete { path } => {
                changeset.log.push(format!("propose delete {path}"));
                changeset.add(crate::agent::Proposal::Delete { path: path.clone() });
                format!("delete proposal recorded for {path}")
            }
            crate::agent::ToolCall::ListNotes => state.with_engine(|engine| {
                let paths: Vec<String> = engine
                    .index()
                    .all_notes()
                    .map_err(err)?
                    .into_iter()
                    .filter(|record| record.is_markdown)
                    .take(AGENT_MAX_RESULTS)
                    .map(|record| record.path.as_str().to_owned())
                    .collect();
                Ok(paths.join("\n"))
            })?,
            crate::agent::ToolCall::SearchVault { query } => state.with_engine(|engine| {
                engine.commit_search_if_dirty().map_err(err)?;
                let hits: Vec<String> = engine
                    .search(&query, AGENT_MAX_RESULTS)
                    .map_err(err)?
                    .into_iter()
                    .map(|hit| hit.path)
                    .collect();
                Ok(if hits.is_empty() {
                    "no matches".to_owned()
                } else {
                    hits.join("\n")
                })
            })?,
            crate::agent::ToolCall::ReadNote { path } => {
                let note = parse_path(&path)?;
                state.with_engine(|engine| {
                    Ok(engine
                        .vault()
                        .read_text(&note)
                        .unwrap_or_else(|_| "(note not found)".to_owned()))
                })?
            }
        };

        messages.push(crate::ai::ChatMessage {
            role: "user".into(),
            content: format!("Tool result:\n{feedback}\n\nNext tool call as JSON."),
        });
    }

    changeset.finished =
        Some("Reached the step limit. Review the proposals gathered so far.".to_owned());
    Ok(changeset)
}

/// Apply approved proposals atomically (each is a normal engine write/
/// delete, so sync + index + search all update). `approved` are the
/// proposal indices the user checked.
#[tauri::command]
pub fn agent_apply(
    state: State<'_, AppState>,
    proposals: Vec<crate::agent::Proposal>,
) -> CmdResult<usize> {
    state.with_engine(|engine| {
        let mut applied = 0;
        for proposal in &proposals {
            let path = onyx_core::NotePath::new(proposal.path()).map_err(err)?;
            match proposal {
                crate::agent::Proposal::Write { content, .. } => {
                    engine.write_note(&path, content).map_err(err)?;
                }
                crate::agent::Proposal::Delete { .. } => {
                    if engine.vault().fs().exists(&path) {
                        engine.delete_note(&path).map_err(err)?;
                    }
                }
            }
            applied += 1;
        }
        engine.commit_search_if_dirty().map_err(err)?;
        Ok(applied)
    })
}

// ---------------------------------------------------------------------------
// Plugin registry + install
// ---------------------------------------------------------------------------

#[derive(Serialize, serde::Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Base URL hosting `manifest.json` and `main.js` for this plugin.
    pub source: String,
}

/// Fetch a plugin registry index (a JSON array, obsidian-releases style).
/// The default community index ships as a URL the user can change.
#[tauri::command]
pub async fn plugin_registry(registry_url: String) -> CmdResult<Vec<RegistryEntry>> {
    let entries = tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(err)?;
        let response = client.get(&registry_url).send().map_err(err)?;
        if !response.status().is_success() {
            return Err(format!("registry returned {}", response.status()));
        }
        response
            .json::<Vec<RegistryEntry>>()
            .map_err(|error| format!("invalid registry JSON: {error}"))
    })
    .await
    .map_err(err)??;
    Ok(entries)
}

/// Install a plugin from a source base URL: fetch + validate manifest,
/// fetch main.js, write into `.onyx/plugins/<id>/`. Returns the manifest.
/// Installs disabled by default — the user reviews capabilities, then
/// enables (a plugin never runs merely by being installed).
#[tauri::command]
pub async fn install_plugin(
    state: State<'_, AppState>,
    source: String,
) -> CmdResult<PluginManifest> {
    let base = source.trim_end_matches('/').to_owned();
    let (manifest, code) = tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(err)?;
        let manifest_text = client
            .get(format!("{base}/manifest.json"))
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .text()
            .map_err(err)?;
        let manifest: PluginManifest = serde_json::from_str(&manifest_text)
            .map_err(|error| format!("bad manifest: {error}"))?;
        let code = client
            .get(format!("{base}/main.js"))
            .send()
            .map_err(err)?
            .error_for_status()
            .map_err(err)?
            .text()
            .map_err(err)?;
        Ok((manifest, code))
    })
    .await
    .map_err(err)??;

    // The id must be path-safe (it becomes a directory name).
    if manifest.id.is_empty()
        || !manifest
            .id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!("unsafe plugin id: {:?}", manifest.id));
    }

    let dir = plugins_dir(&state)?.join(&manifest.id);
    std::fs::create_dir_all(&dir).map_err(err)?;
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).map_err(err)?,
    )
    .map_err(err)?;
    std::fs::write(dir.join("main.js"), code).map_err(err)?;

    // Installed disabled: force an explicit enable after capability review.
    let mut disabled = disabled_plugins(&state)?;
    if !disabled.contains(&manifest.id) {
        disabled.push(manifest.id.clone());
        state.with_engine(|engine| {
            let path = onyx_core::NotePath::new(".onyx/plugins-disabled.json").expect("static");
            engine
                .vault()
                .fs()
                .write_atomic(&path, &serde_json::to_vec_pretty(&disabled).map_err(err)?)
                .map_err(err)
        })?;
    }
    Ok(manifest)
}

/// Uninstall a plugin: remove its directory and disabled-list entry.
#[tauri::command]
pub fn uninstall_plugin(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("invalid plugin id".into());
    }
    let dir = plugins_dir(&state)?.join(&id);
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(err)?;
    }
    let mut disabled = disabled_plugins(&state)?;
    disabled.retain(|entry| entry != &id);
    state.with_engine(|engine| {
        let path = onyx_core::NotePath::new(".onyx/plugins-disabled.json").expect("static");
        engine
            .vault()
            .fs()
            .write_atomic(&path, &serde_json::to_vec_pretty(&disabled).map_err(err)?)
            .map_err(err)
    })
}

/// Whether the OS keychain is available (secrets stored there vs a file).
#[tauri::command]
pub fn keychain_available() -> bool {
    crate::secrets::available()
}

// ---------------------------------------------------------------------------
// Single-note E2EE share links
// ---------------------------------------------------------------------------

const SHARES_PATH: &str = ".onyx/shares.json";

fn base64url(bytes: &[u8]) -> String {
    data_encoding::BASE64URL_NOPAD.encode(bytes)
}

fn load_shares(engine: &Engine) -> std::collections::HashMap<String, String> {
    onyx_core::NotePath::new(SHARES_PATH)
        .ok()
        .and_then(|path| engine.vault().fs().read(&path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_shares(
    engine: &Engine,
    shares: &std::collections::HashMap<String, String>,
) -> CmdResult<()> {
    let path = onyx_core::NotePath::new(SHARES_PATH).expect("static");
    engine
        .vault()
        .fs()
        .write_atomic(&path, &serde_json::to_vec_pretty(shares).map_err(err)?)
        .map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareLink {
    pub id: String,
    pub url: String,
}

/// Share a note as an end-to-end encrypted link. Renders the note to HTML,
/// seals it with a fresh AES-GCM key, uploads the ciphertext, and returns
/// a link whose fragment carries the key (never sent to the server).
#[tauri::command]
pub async fn create_share(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> CmdResult<ShareLink> {
    let note = parse_path(&path)?;

    let (server_url, html) = {
        let guard = state.engine.lock();
        let engine = guard.as_ref().ok_or("no vault is open")?;
        let config = crate::sync::load_config(engine.vault())
            .ok_or("enable sync first — shares use your sync server")?;
        let source = engine.vault().read_text(&note).map_err(err)?;
        (config.server_url, onyx_md::to_html(&source))
    };

    let (key, blob) = onyx_crypto::share_seal(html.as_bytes());
    let mut id_bytes = [0u8; 12];
    getrandom::fill(&mut id_bytes).map_err(err)?;
    let id = base64url(&id_bytes).replace('_', "-"); // id charset is [A-Za-z0-9-]

    let device_path = app_data(&app)?.join("device.key");
    let device = crate::sync::DeviceIdentity::load_or_create(&device_path).map_err(err)?;
    let mut client = crate::sync::SyncClient::new(&server_url, device).map_err(err)?;
    let id_for_upload = id.clone();
    tauri::async_runtime::spawn_blocking(move || client.put_share(&id_for_upload, blob))
        .await
        .map_err(err)?
        .map_err(err)?;

    // Remember the share so it can be listed/revoked.
    state.with_engine(|engine| {
        let mut shares = load_shares(engine);
        shares.insert(id.clone(), path.clone());
        save_shares(engine, &shares)
    })?;

    let url = format!(
        "{}/s/{id}#{}",
        server_url.trim_end_matches('/'),
        base64url(&key)
    );
    Ok(ShareLink { id, url })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareEntry {
    pub id: String,
    pub path: String,
}

#[tauri::command]
pub fn list_shares(state: State<'_, AppState>) -> CmdResult<Vec<ShareEntry>> {
    state.with_engine(|engine| {
        Ok(load_shares(engine)
            .into_iter()
            .map(|(id, path)| ShareEntry { id, path })
            .collect())
    })
}

/// Revoke a share: delete it from the server and forget it locally.
#[tauri::command]
pub async fn revoke_share(app: AppHandle, state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let server_url = {
        let guard = state.engine.lock();
        let engine = guard.as_ref().ok_or("no vault is open")?;
        crate::sync::load_config(engine.vault())
            .ok_or("sync not configured")?
            .server_url
    };
    let device_path = app_data(&app)?.join("device.key");
    let device = crate::sync::DeviceIdentity::load_or_create(&device_path).map_err(err)?;
    let mut client = crate::sync::SyncClient::new(&server_url, device).map_err(err)?;
    let id_for_delete = id.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || client.delete_share(&id_for_delete))
        .await
        .map_err(err)?;
    state.with_engine(|engine| {
        let mut shares = load_shares(engine);
        shares.remove(&id);
        save_shares(engine, &shares)
    })
}

// ---------------------------------------------------------------------------
// Note history (time machine)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteVersion {
    pub created_ms: u64,
    /// Hex plaintext hash (distinguishes identical-looking versions).
    pub hash: String,
}

/// Saved versions of a note, newest first.
#[tauri::command]
pub fn note_history(state: State<'_, AppState>, path: String) -> CmdResult<Vec<NoteVersion>> {
    let note = parse_path(&path)?;
    state.with_engine(|engine| {
        let id = engine.vault().note_id(&note);
        Ok(engine
            .history()
            .versions(id)
            .map_err(err)?
            .into_iter()
            .map(|version| NoteVersion {
                created_ms: version.created_ms,
                hash: version
                    .hash
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            })
            .collect())
    })
}

/// Content of a specific past version (for the diff/preview).
#[tauri::command]
pub fn note_version_content(
    state: State<'_, AppState>,
    path: String,
    created_ms: u64,
) -> CmdResult<String> {
    let note = parse_path(&path)?;
    state.with_engine(|engine| {
        let id = engine.vault().note_id(&note);
        engine
            .history()
            .get(id, created_ms)
            .map_err(err)?
            .ok_or_else(|| "version not found".to_owned())
    })
}

/// Restore a note to a past version (itself recorded, so it's undoable).
#[tauri::command]
pub fn restore_note_version(
    state: State<'_, AppState>,
    path: String,
    created_ms: u64,
) -> CmdResult<()> {
    let note = parse_path(&path)?;
    state.with_engine(|engine| engine.restore_version(&note, created_ms).map_err(err))
}

// ---------------------------------------------------------------------------
// onyx-query blocks
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryOutput {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub error: Option<String>,
}

/// Execute an onyx-query block against the current vault index.
#[tauri::command]
pub fn run_query_block(state: State<'_, AppState>, source: String) -> CmdResult<QueryOutput> {
    state.with_engine(|engine| {
        let rows = engine.index().query_rows().map_err(err)?;
        match onyx_core::run_query(&source, &rows) {
            Ok(result) => Ok(QueryOutput {
                columns: result.columns,
                rows: result.rows,
                error: None,
            }),
            Err(message) => Ok(QueryOutput {
                columns: Vec::new(),
                rows: Vec::new(),
                error: Some(message),
            }),
        }
    })
}
