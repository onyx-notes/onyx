//! Vault change events — the one currency between watcher, indexer, and UI.

use crate::paths::NotePath;

/// A change observed in the vault directory, after debouncing, ignore
/// filtering, and echo suppression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultEvent {
    /// A file appeared.
    Created(NotePath),
    /// A file's content changed.
    Modified(NotePath),
    /// A file disappeared.
    Removed(NotePath),
    /// Too many files changed at once (git checkout, cloud-sync client…).
    /// Consumers must do one bulk stat-scan reconciliation instead of
    /// processing per-file events.
    BulkChange,
}

impl VaultEvent {
    /// The affected path, if this is a per-file event.
    pub fn path(&self) -> Option<&NotePath> {
        match self {
            Self::Created(path) | Self::Modified(path) | Self::Removed(path) => Some(path),
            Self::BulkChange => None,
        }
    }
}
