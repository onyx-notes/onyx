//! Event coalescer: turns a firehose of raw filesystem notifications into
//! calm, deduplicated [`VaultEvent`]s.
//!
//! Three jobs:
//!
//! 1. **Debounce** — editors save in bursts (write + truncate + write);
//!    per-path events within the debounce window merge into one.
//! 2. **Merge semantics** — `create then remove` cancels out, `remove then
//!    create` is a modification, etc.
//! 3. **Storm detection** — when too many distinct paths change at once
//!    (git checkout, cloud-sync client), per-file processing would melt the
//!    indexer. All pending events collapse into one [`VaultEvent::BulkChange`]
//!    emitted after the storm quiets down.
//!
//! The coalescer is a pure state machine: callers push raw events with a
//! timestamp and poll for due events with a timestamp. No threads, no
//! clocks — fully deterministic under test. The watcher owns the real-time
//! driving loop.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::events::VaultEvent;
use crate::paths::NotePath;

/// A raw, undebounced filesystem notification (already ignore-filtered and
/// mapped to vault-relative paths by the watcher).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawEvent {
    Created(NotePath),
    Written(NotePath),
    Removed(NotePath),
}

#[derive(Debug, Clone, Copy)]
pub struct CoalescerConfig {
    /// Quiet time required after the last event on a path before it fires.
    pub debounce: Duration,
    /// Distinct dirty paths within `storm_window` that trigger storm mode.
    pub storm_threshold: usize,
    /// Sliding window for storm detection, and the quiet time required to
    /// leave storm mode.
    pub storm_window: Duration,
}

impl Default for CoalescerConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(150),
            storm_threshold: 500,
            storm_window: Duration::from_secs(2),
        }
    }
}

/// What we believe happened to a path, merged over the debounce window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Created,
    Modified,
    Removed,
}

impl Pending {
    /// Merge a new raw observation into the pending state.
    /// `None` means the events cancel out entirely (created then removed).
    fn merge(previous: Option<Self>, incoming: Self) -> Option<Self> {
        use Pending::*;
        Some(match (previous, incoming) {
            (None, event) => event,
            // Still unseen by consumers, so edits keep it "created".
            (Some(Created), Created | Modified) => Created,
            (Some(Created), Removed) => return None,
            (Some(Modified), Created | Modified) => Modified,
            (Some(Modified), Removed) => Removed,
            // Removed then recreated: content may differ — a modification.
            (Some(Removed), Created | Modified) => Modified,
            (Some(Removed), Removed) => Removed,
        })
    }
}

#[derive(Debug)]
struct Entry {
    path: NotePath,
    pending: Pending,
    deadline: Instant,
}

pub struct Coalescer {
    config: CoalescerConfig,
    /// Keyed by `NotePath::key()` so case/normalization variants of one
    /// note coalesce together.
    entries: HashMap<String, Entry>,
    /// (first-seen-in-window timestamps per path key) for storm detection.
    window: HashMap<String, Instant>,
    /// Set while in storm mode: the time of the most recent raw event.
    storm_last_event: Option<Instant>,
}

impl Coalescer {
    pub fn new(config: CoalescerConfig) -> Self {
        Self {
            config,
            entries: HashMap::new(),
            window: HashMap::new(),
            storm_last_event: None,
        }
    }

    /// Feed a raw event observed at `now`.
    pub fn push(&mut self, event: RawEvent, now: Instant) {
        if self.storm_last_event.is_some() {
            // In storm mode only the quiet-time clock matters.
            self.storm_last_event = Some(now);
            return;
        }

        let (path, pending) = match event {
            RawEvent::Created(path) => (path, Pending::Created),
            RawEvent::Written(path) => (path, Pending::Modified),
            RawEvent::Removed(path) => (path, Pending::Removed),
        };
        let key = path.key();

        // Storm bookkeeping: count distinct paths in the sliding window.
        self.window
            .retain(|_, seen| now.duration_since(*seen) < self.config.storm_window);
        self.window.entry(key.clone()).or_insert(now);
        if self.window.len() >= self.config.storm_threshold {
            self.entries.clear();
            self.window.clear();
            self.storm_last_event = Some(now);
            return;
        }

        let previous = self.entries.remove(&key).map(|entry| entry.pending);
        if let Some(merged) = Pending::merge(previous, pending) {
            self.entries.insert(
                key,
                Entry {
                    path,
                    pending: merged,
                    deadline: now + self.config.debounce,
                },
            );
        }
    }

    /// Collect events whose debounce window has elapsed at `now`.
    pub fn poll(&mut self, now: Instant) -> Vec<VaultEvent> {
        if let Some(last) = self.storm_last_event {
            // Leave storm mode only after a full quiet window, then tell
            // consumers to rescan once.
            if now.duration_since(last) >= self.config.storm_window {
                self.storm_last_event = None;
                return vec![VaultEvent::BulkChange];
            }
            return Vec::new();
        }

        let mut due: Vec<Entry> = Vec::new();
        self.entries.retain(|_, entry| {
            if entry.deadline <= now {
                due.push(Entry {
                    path: entry.path.clone(),
                    pending: entry.pending,
                    deadline: entry.deadline,
                });
                false
            } else {
                true
            }
        });

        // Deterministic emission order (oldest first, then path).
        due.sort_by(|a, b| a.deadline.cmp(&b.deadline).then(a.path.cmp(&b.path)));
        due.into_iter()
            .map(|entry| match entry.pending {
                Pending::Created => VaultEvent::Created(entry.path),
                Pending::Modified => VaultEvent::Modified(entry.path),
                Pending::Removed => VaultEvent::Removed(entry.path),
            })
            .collect()
    }

