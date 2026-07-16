//! Real filesystem watching: wires `notify` into the [`Coalescer`] and
//! delivers debounced [`VaultEvent`]s to a channel.
//!
//! The watcher thread owns the coalescer and sleeps exactly until its next
//! deadline — no polling loops, no busy waking.

use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, Sender};
use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecursiveMode, Watcher};

use crate::VaultError;
use crate::coalescer::{Coalescer, CoalescerConfig, RawEvent};
use crate::events::VaultEvent;
use crate::paths::NotePath;

/// Maps on-disk names to vault names. Encrypted vaults translate
/// ciphertext tokens back to plaintext paths; `None` drops the event
/// (foreign files that aren't part of the vault).
pub type PathTranslator = std::sync::Arc<dyn Fn(&NotePath) -> Option<NotePath> + Send + Sync>;

/// A running watcher. Dropping it stops the background thread.
pub struct VaultWatcher {
    // Option so Drop can drop the notify watcher FIRST (disconnecting the
    // raw-event channel and terminating the thread) before joining. Rust
    // drops fields only after Drop::drop returns, so relying on field
    // order alone would deadlock the join.
    watcher: Option<notify::RecommendedWatcher>,
    thread: Option<JoinHandle<()>>,
}

impl VaultWatcher {
    /// Watch `root` recursively, sending debounced events to `output`.
    pub fn spawn(
        root: &Path,
        config: CoalescerConfig,
        output: Sender<VaultEvent>,
    ) -> Result<Self, VaultError> {
        Self::spawn_translated(root, config, output, None)
    }

    /// Like [`Self::spawn`], with an optional on-disk → vault path
    /// translator (encrypted vaults).
    pub fn spawn_translated(
        root: &Path,
        config: CoalescerConfig,
        output: Sender<VaultEvent>,
        translator: Option<PathTranslator>,
    ) -> Result<Self, VaultError> {
        let (raw_sender, raw_receiver) = crossbeam_channel::unbounded();
        let mut watcher = notify::recommended_watcher(move |result| {
            // Send failures mean the consumer thread is gone; nothing to do.
            let _ = raw_sender.send(result);
        })
        .map_err(|error| VaultError::Watcher(error.to_string()))?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| VaultError::Watcher(error.to_string()))?;

        let root = root.to_path_buf();
        let thread = std::thread::Builder::new()
            .name("onyx-watcher".into())
            .spawn(move || run_loop(&root, config, &raw_receiver, &output, translator.as_ref()))
            .map_err(|error| VaultError::Watcher(error.to_string()))?;

        Ok(Self {
            watcher: Some(watcher),
            thread: Some(thread),
        })
    }
}

impl Drop for VaultWatcher {
    fn drop(&mut self) {
        // Disconnect the raw channel first, then reap the thread.
        drop(self.watcher.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Maximum sleep while idle — bounds shutdown latency if the raw channel
/// somehow stays open without traffic.
const IDLE_WAIT: Duration = Duration::from_millis(500);

fn run_loop(
    root: &Path,
    config: CoalescerConfig,
    raw: &crossbeam_channel::Receiver<Result<notify::Event, notify::Error>>,
    output: &Sender<VaultEvent>,
    translator: Option<&PathTranslator>,
) {
    let mut coalescer = Coalescer::new(config);

    loop {
        let wait = coalescer
            .next_deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(IDLE_WAIT)
            .min(IDLE_WAIT);

        match raw.recv_timeout(wait) {
            Ok(Ok(event)) => {
                let now = Instant::now();
                for raw_event in translate(root, &event, translator) {
                    coalescer.push(raw_event, now);
                }
            }
            Ok(Err(error)) => {
                // Watcher backend error (often overflow): the only safe
                // recovery is a full rescan.
                tracing::warn!(?error, "watcher backend error; requesting rescan");
                if output.send(VaultEvent::BulkChange).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }

        for event in coalescer.poll(Instant::now()) {
            if output.send(event).is_err() {
                return;
            }
        }
    }
}

/// Map a notify event to raw vault events, dropping paths outside the vault
/// model (hidden dirs, non-UTF-8, directories themselves, untranslatable
/// names in encrypted vaults).
fn translate(
    root: &Path,
    event: &notify::Event,
    translator: Option<&PathTranslator>,
) -> Vec<RawEvent> {
    let mut raw_events = Vec::new();
    let mut push = |path: &PathBuf, kind: fn(NotePath) -> RawEvent| {
        let on_disk = to_note_path(root, path);
        let vault_path = match translator {
            Some(translate_name) => on_disk.and_then(|name| translate_name(&name)),
            None => on_disk,
        };
        if let Some(note_path) = vault_path {
            raw_events.push(kind(note_path));
        }
    };

    match &event.kind {
        EventKind::Create(_) => {
            for path in &event.paths {
                push(path, RawEvent::Created);
            }
        }
        EventKind::Remove(_) => {
            for path in &event.paths {
                push(path, RawEvent::Removed);
            }
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            for path in &event.paths {
                push(path, RawEvent::Removed);
            }
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            for path in &event.paths {
                push(path, RawEvent::Created);
            }
        }
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() == 2 => {
            push(&event.paths[0], RawEvent::Removed);
            push(&event.paths[1], RawEvent::Created);
        }
        EventKind::Modify(_) | EventKind::Any | EventKind::Other => {
            for path in &event.paths {
                // A directory mtime change is not a note change.
                if path.is_dir() {
                    continue;
                }
                push(path, RawEvent::Written);
            }
        }
        EventKind::Access(_) => {}
    }

    raw_events
}

fn to_note_path(root: &Path, absolute: &Path) -> Option<NotePath> {
    let relative = absolute.strip_prefix(root).ok()?;
    let note_path = NotePath::new(relative.to_str()?).ok()?;
    (!note_path.is_hidden()).then_some(note_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_filters_hidden_and_foreign_paths() {
        let root = PathBuf::from("/vault");
        let event = notify::Event {
            kind: EventKind::Create(notify::event::CreateKind::File),
            paths: vec![
                PathBuf::from("/vault/note.md"),
                PathBuf::from("/vault/.onyx/index.db"),
                PathBuf::from("/vault/.git/HEAD"),
                PathBuf::from("/elsewhere/other.md"),
            ],
            attrs: Default::default(),
        };
        let raw = translate(&root, &event, None);
        assert_eq!(
            raw,
            vec![RawEvent::Created(NotePath::new("note.md").unwrap())]
        );
    }

    #[test]
    fn translate_rename_both_becomes_remove_and_create() {
        let root = PathBuf::from("/vault");
        let event = notify::Event {
            kind: EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            paths: vec![
                PathBuf::from("/vault/old.md"),
                PathBuf::from("/vault/new.md"),
            ],
            attrs: Default::default(),
        };
        let raw = translate(&root, &event, None);
        assert_eq!(
            raw,
            vec![
                RawEvent::Removed(NotePath::new("old.md").unwrap()),
                RawEvent::Created(NotePath::new("new.md").unwrap()),
            ]
        );
    }
}
