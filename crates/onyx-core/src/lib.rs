//! The Onyx vault engine.
//!
//! A vault is a directory of plain markdown files — the **single source of
//! truth**. Everything this crate builds on top (in-memory state, indexes)
//! is a rebuildable cache of those bytes, and all arrows point one way:
//!
//! ```text
//! disk .md files ──(watcher / own writes)──▶ vault state ──▶ index ──▶ UI
//! ```
//!
//! Design invariants:
//!
//! - **Writes are atomic**: temp file + fsync + rename; a crash can never
//!   leave a torn note.
//! - **Our own writes never echo**: a write journal records the content
//!   hash of every write so the resulting watcher event is swallowed.
//! - **Paths are identities**: NFC-normalized, casefold-keyed — the same
//!   note is the same note on macOS (NFD), Windows (case-insensitive), and
//!   Linux.
//! - **Storms degrade gracefully**: a git checkout touching 10k files
//!   yields one `BulkChange` rescan signal, not 10k events.

mod coalescer;
mod cryptofs;
mod events;
mod fs;
mod graph;
mod history;
mod index;
mod journal;
mod paths;
mod query;
mod quick;
mod search;
mod vault;
mod watcher;

pub use coalescer::{Coalescer, CoalescerConfig, RawEvent};
pub use cryptofs::CryptoFs;
pub use events::VaultEvent;
pub use fs::{FileStat, MemFs, RealFs, VaultFs};
pub use graph::LinkGraph;
pub use history::{History, HistoryError, Version};
pub use index::{
    BacklinkRow, GraphData, GraphNode, HeadingRow, Index, IndexError, NoteRecord, QueryRow,
    TagCount,
};
pub use journal::WriteJournal;
pub use paths::{NoteId, NotePath, PathError};
pub use query::{QueryResult, run_query};
pub use quick::{QuickHit, QuickSwitcher};
pub use search::{SearchError, SearchHit, SearchIndex};
pub use vault::{NoteMeta, Vault, VaultConfig};
pub use watcher::{PathTranslator, VaultWatcher};

/// Errors from vault operations.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("I/O error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to start file watcher: {0}")]
    Watcher(String),
}