    /// When the next `poll` could produce something — the watcher sleeps
    /// until then. `None` when idle.
    pub fn next_deadline(&self) -> Option<Instant> {
        let storm = self
            .storm_last_event
            .map(|last| last + self.config.storm_window);
        let entry = self.entries.values().map(|entry| entry.deadline).min();
        match (storm, entry) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> NotePath {
        NotePath::new(text).unwrap()
    }

    fn config() -> CoalescerConfig {
        CoalescerConfig {
            debounce: Duration::from_millis(150),
            storm_threshold: 5,
            storm_window: Duration::from_secs(2),
        }
    }

    #[test]
    fn debounces_rapid_writes_into_one_event() {
        let mut coalescer = Coalescer::new(config());
        let start = Instant::now();
        for offset in [0, 20, 40, 60] {
            coalescer.push(
                RawEvent::Written(path("a.md")),
                start + Duration::from_millis(offset),
            );
        }
        // Not due yet at +100ms (last event +60ms, debounce 150ms).
        assert!(
            coalescer
                .poll(start + Duration::from_millis(100))
                .is_empty()
        );
        let due = coalescer.poll(start + Duration::from_millis(211));
        assert_eq!(due, vec![VaultEvent::Modified(path("a.md"))]);
        // Nothing left.
        assert!(coalescer.poll(start + Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn create_then_write_is_created() {
        let mut coalescer = Coalescer::new(config());
        let start = Instant::now();
        coalescer.push(RawEvent::Created(path("a.md")), start);
        coalescer.push(RawEvent::Written(path("a.md")), start);
        assert_eq!(
            coalescer.poll(start + Duration::from_secs(1)),
            vec![VaultEvent::Created(path("a.md"))]
        );
    }

    #[test]
    fn create_then_remove_cancels() {
        let mut coalescer = Coalescer::new(config());
        let start = Instant::now();
        coalescer.push(RawEvent::Created(path("tmp.md")), start);
        coalescer.push(RawEvent::Removed(path("tmp.md")), start);
        assert!(coalescer.poll(start + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn remove_then_create_is_modified() {
        // Safe-save editors (vim default) replace files this way.
        let mut coalescer = Coalescer::new(config());
        let start = Instant::now();
        coalescer.push(RawEvent::Removed(path("a.md")), start);
        coalescer.push(RawEvent::Created(path("a.md")), start);
        assert_eq!(
            coalescer.poll(start + Duration::from_secs(1)),
            vec![VaultEvent::Modified(path("a.md"))]
        );
    }

    #[test]
    fn case_variants_coalesce_to_one_note() {
        let mut coalescer = Coalescer::new(config());
        let start = Instant::now();
        coalescer.push(RawEvent::Written(path("Note.md")), start);
        coalescer.push(RawEvent::Written(path("note.md")), start);
        let due = coalescer.poll(start + Duration::from_secs(1));
        assert_eq!(due.len(), 1);
    }

    #[test]
    fn storm_collapses_to_bulk_change() {
        let mut coalescer = Coalescer::new(config());
        let start = Instant::now();
        for index in 0..10 {
            coalescer.push(RawEvent::Written(path(&format!("f{index}.md"))), start);
        }
        // Mid-storm: nothing until quiet.
        assert!(
            coalescer
                .poll(start + Duration::from_millis(500))
                .is_empty()
        );
        // Still events arriving — quiet clock resets.
        coalescer.push(
            RawEvent::Written(path("more.md")),
            start + Duration::from_secs(1),
        );
        assert!(coalescer.poll(start + Duration::from_secs(2)).is_empty());
        // Quiet for a full window → exactly one BulkChange.
        let due = coalescer.poll(start + Duration::from_secs(4));
        assert_eq!(due, vec![VaultEvent::BulkChange]);
        assert!(coalescer.poll(start + Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn storm_discards_pending_per_file_events() {
        let mut coalescer = Coalescer::new(config());
        let start = Instant::now();
        coalescer.push(RawEvent::Written(path("early.md")), start);
        for index in 0..10 {
            coalescer.push(RawEvent::Written(path(&format!("f{index}.md"))), start);
        }
        let due = coalescer.poll(start + Duration::from_secs(5));
        assert_eq!(due, vec![VaultEvent::BulkChange]);
    }

    #[test]
    fn slow_trickle_across_windows_is_not_a_storm() {
        let mut coalescer = Coalescer::new(config());
        let start = Instant::now();
        // 12 files over 12 seconds: window only ever holds ~2.
        for index in 0..12u64 {
            coalescer.push(
                RawEvent::Written(path(&format!("f{index}.md"))),
                start + Duration::from_secs(index),
            );
        }
        let due = coalescer.poll(start + Duration::from_secs(30));
        assert_eq!(due.len(), 12);
        assert!(due.iter().all(|event| event != &VaultEvent::BulkChange));
    }

    #[test]
    fn next_deadline_tracks_pending_work() {
        let mut coalescer = Coalescer::new(config());
        assert_eq!(coalescer.next_deadline(), None);
        let start = Instant::now();
        coalescer.push(RawEvent::Written(path("a.md")), start);
        assert_eq!(
            coalescer.next_deadline(),
            Some(start + Duration::from_millis(150))
        );
    }
}
