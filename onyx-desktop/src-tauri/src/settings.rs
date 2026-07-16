//! Vault settings: stored as JSON at `<vault>/.onyx/settings.json`
//! (syncable, hidden from indexing), plus the `.obsidian` importer.
//!
//! Importer contract: it never writes to `.obsidian/` — Onyx runs
//! side-by-side with Obsidian on the same vault, always.

use onyx_core::{NotePath, Vault};
use serde::{Deserialize, Serialize};

const SETTINGS_PATH: &str = ".onyx/settings.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Cap editor line width for readability (Obsidian's default on).
    pub readable_line_length: bool,
    /// Editor base font size in px.
    pub base_font_size: u16,
    /// "dark" | "light" | "system".
    pub theme: String,
    /// Prefer `[text](path)` over `[[wikilinks]]` for generated links.
    pub use_markdown_links: bool,
    /// Folder for new notes ("" = vault root).
    pub new_file_folder: String,
    /// Folder for pasted/dragged attachments.
    pub attachment_folder: String,
    /// Folder for daily notes ("" = vault root).
    pub daily_note_folder: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            readable_line_length: true,
            base_font_size: 15,
            theme: "dark".into(),
            use_markdown_links: false,
            new_file_folder: String::new(),
            attachment_folder: "attachments".into(),
            daily_note_folder: String::new(),
        }
    }
}

fn settings_path() -> NotePath {
    NotePath::new(SETTINGS_PATH).expect("static settings path is valid")
}

/// Load settings; missing or corrupt file falls back to defaults (never
/// blocks opening a vault).
pub fn load(vault: &Vault) -> Settings {
    vault
        .fs()
        .read(&settings_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(vault: &Vault, settings: &Settings) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    vault
        .fs()
        .write_atomic(&settings_path(), &json)
        .map_err(|error| error.to_string())
}

/// What the importer found, for the review screen ("we imported these N
/// settings") — the user confirms before anything is saved.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObsidianImport {
    pub settings: Settings,
    pub imported: Vec<String>,
}

/// Read `.obsidian/{app,appearance,daily-notes}.json` and map what we
/// understand onto `base`. Read-only with respect to `.obsidian`.
pub fn import_obsidian(vault: &Vault, base: &Settings) -> ObsidianImport {
    let mut settings = base.clone();
    let mut imported = Vec::new();

    let read_json = |name: &str| -> Option<serde_json::Value> {
        let path = NotePath::new(&format!(".obsidian/{name}")).ok()?;
        let bytes = vault.fs().read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    };

    if let Some(app) = read_json("app.json") {
        if let Some(value) = app.get("readableLineLength").and_then(|v| v.as_bool()) {
            settings.readable_line_length = value;
            imported.push("readableLineLength".into());
        }
        if let Some(value) = app.get("useMarkdownLinks").and_then(|v| v.as_bool()) {
            settings.use_markdown_links = value;
            imported.push("useMarkdownLinks".into());
        }
        if let Some(value) = app.get("attachmentFolderPath").and_then(|v| v.as_str()) {
            // "./" prefix means relative-to-note; we only support fixed
            // folders for now — strip the marker, keep the folder.
            settings.attachment_folder =
                value.trim_start_matches("./").trim_matches('/').to_owned();
            imported.push("attachmentFolderPath".into());
        }
        if let Some(value) = app.get("newFileFolderPath").and_then(|v| v.as_str()) {
            settings.new_file_folder = value.trim_matches('/').to_owned();
            imported.push("newFileFolderPath".into());
        }
    }

    if let Some(appearance) = read_json("appearance.json") {
        if let Some(value) = appearance.get("baseFontSize").and_then(|v| v.as_u64()) {
            settings.base_font_size = value.clamp(8, 40) as u16;
            imported.push("baseFontSize".into());
        }
        if let Some(value) = appearance.get("theme").and_then(|v| v.as_str()) {
            // Obsidian: "obsidian" = dark, "moonstone" = light.
            settings.theme = match value {
                "moonstone" => "light".into(),
                "system" => "system".into(),
                _ => "dark".into(),
            };
            imported.push("theme".into());
        }
    }

    if let Some(daily) = read_json("daily-notes.json") {
        if let Some(value) = daily.get("folder").and_then(|v| v.as_str()) {
            settings.daily_note_folder = value.trim_matches('/').to_owned();
            imported.push("dailyNotes.folder".into());
        }
    }

    ObsidianImport { settings, imported }
}

