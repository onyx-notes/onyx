//! Client-side RAG: heading-aware chunking + a cosine vector store, so AI
//! chat can ground answers in the vault.
//!
//! Embeddings come from the user's configured endpoint (`/embeddings`,
//! the OpenAI shape that OpenAI, Ollama, LM Studio, etc. all speak) — no
//! bundled models, and the same BYOK privacy stance as chat: nothing
//! leaves the machine except to the endpoint you chose. For a fully local
//! setup, point it at Ollama.
//!
//! The index persists to `.onyx/embeddings` (plaintext vaults) or stays in
//! RAM (encrypted vaults) — same rule as every other derived cache.

use serde::{Deserialize, Serialize};

/// A retrievable unit: a slice of a note under a heading.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chunk {
    pub path: String,
    /// The heading trail this chunk lives under (for citation).
    pub heading: String,
    pub text: String,
}

/// Split a note into heading-scoped chunks of roughly `target_chars`.
/// Headings start new chunks; long sections split on paragraph
/// boundaries. Frontmatter and empty sections are dropped.
pub fn chunk_note(path: &str, source: &str, target_chars: usize) -> Vec<Chunk> {
    let extracted = onyx_md::extract(source);
    let body = &source[extracted.body_range.clone()];

    let mut chunks = Vec::new();
    let mut heading = String::new();
    let mut buffer = String::new();

    let flush = |chunks: &mut Vec<Chunk>, heading: &str, buffer: &mut String| {
        let trimmed = buffer.trim();
        if !trimmed.is_empty() {
            chunks.push(Chunk {
                path: path.to_owned(),
                heading: heading.to_owned(),
                text: trimmed.to_owned(),
            });
        }
        buffer.clear();
    };

    for line in body.lines() {
        if let Some(level_text) = parse_heading(line) {
            flush(&mut chunks, &heading, &mut buffer);
            heading = level_text;
            continue;
        }
        // Paragraph break inside an over-long section → split.
        if line.trim().is_empty() && buffer.len() >= target_chars {
            flush(&mut chunks, &heading, &mut buffer);
            continue;
        }
        buffer.push_str(line);
        buffer.push('\n');
    }
    flush(&mut chunks, &heading, &mut buffer);
    chunks
}

fn parse_heading(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let level = trimmed.bytes().take_while(|&byte| byte == b'#').count();
    if (1..=6).contains(&level) && trimmed[level..].starts_with(' ') {
        Some(trimmed[level..].trim().to_owned())
    } else {
        None
    }
}

/// A stored chunk plus its embedding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Embedded {
    pub chunk: Chunk,
    pub vector: Vec<f32>,
}

/// One retrieval hit.
#[derive(Debug, Clone)]
pub struct Retrieved {
    pub chunk: Chunk,
    pub score: f32,
}

/// Cosine-similarity vector store over normalized embeddings.
#[derive(Default, Serialize, Deserialize)]
pub struct VectorStore {
    entries: Vec<Embedded>,
}

impl VectorStore {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Replace all chunks for one note (re-index on edit).
    pub fn set_note(&mut self, path: &str, embedded: Vec<Embedded>) {
        self.entries.retain(|entry| entry.chunk.path != path);
        self.entries.extend(embedded.into_iter().map(|mut entry| {
            normalize(&mut entry.vector);
            entry
        }));
    }

    pub fn remove_note(&mut self, path: &str) {
        self.entries.retain(|entry| entry.chunk.path != path);
    }

    /// Paths currently represented in the store.
    pub fn indexed_paths(&self) -> std::collections::HashSet<String> {
        self.entries
            .iter()
            .map(|entry| entry.chunk.path.clone())
            .collect()
    }

    /// Top-`k` chunks by cosine similarity to `query` (assumed already the
    /// query embedding). Returns descending by score.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<Retrieved> {
        let mut normalized = query.to_vec();
        normalize(&mut normalized);
        let mut scored: Vec<Retrieved> = self
            .entries
            .iter()
            .map(|entry| Retrieved {
                chunk: entry.chunk.clone(),
                score: dot(&normalized, &entry.vector),
            })
            .collect();
        scored.sort_by(|a, b| b.score.total_cmp(&a.score));
        scored.truncate(k);
        scored
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|error| error.to_string())
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|error| error.to_string())
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalize(vector: &mut [f32]) {
    let norm = dot(vector, vector).sqrt();
    if norm > f32::EPSILON {
        for value in vector.iter_mut() {
            *value /= norm;
        }
    }
}

