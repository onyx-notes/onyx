//! The metadata index: a SQLite-backed, always-rebuildable cache of what's
//! in the vault.
//!
//! **The index is never the source of truth.** It can be deleted at any
//! moment and rebuilt from the markdown files; a schema-version or salt
//! mismatch does exactly that automatically. The one invariant that
//! matters (and is property-tested): after any sequence of incremental
//! updates, the index is byte-identical to a fresh rebuild.

mod store;

use std::path::Path;

use crate::VaultError;
use crate::events::VaultEvent;
use crate::paths::{NoteId, NotePath};
use crate::vault::Vault;

pub use store::{BacklinkRow, GraphData, GraphNode, IndexError, NoteRecord, TagCount};

/// The vault index. One writer at a time (SQLite connection is not shared);
/// the app layer owns threading.
pub struct Index {
    store: store::Store,
    salt: [u8; 16],
}

impl Index {
    /// Open (or create) an index at `path`. A schema or salt mismatch wipes
    /// and recreates — the index is disposable by design.
    pub fn open(path: &Path, salt: [u8; 16]) -> Result<Self, IndexError> {
        Ok(Self {
            store: store::Store::open(path, salt)?,
            salt,
        })
    }

    /// In-memory index (tests, encrypted vaults keep theirs in RAM too).
    pub fn open_in_memory(salt: [u8; 16]) -> Result<Self, IndexError> {
        Ok(Self {
            store: store::Store::open_in_memory(salt)?,
            salt,
        })
    }

    /// Apply one debounced vault event.
    pub fn handle_event(&mut self, vault: &Vault, event: &VaultEvent) -> Result<(), IndexError> {
        match event {
            VaultEvent::Created(path) | VaultEvent::Modified(path) => self.upsert(vault, path),
            VaultEvent::Removed(path) => self.store.remove(path.note_id(&self.salt)),
            VaultEvent::BulkChange => self.reconcile(vault),
        }
    }

    /// Index (or re-index) a single file from its current on-disk state.
    pub fn upsert(&mut self, vault: &Vault, path: &NotePath) -> Result<(), IndexError> {
        let stat = match vault.fs().stat(path) {
            Ok(stat) => stat,
            // Vanished between event and processing: treat as removal.
            Err(_) => return self.store.remove(path.note_id(&self.salt)),
        };
        // Stat-only fast path: reconcile over a quiet vault never reads
        // file content.
        if self.store.stat_matches(path.note_id(&self.salt), &stat)? {
            return Ok(());
        }
        let Ok(bytes) = vault.fs().read(path) else {
            return self.store.remove(path.note_id(&self.salt));
        };

        let hash = blake3::hash(&bytes);
        let id = path.note_id(&self.salt);

        if self.store.is_current(id, &stat, hash.as_bytes())? {
            return Ok(());
        }

        if path.is_markdown() {
            let text = String::from_utf8_lossy(&bytes);
            let extracted = onyx_md::extract(&text);
            self.store
                .upsert_note(id, path, &stat, hash.as_bytes(), Some(&extracted))
        } else {
            // Attachments are tracked (embeds must resolve, renames must
            // propagate) but carry no extracted structure.
            self.store
                .upsert_note(id, path, &stat, hash.as_bytes(), None)
        }
    }

    /// Reconcile against a full vault scan: index exactly what's on disk.
    /// Used at startup, after watcher storms/gaps, and for full rebuilds.
    pub fn reconcile(&mut self, vault: &Vault) -> Result<(), IndexError> {
        let on_disk = vault.scan().map_err(|error| match error {
            VaultError::Io { source, .. } => IndexError::from(source),
            other => IndexError::Internal(other.to_string()),
        })?;

        let mut disk_ids = Vec::with_capacity(on_disk.len());
        for meta in &on_disk {
            disk_ids.push(meta.id);
            // upsert() skips unchanged files via the (mtime, size, hash)
            // check, so reconcile cost is stat-bound for a quiet vault.
            self.upsert(vault, &meta.path)?;
        }
        self.store.remove_all_except(&disk_ids)
    }

    /// Drop everything and re-index from scratch.
    pub fn rebuild(&mut self, vault: &Vault) -> Result<(), IndexError> {
        self.store.clear()?;
        self.reconcile(vault)
    }

    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    pub fn note(&self, id: NoteId) -> Result<Option<NoteRecord>, IndexError> {
        self.store.note(id)
    }

    pub fn note_count(&self) -> Result<usize, IndexError> {
        self.store.note_count()
    }

    /// Resolve a link target the way Obsidian does: exact vault path
    /// first (with or without `.md`), then unique-enough basename with the
    /// shortest path winning.
    pub fn resolve(&self, target: &str) -> Result<Option<NoteId>, IndexError> {
        self.store.resolve(target)
    }

    /// Notes linking *to* `id`, with the spans of each link occurrence.
    pub fn backlinks(&self, id: NoteId) -> Result<Vec<BacklinkRow>, IndexError> {
        self.store.backlinks(id)
    }

    /// All tags with usage counts (inline + frontmatter), descending.
    pub fn tags(&self) -> Result<Vec<TagCount>, IndexError> {
        self.store.tags()
    }

    /// Link targets that resolve to no note — "create this note" material.
    pub fn unresolved_targets(&self) -> Result<Vec<String>, IndexError> {
        self.store.unresolved_targets()
    }

    /// Bulk node + edge data for [`crate::LinkGraph`] construction.
    pub fn graph_data(&self) -> Result<GraphData, IndexError> {
        self.store.graph_data()
    }

    /// Canonical dump of all indexed state, for equality assertions in
    /// tests (incremental == rebuild).
    pub fn dump(&self) -> Result<String, IndexError> {
        self.store.dump()
    }
}
