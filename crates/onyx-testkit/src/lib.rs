//! Deterministic synthetic vault corpora for tests and benchmarks.
//!
//! The same seed always produces byte-identical vaults, so perf baselines
//! and regression tests compare like with like. No `rand` dependency: a
//! tiny splitmix64 is all we need, and it keeps determinism under our
//! control forever.

use std::io;
use std::path::Path;

/// Configuration for a generated corpus.
#[derive(Debug, Clone, Copy)]
pub struct CorpusConfig {
    /// Number of markdown notes.
    pub notes: usize,
    /// RNG seed; same seed ⇒ byte-identical corpus.
    pub seed: u64,
    /// Number of folders notes are spread across.
    pub folders: usize,
}

impl CorpusConfig {
    /// A small corpus for unit/integration tests.
    pub const SMALL: Self = Self {
        notes: 100,
        seed: 20260716,
        folders: 8,
    };

    /// The benchmark corpus: 100k notes, the scale every perf budget is
    /// measured against.
    pub const BENCH_100K: Self = Self {
        notes: 100_000,
        seed: 20260716,
        folders: 400,
    };
}

/// splitmix64 — tiny, seedable, good-enough distribution for test data.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..bound` (bound > 0).
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const WORDS: &[&str] = &[
    "graph", "vault", "note", "system", "design", "product", "market", "index", "search", "编集",
    "編集", "sync", "backup", "privacy", "crypto", "editor", "plugin", "theme", "insight",
    "review", "draft", "final", "meeting", "journal", "projekt", "idée", "über", "launch",
    "metric", "user", "growth", "test", "bench", "release", "week", "plan",
];

const TAGS: &[&str] = &[
    "project",
    "project/onyx",
    "project/personal",
    "status/draft",
    "status/done",
    "idea",
    "meeting",
    "journal",
    "reference",
    "todo",
    "später",
    "日記",
];

/// Generate the corpus, calling `emit(relative_path, content)` per note.
pub fn generate(config: CorpusConfig, mut emit: impl FnMut(&str, &str)) {
    assert!(config.folders > 0, "at least one folder required");
    let mut rng = Rng(config.seed);

    for index in 0..config.notes {
        let folder = index % config.folders;
        let path = format!("folder-{folder:03}/note-{index:06}.md");
        let content = note_content(&mut rng, index, config);
        emit(&path, &content);
    }
}

/// Write the corpus under `root`. Returns the number of notes written.
pub fn write_to_dir(root: &Path, config: CorpusConfig) -> io::Result<usize> {
    let mut written = 0;
    let mut error = None;
    generate(config, |relative, content| {
        if error.is_some() {
            return;
        }
        let target = root.join(relative);
        let result = target
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|()| std::fs::write(&target, content));
        match result {
            Ok(()) => written += 1,
            Err(e) => error = Some(e),
        }
    });
    match error {
        Some(e) => Err(e),
        None => Ok(written),
    }
}

fn note_content(rng: &mut Rng, index: usize, config: CorpusConfig) -> String {
    let mut content = String::with_capacity(1024);

    // ~40% of notes carry frontmatter.
    if rng.below(10) < 4 {
        content.push_str("---\ntags: [");
        content.push_str(rng.pick(TAGS));
        content.push_str("]\ncreated: 2026-07-16\n---\n");
    }

    content.push_str(&format!("# Note {index}\n\n"));

    let paragraphs = 1 + rng.below(4);
    for _ in 0..paragraphs {
        let words = 20 + rng.below(60);
        for position in 0..words {
            if position > 0 {
                content.push(' ');
            }
            content.push_str(rng.pick(WORDS));
        }
        content.push_str("\n\n");
    }

    // 0–5 wikilinks to other notes (realistic link density). Targets use
    // the same folder assignment as `generate`, so links always resolve.
    for _ in 0..rng.below(6) {
        let target_index = rng.below(config.notes);
        let target_folder = target_index % config.folders;
        content.push_str(&format!(
            "See [[folder-{target_folder:03}/note-{target_index:06}]] for more.\n"
        ));
    }

    // 0–2 inline tags.
    for _ in 0..rng.below(3) {
        content.push_str(&format!("\n#{}\n", rng.pick(TAGS)));
    }

    // A second heading in ~30% of notes.
    if rng.below(10) < 3 {
        content.push_str("\n## Details\n\nmore body text here.\n");
    }

    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_same_seed_same_bytes() {
        let mut first = Vec::new();
        let mut second = Vec::new();
        generate(CorpusConfig::SMALL, |path, content| {
            first.push((path.to_owned(), content.to_owned()));
        });
        generate(CorpusConfig::SMALL, |path, content| {
            second.push((path.to_owned(), content.to_owned()));
        });
        assert_eq!(first, second);
    }

    #[test]
    fn different_seed_different_bytes() {
        let mut first = Vec::new();
        generate(CorpusConfig::SMALL, |_, content| {
            first.push(content.to_owned())
        });
        let mut second = Vec::new();
        let other = CorpusConfig {
            seed: 42,
            ..CorpusConfig::SMALL
        };
        generate(other, |_, content| second.push(content.to_owned()));
        assert_ne!(first, second);
    }

    #[test]
    fn corpus_shape() {
        let mut count = 0;
        let mut with_links = 0;
        generate(CorpusConfig::SMALL, |path, content| {
            count += 1;
            assert!(path.ends_with(".md"));
            assert!(content.starts_with("---") || content.starts_with("# Note"));
            if content.contains("[[") {
                with_links += 1;
            }
        });
        assert_eq!(count, 100);
        assert!(with_links > 30, "links should be common: {with_links}");
    }

    #[test]
    fn writes_to_disk() {
        let dir = std::env::temp_dir().join(format!("onyx-testkit-{}", std::process::id()));
        let config = CorpusConfig {
            notes: 10,
            ..CorpusConfig::SMALL
        };
        let written = write_to_dir(&dir, config).unwrap();
        assert_eq!(written, 10);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