/// Fetch embeddings from an OpenAI-shaped `/embeddings` endpoint.
/// Blocking; callers use a worker thread.
pub fn embed_texts(
    base_url: &str,
    api_key: &str,
    model: &str,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = client
        .post(format!("{}/embeddings", base_url.trim_end_matches('/')))
        .json(&serde_json::json!({ "model": model, "input": texts }));
    if !api_key.is_empty() {
        request = request.bearer_auth(api_key);
    }
    let response = request.send().map_err(|error| error.to_string())?;
    let status = response.status();
    let payload: serde_json::Value = response.json().map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!("{status}: {payload}"));
    }
    let data = payload["data"]
        .as_array()
        .ok_or("embeddings response missing 'data'")?;
    let mut vectors = Vec::with_capacity(data.len());
    for item in data {
        let vector: Vec<f32> = item["embedding"]
            .as_array()
            .ok_or("embedding entry missing vector")?
            .iter()
            .filter_map(|value| value.as_f64().map(|f| f as f32))
            .collect();
        vectors.push(vector);
    }
    Ok(vectors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunking_respects_headings() {
        let source = "---\ntitle: x\n---\n\
            # Intro\nHello world.\n\n\
            # Details\nFirst para.\n\nSecond para.\n";
        let chunks = chunk_note("note.md", source, 1000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "Intro");
        assert!(chunks[0].text.contains("Hello world"));
        assert!(!chunks[0].text.contains("title")); // frontmatter dropped
        assert_eq!(chunks[1].heading, "Details");
        assert!(chunks[1].text.contains("First para"));
        assert!(chunks[1].text.contains("Second para"));
    }

    #[test]
    fn long_sections_split_on_paragraphs() {
        let big = "word ".repeat(80); // ~400 chars
        let source = format!("# H\n{big}\n\n{big}\n\n{big}\n");
        let chunks = chunk_note("n.md", &source, 300);
        assert!(
            chunks.len() >= 2,
            "long section should split: {}",
            chunks.len()
        );
        assert!(chunks.iter().all(|chunk| chunk.heading == "H"));
    }

    #[test]
    fn empty_and_frontmatter_only_notes_yield_nothing() {
        assert!(chunk_note("a.md", "", 300).is_empty());
        assert!(chunk_note("a.md", "---\ntitle: x\n---\n", 300).is_empty());
    }

    /// Deterministic hash-based embedder for tests — same shape as a real
    /// one (unit vectors), no network.
    fn mock_embed(text: &str) -> Vec<f32> {
        let hash = blake3::hash(text.as_bytes());
        hash.as_bytes()[..16]
            .iter()
            .map(|&byte| byte as f32 / 255.0)
            .collect()
    }

    #[test]
    fn search_ranks_by_similarity() {
        let mut store = VectorStore::default();
        for (path, text) in [
            ("a.md", "the quick brown fox"),
            ("b.md", "a lazy sleeping dog"),
            ("c.md", "quantum field theory"),
        ] {
            store.set_note(
                path,
                vec![Embedded {
                    chunk: Chunk {
                        path: path.into(),
                        heading: String::new(),
                        text: text.into(),
                    },
                    vector: mock_embed(text),
                }],
            );
        }
        // Querying with an exact chunk's text ranks that chunk first.
        let hits = store.search(&mock_embed("quantum field theory"), 3);
        assert_eq!(hits[0].chunk.path, "c.md");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn set_note_replaces_and_remove_deletes() {
        let mut store = VectorStore::default();
        let entry = |text: &str| Embedded {
            chunk: Chunk {
                path: "a.md".into(),
                heading: String::new(),
                text: text.into(),
            },
            vector: mock_embed(text),
        };
        store.set_note("a.md", vec![entry("v1"), entry("v1b")]);
        assert_eq!(store.len(), 2);
        store.set_note("a.md", vec![entry("v2")]);
        assert_eq!(store.len(), 1); // replaced, not appended
        assert!(store.indexed_paths().contains("a.md"));
        store.remove_note("a.md");
        assert!(store.is_empty());
    }

    #[test]
    fn persistence_roundtrip() {
        let mut store = VectorStore::default();
        store.set_note(
            "a.md",
            vec![Embedded {
                chunk: Chunk {
                    path: "a.md".into(),
                    heading: "H".into(),
                    text: "content".into(),
                },
                vector: mock_embed("content"),
            }],
        );
        let bytes = store.to_bytes().unwrap();
        let restored = VectorStore::from_bytes(&bytes).unwrap();
        assert_eq!(restored.len(), 1);
        let hits = restored.search(&mock_embed("content"), 1);
        assert!(hits[0].score > 0.99, "normalized self-similarity ≈ 1");
    }
}
