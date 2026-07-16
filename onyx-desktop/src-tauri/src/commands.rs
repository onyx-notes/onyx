//! Tauri IPC commands: the JSON control plane. Bulk payloads (note bodies,
//! images) go through the `onyx://` protocol instead — see `protocol.rs`.

use onyx_core::NotePath;
use serde::Serialize;
use tauri::{AppHandle, State};

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

#[tauri::command]
pub async fn open_vault(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> CmdResult<VaultInfo> {
    let root = std::path::PathBuf::from(&path);
    if !root.is_dir() {
        return Err(format!("not a directory: {path}"));
    }
    let engine = Engine::open(&root).map_err(err)?;
    let note_count = engine.index().note_count().map_err(err)?;

    *state.engine.lock() = Some(engine);
    spawn_watcher(&app, &state, &root).map_err(err)?;

    Ok(VaultInfo {
        root: path,
        note_count,
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
