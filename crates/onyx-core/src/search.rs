//! Full-text search over note bodies, powered by tantivy.
//!
//! Why tantivy over SQLite FTS5: BM25 relevance, ~5–15 ms queries at 100k
//! notes, and a pluggable `Directory` — which is what lets encrypted vaults
//! keep their search index encrypted too (a custom Directory slots in
//! without touching this module's callers).
//!
//! Commits are the caller's concern: the app layer debounces them (~500 ms
//! after the last change) so a typing burst costs one segment, not fifty.

use std::path::Path;

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{
    BytesOptions, Field, STORED, Schema, TantivyDocument, TextFieldIndexing, TextOptions, Value,
};
use tantivy::{Index as TantivyIndex, IndexReader, IndexWriter, TantivyError, Term, doc};

use crate::paths::NoteId;

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("search index error: {0}")]
    Tantivy(#[from] TantivyError),
    #[error("search index error: {0}")]
    Internal(String),
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub id: NoteId,
    pub path: String,
    pub score: f32,
}

struct Fields {
    id: Field,
    path: Field,
    title: Field,
    body: Field,
    tags: Field,
}

/// The full-text index. One writer; readers reload on commit.
pub struct SearchIndex {
    writer: IndexWriter,
    reader: IndexReader,
    parser: QueryParser,
    fields: Fields,
}

/// 64 MB writer heap: comfortable for interactive updates, far below the
/// point where memory matters on a desktop.
const WRITER_HEAP: usize = 64 * 1024 * 1024;

impl SearchIndex {
    /// On-disk index under `dir` (`.onyx/tantivy/`).
    pub fn open_in_dir(dir: &Path) -> Result<Self, SearchError> {
        std::fs::create_dir_all(dir).map_err(|error| SearchError::Internal(error.to_string()))?;
        let directory = tantivy::directory::MmapDirectory::open(dir)
            .map_err(|error| SearchError::Internal(error.to_string()))?;
        let index = TantivyIndex::open_or_create(directory, schema())?;
        Self::from_index(index)
    }

    /// RAM-only index (tests; encrypted vaults will use an encrypted
    /// Directory instead).
    pub fn open_in_ram() -> Result<Self, SearchError> {
        Self::from_index(TantivyIndex::create_in_ram(schema()))
    }

    fn from_index(index: TantivyIndex) -> Result<Self, SearchError> {
        let schema = index.schema();
        let fields = Fields {
            id: schema
                .get_field("id")
                .map_err(|_| SearchError::Internal("schema missing id".into()))?,
            path: schema
                .get_field("path")
                .map_err(|_| SearchError::Internal("schema missing path".into()))?,
            title: schema
                .get_field("title")
                .map_err(|_| SearchError::Internal("schema missing title".into()))?,
            body: schema
                .get_field("body")
                .map_err(|_| SearchError::Internal("schema missing body".into()))?,
            tags: schema
                .get_field("tags")
                .map_err(|_| SearchError::Internal("schema missing tags".into()))?,
        };

        let writer = index.writer(WRITER_HEAP)?;
        let reader = index.reader()?;
        let mut parser =
            QueryParser::for_index(&index, vec![fields.title, fields.body, fields.tags]);
        // Titles matter more than body prose — same weighting Obsidian uses.
        parser.set_field_boost(fields.title, 2.0);

        Ok(Self {
            writer,
            reader,
            parser,
            fields,
        })
    }

    /// Add or replace one note's searchable content.
    pub fn upsert(
        &mut self,
        id: NoteId,
        path: &str,
        title: &str,
        body: &str,
        tags: &[String],
    ) -> Result<(), SearchError> {
        self.writer
            .delete_term(Term::from_field_bytes(self.fields.id, id.as_bytes()));
        self.writer.add_document(doc!(
            self.fields.id => id.as_bytes().as_slice(),
            self.fields.path => path,
            self.fields.title => title,
            self.fields.body => body,
            self.fields.tags => tags.join(" "),
        ))?;
        Ok(())
    }

    pub fn remove(&mut self, id: NoteId) -> Result<(), SearchError> {
        self.writer
            .delete_term(Term::from_field_bytes(self.fields.id, id.as_bytes()));
        Ok(())
    }

    /// Make pending changes visible to searches. Debounced by the caller.
    pub fn commit(&mut self) -> Result<(), SearchError> {
        self.writer.commit()?;
        self.reader.reload()?;
        Ok(())
    }

    /// BM25 search. User input is parsed leniently — a stray `(` must never
    /// error at the user.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, SearchError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        // The query grammar treats `-` as exclusion, so a plain search for
        // "well-known" would silently EXCLUDE "known". In-word hyphens
        // become term separators (matching the tokenizer); ` -exclude`
        // after whitespace keeps its operator meaning.
        let mut cleaned = String::with_capacity(trimmed.len());
        let mut previous = ' ';
        for c in trimmed.chars() {
            if c == '-' && previous.is_alphanumeric() {
                cleaned.push(' ');
            } else {
                cleaned.push(c);
            }
            previous = c;
        }
        let (parsed, _syntax_errors) = self.parser.parse_query_lenient(&cleaned);