/// Vault path of the daily note for `date` (`YYYY-MM-DD`, computed by the
/// frontend so it respects the user's local timezone).
pub fn daily_note_path(settings: &Settings, date: &str) -> Result<NotePath, String> {
    let valid = date.len() == 10
        && date.bytes().enumerate().all(|(i, byte)| match i {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        });
    if !valid {
        return Err(format!("invalid date: {date}"));
    }
    let path = if settings.daily_note_folder.is_empty() {
        format!("{date}.md")
    } else {
        format!("{}/{date}.md", settings.daily_note_folder)
    };
    NotePath::new(&path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use onyx_core::{MemFs, VaultConfig};

    use super::*;

    fn vault() -> Vault {
        Vault::new(Arc::new(MemFs::new()), VaultConfig::default())
    }

    fn write(vault: &Vault, path: &str, content: &str) {
        vault
            .fs()
            .write_atomic(&NotePath::new(path).unwrap(), content.as_bytes())
            .unwrap();
    }

    #[test]
    fn load_defaults_when_missing_or_corrupt() {
        let vault = vault();
        assert_eq!(load(&vault), Settings::default());
        write(&vault, SETTINGS_PATH, "{not json");
        assert_eq!(load(&vault), Settings::default());
    }

    #[test]
    fn save_load_roundtrip() {
        let vault = vault();
        let settings = Settings {
            base_font_size: 18,
            theme: "light".into(),
            ..Settings::default()
        };
        save(&vault, &settings).unwrap();
        assert_eq!(load(&vault), settings);
    }

    #[test]
    fn unknown_keys_in_file_are_tolerated() {
        let vault = vault();
        write(
            &vault,
            SETTINGS_PATH,
            r#"{"theme":"light","futureSetting":123}"#,
        );
        let settings = load(&vault);
        assert_eq!(settings.theme, "light");
        // Everything else falls back to defaults.
        assert_eq!(settings.base_font_size, 15);
    }

    #[test]
    fn obsidian_import_maps_known_settings() {
        let vault = vault();
        write(
            &vault,
            ".obsidian/app.json",
            r#"{"readableLineLength":false,"useMarkdownLinks":true,
                "attachmentFolderPath":"./assets","newFileFolderPath":"inbox/"}"#,
        );
        write(
            &vault,
            ".obsidian/appearance.json",
            r#"{"baseFontSize":17,"theme":"moonstone"}"#,
        );
        write(
            &vault,
            ".obsidian/daily-notes.json",
            r#"{"folder":"journal"}"#,
        );

        let import = import_obsidian(&vault, &Settings::default());
        assert!(!import.settings.readable_line_length);
        assert!(import.settings.use_markdown_links);
        assert_eq!(import.settings.attachment_folder, "assets");
        assert_eq!(import.settings.new_file_folder, "inbox");
        assert_eq!(import.settings.base_font_size, 17);
        assert_eq!(import.settings.theme, "light");
        assert_eq!(import.settings.daily_note_folder, "journal");
        assert_eq!(import.imported.len(), 7);
    }

    #[test]
    fn obsidian_import_without_config_imports_nothing() {
        let vault = vault();
        let import = import_obsidian(&vault, &Settings::default());
        assert!(import.imported.is_empty());
        assert_eq!(import.settings, Settings::default());
    }

    #[test]
    fn daily_note_paths() {
        let mut settings = Settings::default();
        assert_eq!(
            daily_note_path(&settings, "2026-07-16").unwrap().as_str(),
            "2026-07-16.md"
        );
        settings.daily_note_folder = "journal".into();
        assert_eq!(
            daily_note_path(&settings, "2026-07-16").unwrap().as_str(),
            "journal/2026-07-16.md"
        );
        assert!(daily_note_path(&settings, "not-a-date").is_err());
        assert!(daily_note_path(&settings, "2026-7-16").is_err());
        assert!(daily_note_path(&settings, "2026-07-16'; rm").is_err());
    }
}
