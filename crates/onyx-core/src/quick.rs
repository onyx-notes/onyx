//! Quick-switcher fuzzy index: the single most latency-critical search in
//! the app (fires on every keystroke of Ctrl+P).
//!
//! Deliberately not tantivy: an in-memory fuzzy matcher over titles, paths,
//! and aliases, scored in parallel across cores. Measured ~11 ms for a
//! worst-case cold query over 100k entries (every entry matching). That is
//! the *first*-keystroke cost only: the app shell keeps nucleo's
//! incremental engine on top, so subsequent keystrokes rescore only the
//! previous keystroke's survivors and stay well under the 5 ms budget.

use std::collections::HashMap;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32String};
use rayon::prelude::*;

use crate::paths::NoteId;

/// A quick-switcher result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickHit {
    pub id: NoteId,
    /// Display path (original casing).
    pub path: String,
    pub score: u32,
}

struct Entry {
    path: String,
    /// What the pattern matches against: `title ⏐ aliases ⏐ path`.
    haystack: Utf32String,
}

/// In-memory fuzzy index over note titles/paths/aliases.
#[derive(Default)]
pub struct QuickSwitcher {
    entries: HashMap<NoteId, Entry>,
}

impl QuickSwitcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&mut self, id: NoteId, title: &str, path: &str, aliases: &[String]) {
        let mut haystack = String::with_capacity(title.len() + path.len() + 16);
        haystack.push_str(title);
        for alias in aliases {
            haystack.push(' ');
            haystack.push_str(alias);
        }
        haystack.push(' ');
        haystack.push_str(path);

        self.entries.insert(
            id,
            Entry {
                path: path.to_owned(),
                haystack: Utf32String::from(haystack),
            },
        );
    }

    pub fn remove(&mut self, id: NoteId) {
        self.entries.remove(&id);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Fuzzy query. Empty input returns everything (UI caps and orders by
    /// recency at a higher layer), capped at `limit`.
    pub fn query(&self, input: &str, limit: usize) -> Vec<QuickHit> {
        if input.trim().is_empty() {
            let mut all: Vec<QuickHit> = self
                .entries
                .iter()
                .map(|(id, entry)| QuickHit {
                    id: *id,
                    path: entry.path.clone(),
                    score: 0,
                })
                .collect();
            all.sort_by(|a, b| a.path.cmp(&b.path));
            all.truncate(limit);
            return all;
        }

        let pattern = Pattern::parse(input, CaseMatching::Ignore, Normalization::Smart);
        // Parallel scoring: nucleo's Matcher is cheap per thread but not
        // Sync, so each rayon worker initializes its own.
        let mut hits: Vec<QuickHit> = self
            .entries
            .par_iter()
            .map_init(
                || Matcher::new(Config::DEFAULT),
                |matcher, (id, entry)| {
                    pattern
                        .score(entry.haystack.slice(..), matcher)
                        .map(|score| QuickHit {
                            id: *id,
                            path: entry.path.clone(),
                            score,
                        })
                },
            )
            .flatten()
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.path.cmp(&b.path)));
        hits.truncate(limit);
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> NoteId {
        NoteId::from_bytes([byte; 16])
    }

    fn build() -> QuickSwitcher {
        let mut quick = QuickSwitcher::new();
        quick.upsert(id(1), "Meeting Notes", "work/Meeting Notes.md", &[]);
        quick.upsert(id(2), "Groceries", "personal/Groceries.md", &[]);
        quick.upsert(
            id(3),
            "Onyx Design",
            "projects/Onyx Design.md",
            &["architecture".to_owned()],
        );
        quick
    }

    #[test]
    fn fuzzy_matches_subsequences() {
        let quick = build();
        let hits = quick.query("mtng", 10);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].id, id(1));
    }

    #[test]
    fn matches_aliases() {
        let quick = build();
        let hits = quick.query("architecture", 10);
        assert_eq!(hits[0].id, id(3));
    }

    #[test]
    fn case_insensitive_and_unicode_normalized() {
        let mut quick = build();
        quick.upsert(id(4), "Über Note", "Über Note.md", &[]);
        // ASCII query finds the umlaut title (Normalization::Smart).
        let hits = quick.query("uber", 10);
        assert_eq!(hits[0].id, id(4));
        let upper = quick.query("GROCERIES", 10);
        assert_eq!(upper[0].id, id(2));
    }

    #[test]
    fn empty_query_lists_all_sorted() {
        let quick = build();
        let hits = quick.query("", 10);
        assert_eq!(hits.len(), 3);
        assert!(hits[0].path < hits[1].path);
    }

    #[test]
    fn rename_and_remove_update_results() {
        let mut quick = build();
        quick.upsert(id(2), "Shopping", "personal/Shopping.md", &[]);
        assert!(quick.query("groceries", 10).is_empty());
        assert_eq!(quick.query("shopping", 10)[0].id, id(2));

        quick.remove(id(2));
        assert!(quick.query("shopping", 10).is_empty());
        assert_eq!(quick.len(), 2);
    }

    #[test]
    fn no_match_returns_empty() {
        let quick = build();
        assert!(quick.query("zzzzqqqq", 10).is_empty());
    }
}