        let searcher = self.reader.searcher();
        let top = searcher.search(&parsed, &TopDocs::with_limit(limit.max(1)))?;

        let mut hits = Vec::with_capacity(top.len());
        for (score, address) in top {
            let document: TantivyDocument = searcher.doc(address)?;
            let id_bytes = document
                .get_first(self.fields.id)
                .and_then(|value| value.as_bytes());
            let path = document
                .get_first(self.fields.path)
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if let Some(bytes) = id_bytes {
                if let Ok(array) = <[u8; 16]>::try_from(bytes) {
                    hits.push(SearchHit {
                        id: NoteId::from_bytes(array),
                        path: path.to_owned(),
                        score,
                    });
                }
            }
        }
        Ok(hits)
    }
}

fn schema() -> Schema {
    let mut builder = Schema::builder();
    let id_options = BytesOptions::default().set_indexed().set_stored();
    builder.add_bytes_field("id", id_options);
    builder.add_text_field("path", STORED);
    let text = TextOptions::default()
        .set_indexing_options(TextFieldIndexing::default().set_tokenizer("default"));
    builder.add_text_field("title", text.clone());
    builder.add_text_field("body", text.clone());
    builder.add_text_field("tags", text);
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> NoteId {
        NoteId::from_bytes([byte; 16])
    }

    fn build() -> SearchIndex {
        let mut search = SearchIndex::open_in_ram().unwrap();
        search
            .upsert(
                id(1),
                "notes/privacy.md",
                "privacy design",
                "zero knowledge encryption for the vault",
                &["crypto".into()],
            )
            .unwrap();
        search
            .upsert(
                id(2),
                "notes/graph.md",
                "graph view",
                "render the link graph with webgl",
                &[],
            )
            .unwrap();
        search
            .upsert(
                id(3),
                "journal/today.md",
                "today",
                "wrote about encryption twice: encryption",
                &[],
            )
            .unwrap();
        search.commit().unwrap();
        search
    }

    #[test]
    fn finds_by_body_terms() {
        let search = build();
        let hits = search.search("webgl", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id(2));
        assert_eq!(hits[0].path, "notes/graph.md");
    }

    #[test]
    fn title_outranks_body() {
        let search = build();
        // "privacy" appears in note 1's title; nowhere else.
        let hits = search.search("privacy", 10).unwrap();
        assert_eq!(hits[0].id, id(1));
    }

    #[test]
    fn tags_are_searchable() {
        let search = build();
        let hits = search.search("crypto", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, id(1));
    }

    #[test]
    fn update_replaces_old_content() {
        let mut search = build();
        search
            .upsert(
                id(2),
                "notes/graph.md",
                "graph view",
                "now about canvas",
                &[],
            )
            .unwrap();
        search.commit().unwrap();
        assert!(search.search("webgl", 10).unwrap().is_empty());
        assert_eq!(search.search("canvas", 10).unwrap().len(), 1);
    }

    #[test]
    fn remove_drops_from_results() {
        let mut search = build();
        search.remove(id(1)).unwrap();
        search.commit().unwrap();
        assert!(search.search("privacy", 10).unwrap().is_empty());
    }

    #[test]
    fn empty_and_garbage_queries_are_safe() {
        let search = build();
        assert!(search.search("", 10).unwrap().is_empty());
        assert!(search.search("   ", 10).unwrap().is_empty());
        // Lenient parsing: unbalanced syntax must not error.
        let _ = search.search("((foo AND", 10).unwrap();
    }

    #[test]
    fn uncommitted_changes_are_invisible_until_commit() {
        let mut search = build();
        search
            .upsert(id(9), "x.md", "brand new", "unindexed until commit", &[])
            .unwrap();
        assert!(search.search("unindexed", 10).unwrap().is_empty());
        search.commit().unwrap();
        assert_eq!(search.search("unindexed", 10).unwrap().len(), 1);
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut search = SearchIndex::open_in_dir(dir.path()).unwrap();
            search
                .upsert(id(5), "a.md", "persistent", "survives reopen", &[])
                .unwrap();
            search.commit().unwrap();
        }
        let search = SearchIndex::open_in_dir(dir.path()).unwrap();
        assert_eq!(search.search("survives", 10).unwrap().len(), 1);
    }
}

#[cfg(test)]
mod hyphen_tests {
    use super::*;

    #[test]
    fn in_word_hyphens_search_not_exclude() {
        let mut search = SearchIndex::open_in_ram().unwrap();
        search
            .upsert(
                NoteId::from_bytes([1; 16]),
                "a.md",
                "a",
                "a well-known fact",
                &[],
            )
            .unwrap();
        search.commit().unwrap();
        assert_eq!(search.search("well-known", 10).unwrap().len(), 1);
        // Explicit exclusion after whitespace still works.
        assert!(search.search("fact -known", 10).unwrap().is_empty());
    }
}
