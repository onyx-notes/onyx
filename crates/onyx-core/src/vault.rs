//! The vault: the orchestrating façade over filesystem, journal, and
//! (coming next) the index.

use std::sync::Arc;
use std::time::Instant;

use crate::fs::{FileStat, VaultFs};
use crate::journal::WriteJournal;
use crate::paths::{NoteId, NotePath};
use crate::{VaultError, VaultEvent};

/// Per-vault configuration.
#[derive(Debug, Clone, Default)]
pub struct VaultConfig {
    /// Salt for note identities. Persisted with the vault (once sync
    /// arrives) so ids stay stable across devices; zero salt is valid for
    /// purely local use.
    pub salt: [u8; 16],
}

/// A file known to the vault: identity plus change-detection metadata.
#[derive(Debug, Clone)]
pub struct NoteMeta {
    pub path: NotePath,
    pub id: NoteId,
    pub stat: FileStat,
}

/// A vault of markdown files. Cheap to clone-share via `Arc<dyn VaultFs>`.
pub struct Vault {
    fs: Arc<dyn VaultFs>,
    config: VaultConfig,
    journal: WriteJournal,
}

impl Vault {
    pub fn new(fs: Arc<dyn VaultFs>, config: VaultConfig) -> Self {
        Self {
            fs,
            config,
            journal: WriteJournal::new(),
        }
    }

    pub fn fs(&self) -> &Arc<dyn VaultFs> {
        &self.fs
    }

    pub fn note_id(&self, path: &NotePath) -> NoteId {
        path.note_id(&self.config.salt)
    }

    /// Enumerate all visible files (hidden dirs like `.onyx`, `.git`,
    /// `.obsidian` excluded).
    pub fn scan(&self) -> Result<Vec<NoteMeta>, VaultError> {
        let listed = self.fs.list().map_err(|source| VaultError::Io {
            path: "<vault root>".into(),
            source,
        })?;
        Ok(listed
            .into_iter()
            .filter(|(path, _)| !path.is_hidden())
            .map(|(path, stat)| NoteMeta {
                id: self.note_id(&path),
                path,
                stat,
            })
            .collect())
    }

    pub fn read_bytes(&self, path: &NotePath) -> Result<Vec<u8>, VaultError> {
        self.fs.read(path).map_err(|source| VaultError::Io {
            path: path.to_string(),
            source,
        })
    }

    /// Read a note as text. Invalid UTF-8 is replaced, never fatal — notes
    /// are user files and may be half-written by other tools.
    pub fn read_text(&self, path: &NotePath) -> Result<String, VaultError> {
        Ok(String::from_utf8_lossy(&self.read_bytes(path)?).into_owned())
    }

    /// Write a note atomically, recording it in the journal so the
    /// resulting watcher event is recognized as our own echo.
    pub fn write(&self, path: &NotePath, content: &[u8]) -> Result<(), VaultError> {
        self.journal
            .record(path.key(), blake3::hash(content), Instant::now());
        self.fs
            .write_atomic(path, content)
            .map_err(|source| VaultError::Io {
                path: path.to_string(),
                source,
            })
    }

    pub fn remove(&self, path: &NotePath) -> Result<(), VaultError> {
        self.fs.remove(path).map_err(|source| VaultError::Io {
            path: path.to_string(),
            source,
        })
    }

    pub fn rename(&self, from: &NotePath, to: &NotePath) -> Result<(), VaultError> {
        self.fs.rename(from, to).map_err(|source| VaultError::Io {
            path: from.to_string(),
            source,
        })
    }

    /// Is this event just our own write coming back through the watcher?
    ///
    /// `Created`/`Modified` events hash the current file content and check
    /// the journal. `Removed` and `BulkChange` are never echoes.
    pub fn is_own_echo(&self, event: &VaultEvent) -> bool {
        let path = match event {
            VaultEvent::Created(path) | VaultEvent::Modified(path) => path,
            VaultEvent::Removed(_) | VaultEvent::BulkChange => return false,
        };
        let Ok(content) = self.fs.read(path) else {
            // File vanished between event and check; let the event through
            // so consumers reconcile against reality.
            return false;
        };
        self.journal
            .is_echo(&path.key(), blake3::hash(&content), Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::MemFs;

    fn vault() -> Vault {
        Vault::new(Arc::new(MemFs::new()), VaultConfig::default())
    }

    fn path(text: &str) -> NotePath {
        NotePath::new(text).unwrap()
    }

    #[test]
    fn write_read_roundtrip() {
        let vault = vault();
        let note = path("notes/hello.md");
        vault.write(&note, "# Hello\n".as_bytes()).unwrap();
        assert_eq!(vault.read_text(&note).unwrap(), "# Hello\n");
    }

    #[test]
    fn scan_filters_hidden() {
        let vault = vault();
        vault.write(&path("visible.md"), b"x").unwrap();
        vault.write(&path(".obsidian/app.json"), b"{}").unwrap();
        vault.write(&path(".onyx/index.db"), b"bin").unwrap();
        let metas = vault.scan().unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].path.as_str(), "visible.md");
    }

    #[test]
    fn own_write_is_echo_external_edit_is_not() {
        let vault = vault();
        let note = path("a.md");
        vault.write(&note, b"ours").unwrap();
        assert!(vault.is_own_echo(&VaultEvent::Modified(note.clone())));
        // Echo consumed; the same event again is external.
        assert!(!vault.is_own_echo(&VaultEvent::Modified(note.clone())));

        // External edit (bypassing Vault::write) is never an echo.
        vault.write(&note, b"ours again").unwrap();
        vault.fs().write_atomic(&note, b"theirs").unwrap();
        assert!(!vault.is_own_echo(&VaultEvent::Modified(note)));
    }

    #[test]
    fn echo_on_missing_file_is_false() {
        let vault = vault();
        assert!(!vault.is_own_echo(&VaultEvent::Modified(path("gone.md"))));
        assert!(!vault.is_own_echo(&VaultEvent::BulkChange));
    }

    #[test]
    fn read_invalid_utf8_is_lossy_not_fatal() {
        let vault = vault();
        let note = path("bin.md");
        vault.write(&note, &[0x66, 0xFF, 0x6F]).unwrap();
        assert_eq!(vault.read_text(&note).unwrap(), "f\u{FFFD}o");
    }
}
