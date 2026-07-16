//! Write journal: echo suppression for the vault's own writes.
//!
//! Every write the vault performs is recorded here as `(path key, content
//! hash)`. When the file watcher later reports that path, the vault hashes
//! the file and asks the journal: *is this just our own write coming back?*
//! If so, the event is swallowed — otherwise a save would trigger a reindex
//! and, worse, a sync engine would echo changes in a loop.
//!
//! Entries expire so that a genuinely external edit that happens to occur
//! after our write (but before its watcher event) can't be swallowed
//! forever. Time is injected for testability.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// How long a recorded write can wait for its watcher echo. Generous:
/// watcher latency is milliseconds, but loaded systems stall.
const EXPIRY: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct WriteJournal {
    entries: Mutex<HashMap<String, (blake3::Hash, Instant)>>,
}

impl WriteJournal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a write of `content_hash` to the note keyed by `path_key`
    /// (see `NotePath::key`).
    pub fn record(&self, path_key: String, content_hash: blake3::Hash, now: Instant) {
        self.entries.lock().insert(path_key, (content_hash, now));
    }

    /// Check whether an observed change is the echo of our own write.
    /// A positive match consumes the entry (one write, one echo).
    pub fn is_echo(&self, path_key: &str, content_hash: blake3::Hash, now: Instant) -> bool {
        let mut entries = self.entries.lock();
        // Opportunistic cleanup; the map only ever holds in-flight writes.
        entries.retain(|_, (_, recorded)| now.duration_since(*recorded) < EXPIRY);

        match entries.get(path_key) {
            Some((hash, _)) if *hash == content_hash => {
                entries.remove(path_key);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(data: &[u8]) -> blake3::Hash {
        blake3::hash(data)
    }

    #[test]
    fn own_write_is_echo_once() {
        let journal = WriteJournal::new();
        let now = Instant::now();
        journal.record("a.md".into(), hash(b"content"), now);
        assert!(journal.is_echo("a.md", hash(b"content"), now));
        // Consumed: a second identical event is a real external change
        // (e.g. user re-saved the same bytes in another editor).
        assert!(!journal.is_echo("a.md", hash(b"content"), now));
    }

    #[test]
    fn different_content_is_not_echo() {
        let journal = WriteJournal::new();
        let now = Instant::now();
        journal.record("a.md".into(), hash(b"ours"), now);
        assert!(!journal.is_echo("a.md", hash(b"theirs"), now));
        // The entry survives a failed match: our echo may still arrive.
        assert!(journal.is_echo("a.md", hash(b"ours"), now));
    }

    #[test]
    fn entries_expire() {
        let journal = WriteJournal::new();
        let start = Instant::now();
        journal.record("a.md".into(), hash(b"content"), start);
        let late = start + EXPIRY + Duration::from_millis(1);
        assert!(!journal.is_echo("a.md", hash(b"content"), late));
    }

    #[test]
    fn newer_write_replaces_older() {
        let journal = WriteJournal::new();
        let now = Instant::now();
        journal.record("a.md".into(), hash(b"v1"), now);
        journal.record("a.md".into(), hash(b"v2"), now);
        assert!(!journal.is_echo("a.md", hash(b"v1"), now));
        assert!(journal.is_echo("a.md", hash(b"v2"), now));
    }
}
