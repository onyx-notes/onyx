//! The engine: one open vault wired to its index, search, and
//! quick-switcher.
//!
//! Deliberately free of Tauri types so it can be tested headless and reused
//! by the mobile shells. The Tauri layer owns windows, IPC, and event
//! emission; the engine owns correctness.
//!
//! Update discipline: the vault's own writes update the index *synchronously*
//! in the writing call (the write journal then swallows the watcher echo).
//! External edits arrive via watcher events. Both paths converge on
//! [`Engine::apply_event`], so there is exactly one way state changes.

use std::path::{Path, PathBuf};

use onyx_core::{
    Index, NotePath, QuickSwitcher, RealFs, SearchIndex, Vault, VaultConfig, VaultEvent,
};

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Vault(#[from] onyx_core::VaultError),
    #[error("{0}")]
    Index(#[from] onyx_core::IndexError),
    #[error("{0}")]
    Search(#[from] onyx_core::SearchError),
    #[error("invalid path: {0}")]
    Path(#[from] onyx_core::PathError),
    #[error("no vault is open")]
    NoVault,
}

pub struct Engine {
    root: PathBuf,
    vault: Vault,
    index: Index,
    search: SearchIndex,
    quick: QuickSwitcher,
    /// Search commits are debounced by the caller; this tracks dirtiness.
    search_dirty: bool,
}

impl Engine {
    /// Open a vault directory: reconcile the on-disk index, then build the
    /// in-memory search and quick-switcher state.
    pub fn open(root: &Path) -> Result<Self, EngineError> {
        let vault = Vault::new(
            std::sync::Arc::new(RealFs::new(root)),
            VaultConfig::default(),
        );
        let mut index = Index::open(&root.join(".onyx/index.db"), [0; 16])?;
        index.reconcile(&vault)?;

        let mut search = SearchIndex::open_in_dir(&root.join(".onyx/tantivy"))?;
        let mut quick = QuickSwitcher::new();
        for record in index.all_notes()? {
            quick.upsert(record.id, &record.title, record.path.as_str(), &[]);
            if record.is_markdown {
                let body = vault.read_text(&record.path)?;
                search.upsert(record.id, record.path.as_str(), &record.title, &body, &[])?;
            }
        }
        search.commit()?;

        Ok(Self {
            root: root.to_path_buf(),
            vault,
            index,
            search,
            quick,
            search_dirty: false,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    pub fn quick(&self) -> &QuickSwitcher {
        &self.quick
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<onyx_core::SearchHit>, EngineError> {
        Ok(self.search.search(query, limit)?)
    }

    /// The single state-update path: apply one vault event to index,
    /// quick-switcher, and full-text search. Returns whether this was our
    /// own write echoing back (callers skip UI refresh for those).
    pub fn apply_event(&mut self, event: &VaultEvent) -> Result<bool, EngineError> {
        if self.vault.is_own_echo(event) {
            return Ok(true);
        }
        self.index.handle_event(&self.vault, event)?;

        match event {
            VaultEvent::Created(path) | VaultEvent::Modified(path) => {
                let id = self.vault.note_id(path);
                match self.index.note(id)? {
                    Some(record) => {
                        self.quick
                            .upsert(id, &record.title, record.path.as_str(), &[]);
                        if record.is_markdown {
                            let body = self.vault.read_text(&record.path)?;
                            self.search.upsert(
                                id,
                                record.path.as_str(),
                                &record.title,
                                &body,
                                &[],
                            )?;
                            self.search_dirty = true;
                        }
                    }
                    // Vanished before we processed the event.
                    None => self.forget(id)?,
                }
            }
            VaultEvent::Removed(path) => {
                let id = self.vault.note_id(path);
                self.forget(id)?;
            }
            VaultEvent::BulkChange => {
                // Reconcile already ran in handle_event; rebuild the
                // in-memory views from the reconciled index.
                self.quick = QuickSwitcher::new();
                for record in self.index.all_notes()? {
                    self.quick
                        .upsert(record.id, &record.title, record.path.as_str(), &[]);
                    if record.is_markdown {
                        let body = self.vault.read_text(&record.path)?;
                        self.search.upsert(
                            record.id,
                            record.path.as_str(),
                            &record.title,
                            &body,
                            &[],
                        )?;
                    }
                }
                self.search_dirty = true;
            }
        }
        Ok(false)
    }

    fn forget(&mut self, id: onyx_core::NoteId) -> Result<(), EngineError> {
        self.quick.remove(id);
        self.search.remove(id)?;
        self.search_dirty = true;
        Ok(())
    }

    /// Flush pending search changes (debounced by the event loop).
    pub fn commit_search_if_dirty(&mut self) -> Result<(), EngineError> {
        if self.search_dirty {
            self.search.commit()?;
            self.search_dirty = false;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Write operations (used by IPC commands; index updates synchronously)
    // ------------------------------------------------------------------

    pub fn write_note(&mut self, path: &NotePath, content: &str) -> Result<(), EngineError> {
        let existed = self.vault.fs().exists(path);
        self.vault.write(path, content.as_bytes())?;
        let event = if existed {
            VaultEvent::Modified(path.clone())
        } else {
            VaultEvent::Created(path.clone())
        };
        // Bypass echo detection: we *want* this update applied here; the
        // journal entry exists to swallow the upcoming watcher echo.
        self.index.handle_event(&self.vault, &event)?;
        let id = self.vault.note_id(path);
        if let Some(record) = self.index.note(id)? {
            self.quick
                .upsert(id, &record.title, record.path.as_str(), &[]);
            if record.is_markdown {
                self.search
                    .upsert(id, record.path.as_str(), &record.title, content, &[])?;
                self.search_dirty = true;
            }
        }
        Ok(())
    }

    pub fn delete_note(&mut self, path: &NotePath) -> Result<(), EngineError> {
        self.vault.remove(path)?;
        let id = self.vault.note_id(path);
        self.index
            .handle_event(&self.vault, &VaultEvent::Removed(path.clone()))?;
        self.forget(id)
    }

    pub fn rename_note(&mut self, from: &NotePath, to: &NotePath) -> Result<(), EngineError> {
        self.vault.rename(from, to)?;
        let from_id = self.vault.note_id(from);
        self.index
            .handle_event(&self.vault, &VaultEvent::Removed(from.clone()))?;
        self.forget(from_id)?;
        // Reuse the write path's indexing via a synthetic Created event.
        self.index
            .handle_event(&self.vault, &VaultEvent::Created(to.clone()))?;
        let to_id = self.vault.note_id(to);
        if let Some(record) = self.index.note(to_id)? {
            self.quick
                .upsert(to_id, &record.title, record.path.as_str(), &[]);
            if record.is_markdown {
                let body = self.vault.read_text(&record.path)?;
                self.search
                    .upsert(to_id, record.path.as_str(), &record.title, &body, &[])?;
                self.search_dirty = true;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(path: &str) -> NotePath {
        NotePath::new(path).unwrap()
    }

    fn open_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let target = dir.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, content).unwrap();
        }
        let engine = Engine::open(dir.path()).unwrap();
        (dir, engine)
    }

    #[test]
    fn open_indexes_existing_vault() {
        let (_dir, engine) = open_with(&[
            ("a.md", "# Alpha\nsearchable-alpha content [[Beta]]"),
            ("sub/Beta.md", "# Beta"),
            (".obsidian/app.json", "{}"),
        ]);
        assert_eq!(engine.index().note_count().unwrap(), 2);
        assert_eq!(engine.search("searchable-alpha", 5).unwrap().len(), 1);
        // Quick-switcher matches titles = filename stems (Obsidian semantics).
        assert_eq!(engine.quick().query("beta", 5).len(), 1);
    }

    #[test]
    fn write_note_is_immediately_visible_everywhere() {
        let (_dir, mut engine) = open_with(&[]);
        engine
            .write_note(&note("fresh.md"), "# Fresh\nbrand-new-token")
            .unwrap();
        engine.commit_search_if_dirty().unwrap();

        assert_eq!(engine.index().note_count().unwrap(), 1);
        assert_eq!(engine.search("brand-new-token", 5).unwrap().len(), 1);
        assert_eq!(engine.quick().query("fresh", 5).len(), 1);
        // And the bytes are really on disk.
        assert_eq!(
            engine.vault().read_text(&note("fresh.md")).unwrap(),
            "# Fresh\nbrand-new-token"
        );
    }

    #[test]
    fn own_write_watcher_echo_is_detected_once() {
        let (dir, mut engine) = open_with(&[]);
        engine.write_note(&note("a.md"), "content").unwrap();
        // The watcher will deliver this event; apply_event must flag it as
        // our echo (and not double-apply).
        let echo = engine
            .apply_event(&VaultEvent::Created(note("a.md")))
            .unwrap();
        assert!(echo);
        // A genuinely external change is not an echo.
        std::fs::write(dir.path().join("a.md"), "external edit").unwrap();
        let echo = engine
            .apply_event(&VaultEvent::Modified(note("a.md")))
            .unwrap();
        assert!(!echo);
    }

    #[test]
    fn external_event_updates_all_views() {
        let (dir, mut engine) = open_with(&[]);
        std::fs::write(dir.path().join("External.md"), "# External\nxyzzy-token").unwrap();
        engine
            .apply_event(&VaultEvent::Created(note("External.md")))
            .unwrap();
        engine.commit_search_if_dirty().unwrap();
        assert_eq!(engine.search("xyzzy-token", 5).unwrap().len(), 1);
        assert_eq!(engine.quick().query("external", 5).len(), 1);
    }

    #[test]
    fn delete_and_rename_propagate() {
        let (_dir, mut engine) = open_with(&[("old.md", "# Old\nfindme-token")]);
        engine
            .rename_note(&note("old.md"), &note("new.md"))
            .unwrap();
        engine.commit_search_if_dirty().unwrap();

        assert!(engine.quick().query("old", 5).is_empty());
        assert_eq!(engine.quick().query("new", 5).len(), 1);
        let hits = engine.search("findme-token", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "new.md");

        engine.delete_note(&note("new.md")).unwrap();
        engine.commit_search_if_dirty().unwrap();
        assert_eq!(engine.index().note_count().unwrap(), 0);
        assert!(engine.search("findme-token", 5).unwrap().is_empty());
    }

    #[test]
    fn reopen_reuses_persisted_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "# A\npersisted-token").unwrap();
        {
            let _first = Engine::open(dir.path()).unwrap();
        }
        // Second open: index.db and tantivy dir already exist.
        let engine = Engine::open(dir.path()).unwrap();
        assert_eq!(engine.index().note_count().unwrap(), 1);
        assert_eq!(engine.search("persisted-token", 5).unwrap().len(), 1);
    }
}
