//! Raw SQLite storage for the index. No ORM — the schema is small, the
//! queries are hot paths, and we want to see every one of them.

use std::path::Path;
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, OptionalExtension, params};

use crate::fs::FileStat;
use crate::paths::{NoteId, NotePath};

/// Bump on any schema change: existing index files are then silently
/// rebuilt (they are caches, not data).
const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE notes (
    id          BLOB PRIMARY KEY,
    path        TEXT NOT NULL,           -- display form
    path_key    TEXT NOT NULL UNIQUE,    -- casefolded identity
    lookup_path TEXT NOT NULL,           -- path_key minus .md, for link resolution
    lookup_name TEXT NOT NULL,           -- basename form links use
    title       TEXT NOT NULL,           -- filename stem
    is_markdown INTEGER NOT NULL,
    mtime_ns    INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    hash        BLOB NOT NULL,
    word_count  INTEGER                  -- NULL for attachments
) STRICT;
CREATE INDEX notes_lookup_path ON notes(lookup_path);
CREATE INDEX notes_lookup_name ON notes(lookup_name);

CREATE TABLE links (
    src        BLOB NOT NULL,
    ord        INTEGER NOT NULL,         -- source order within the note
    target_key TEXT NOT NULL,            -- normalized target ('' for external)
    raw_target TEXT NOT NULL,
    kind       INTEGER NOT NULL,         -- LinkKind discriminant
    heading    TEXT,
    block      TEXT,
    alias      TEXT,
    span_start INTEGER NOT NULL,
    span_end   INTEGER NOT NULL,
    PRIMARY KEY (src, ord)
) STRICT;
CREATE INDEX links_target ON links(target_key);

CREATE TABLE tags (
    note_id          BLOB NOT NULL,
    tag              TEXT NOT NULL,
    from_frontmatter INTEGER NOT NULL
) STRICT;
CREATE INDEX tags_note ON tags(note_id);
CREATE INDEX tags_tag ON tags(tag);

CREATE TABLE frontmatter (
    note_id    BLOB NOT NULL,
    key        TEXT NOT NULL,
    value_json TEXT NOT NULL,
    PRIMARY KEY (note_id, key)
) STRICT;

CREATE TABLE headings (
    note_id    BLOB NOT NULL,
    ord        INTEGER NOT NULL,
    level      INTEGER NOT NULL,
    text       TEXT NOT NULL,
    span_start INTEGER NOT NULL,
    PRIMARY KEY (note_id, ord)
) STRICT;
";

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("index I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("index error: {0}")]
    Internal(String),
}

/// A note row as consumers see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRecord {
    pub id: NoteId,
    pub path: NotePath,
    pub title: String,
    pub is_markdown: bool,
    pub size: u64,
    pub word_count: Option<u64>,
}

/// One backlink occurrence: who links here, and where in their text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklinkRow {
    pub src: NoteId,
    pub span_start: usize,
    pub span_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagCount {
    pub tag: String,
    pub count: u64,
}

pub(super) struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path, salt: [u8; 16]) -> Result<Self, IndexError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match Self::try_open(path, salt) {
            Ok(store) => Ok(store),
            Err(_) => {
                // Any failure — corruption, old schema, salt change — means
                // the cache is stale. Destroy and recreate.
                let _ = std::fs::remove_file(path);
                let _ = std::fs::remove_file(path.with_extension("db-wal"));
                let _ = std::fs::remove_file(path.with_extension("db-shm"));
                Self::try_open(path, salt)
            }
        }
    }

    pub fn open_in_memory(salt: [u8; 16]) -> Result<Self, IndexError> {
        let conn = Connection::open_in_memory()?;
        Self::initialize(conn, salt)
    }

    fn try_open(path: &Path, salt: [u8; 16]) -> Result<Self, IndexError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        let existing: Option<i64> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0).map(|v| v.parse().unwrap_or(-1)),
            )
            .optional()
            .unwrap_or(None);

        match existing {
            Some(version) if version == SCHEMA_VERSION => {
                let stored_salt: String =
                    conn.query_row("SELECT value FROM meta WHERE key = 'salt'", [], |row| {
                        row.get(0)
                    })?;
                if stored_salt != hex(&salt) {
                    return Err(IndexError::Internal("salt mismatch".into()));
                }
                Ok(Self { conn })
            }
            Some(_) => Err(IndexError::Internal("schema version mismatch".into())),
            None => Self::initialize(conn, salt),
        }
    }

    fn initialize(conn: Connection, salt: [u8; 16]) -> Result<Self, IndexError> {
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1), ('salt', ?2)",
            params![SCHEMA_VERSION.to_string(), hex(&salt)],
        )?;
        Ok(Self { conn })
    }

    // ------------------------------------------------------------------
    // Writes
    // ------------------------------------------------------------------

    /// Cheapest change check: `(mtime, size)` unchanged. Lets reconcile
    /// skip reading file content entirely for a quiet vault.
    pub fn stat_matches(&self, id: NoteId, stat: &FileStat) -> Result<bool, IndexError> {
        let row: Option<(i64, i64)> = self
            .conn
            .query_row(
                "SELECT mtime_ns, size FROM notes WHERE id = ?1",
                params![id.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row == Some((stat_mtime_ns(stat), stat.size as i64)))
    }

    /// Fast change check: `(mtime, size)` equal, or content hash equal
    /// (in which case the stored stat is refreshed).
    pub fn is_current(
        &mut self,
        id: NoteId,
        stat: &FileStat,
        hash: &[u8; 32],
    ) -> Result<bool, IndexError> {
        let row: Option<(i64, i64, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT mtime_ns, size, hash FROM notes WHERE id = ?1",
                params![id.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((mtime_ns, size, stored_hash)) = row else {
            return Ok(false);
        };

        if mtime_ns == stat_mtime_ns(stat) && size as u64 == stat.size {
            return Ok(true);
        }
        if stored_hash == hash {
            // Touched but unchanged: refresh stat so future checks stay fast.
            self.conn.execute(
                "UPDATE notes SET mtime_ns = ?2, size = ?3 WHERE id = ?1",
                params![id.as_bytes(), stat_mtime_ns(stat), stat.size as i64],
            )?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn upsert_note(
        &mut self,
        id: NoteId,
        path: &NotePath,
        stat: &FileStat,
        hash: &[u8; 32],
        extracted: Option<&onyx_md::ExtractedNote>,
    ) -> Result<(), IndexError> {
        let tx = self.conn.transaction()?;

        let path_key = path.key();
        let (lookup_path, lookup_name) = lookup_keys(path);
        tx.execute(
            "INSERT INTO notes (id, path, path_key, lookup_path, lookup_name, title,
                                is_markdown, mtime_ns, size, hash, word_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                path = ?2, path_key = ?3, lookup_path = ?4, lookup_name = ?5,
                title = ?6, is_markdown = ?7, mtime_ns = ?8, size = ?9,
                hash = ?10, word_count = ?11",
            params![
                id.as_bytes(),
                path.as_str(),
                path_key,
                lookup_path,
                lookup_name,
                path.stem(),
                extracted.is_some(),
                stat_mtime_ns(stat),
                stat.size as i64,
                hash,
                extracted.map(|e| e.word_count as i64),
            ],
        )?;

        // Children are always replaced wholesale — no partial-update drift.
        tx.execute("DELETE FROM links WHERE src = ?1", params![id.as_bytes()])?;
        tx.execute(
            "DELETE FROM tags WHERE note_id = ?1",
            params![id.as_bytes()],
        )?;
        tx.execute(
            "DELETE FROM frontmatter WHERE note_id = ?1",
            params![id.as_bytes()],
        )?;
        tx.execute(
            "DELETE FROM headings WHERE note_id = ?1",
            params![id.as_bytes()],
        )?;

        if let Some(note) = extracted {
            for (ord, link) in note.links.iter().enumerate() {
                let target_key = if link.kind == onyx_md::LinkKind::External {
                    String::new()
                } else {
                    normalize_target(&link.target)
                };
                tx.execute(
                    "INSERT INTO links (src, ord, target_key, raw_target, kind, heading,
                                        block, alias, span_start, span_end)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        id.as_bytes(),
                        ord as i64,
                        target_key,
                        link.target,
                        link_kind_code(link.kind),
                        link.heading,
                        link.block,
                        link.alias,
                        link.span.start as i64,
                        link.span.end as i64,
                    ],
                )?;
            }
            for tag in &note.tags {
                tx.execute(
                    "INSERT INTO tags (note_id, tag, from_frontmatter) VALUES (?1, ?2, 0)",
                    params![id.as_bytes(), tag.tag.to_lowercase()],
                )?;
            }
            for tag in note.frontmatter_tags() {
                tx.execute(
                    "INSERT INTO tags (note_id, tag, from_frontmatter) VALUES (?1, ?2, 1)",
                    params![id.as_bytes(), tag.to_lowercase()],
                )?;
            }
            if let Some(frontmatter) = &note.frontmatter {
                if let Some(map) = frontmatter.value().as_object() {
                    for (key, value) in map {
                        tx.execute(
                            "INSERT INTO frontmatter (note_id, key, value_json)
                             VALUES (?1, ?2, ?3)
                             ON CONFLICT(note_id, key) DO UPDATE SET value_json = ?3",
                            params![id.as_bytes(), key, value.to_string()],
                        )?;
                    }
                }
            }
            for (ord, heading) in note.headings.iter().enumerate() {
                tx.execute(
                    "INSERT INTO headings (note_id, ord, level, text, span_start)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        id.as_bytes(),
                        ord as i64,
                        heading.level as i64,
                        heading.text,
                        heading.span.start as i64,
                    ],
                )?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub fn remove(&mut self, id: NoteId) -> Result<(), IndexError> {
        let tx = self.conn.transaction()?;
        for table in ["links", "tags", "frontmatter", "headings"] {
            let column = if table == "links" { "src" } else { "note_id" };
            tx.execute(
                &format!("DELETE FROM {table} WHERE {column} = ?1"),
                params![id.as_bytes()],
            )?;
        }
        tx.execute("DELETE FROM notes WHERE id = ?1", params![id.as_bytes()])?;
        tx.commit()?;
        Ok(())
    }

    /// Remove every note whose id is not in `keep` (reconcile sweep).
    pub fn remove_all_except(&mut self, keep: &[NoteId]) -> Result<(), IndexError> {
        let known: Vec<NoteId> = {
            let mut statement = self.conn.prepare("SELECT id FROM notes")?;
            statement
                .query_map([], |row| row.get::<_, Vec<u8>>(0))?
                .filter_map(|result| {
                    result
                        .ok()
                        .and_then(|bytes| bytes.try_into().ok().map(NoteId::from_bytes))
                })
                .collect()
        };
        let keep_set: std::collections::HashSet<&NoteId> = keep.iter().collect();
        for id in known {
            if !keep_set.contains(&id) {
                self.remove(id)?;
            }
        }
        Ok(())
    }

    pub fn clear(&mut self) -> Result<(), IndexError> {
        let tx = self.conn.transaction()?;
        for table in ["links", "tags", "frontmatter", "headings", "notes"] {
            tx.execute(&format!("DELETE FROM {table}"), [])?;
        }
        tx.commit()?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    pub fn note(&self, id: NoteId) -> Result<Option<NoteRecord>, IndexError> {
        self.conn
            .query_row(
                "SELECT id, path, title, is_markdown, size, word_count
                 FROM notes WHERE id = ?1",
                params![id.as_bytes()],
                note_record_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn note_count(&self) -> Result<usize, IndexError> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get::<_, i64>(0))?
            as usize)
    }

    pub fn resolve(&self, target: &str) -> Result<Option<NoteId>, IndexError> {
        let key = normalize_target(target);
        if key.is_empty() {
            return Ok(None);
        }

        // 1. Exact vault path (as `folder/note` or an attachment path).
        let exact: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT id FROM notes WHERE lookup_path = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(bytes) = exact {
            return Ok(bytes.try_into().ok().map(NoteId::from_bytes));
        }

        // 2. Basename; shortest full path wins (Obsidian's rule), path as
        //    tiebreaker for determinism.
        let by_name: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT id FROM notes WHERE lookup_name = ?1
                 ORDER BY length(path_key) ASC, path_key ASC LIMIT 1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(by_name.and_then(|bytes| bytes.try_into().ok().map(NoteId::from_bytes)))
    }

    pub fn backlinks(&self, id: NoteId) -> Result<Vec<BacklinkRow>, IndexError> {
        // Candidate links: anything whose normalized target could name this
        // note; then confirm via full resolution (ambiguous basenames must
        // resolve to *this* note to count).
        let (lookup_path, lookup_name): (String, String) = self.conn.query_row(
            "SELECT lookup_path, lookup_name FROM notes WHERE id = ?1",
            params![id.as_bytes()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let mut statement = self.conn.prepare(
            "SELECT src, target_key, span_start, span_end FROM links
             WHERE target_key IN (?1, ?2) AND src != ?3
             ORDER BY src, ord",
        )?;
        let candidates: Vec<(Vec<u8>, String, i64, i64)> = statement
            .query_map(params![lookup_path, lookup_name, id.as_bytes()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<Result<_, _>>()?;

        let mut rows = Vec::with_capacity(candidates.len());
        for (src_bytes, target_key, start, end) in candidates {
            if self.resolve(&target_key)? == Some(id) {
                let Ok(src) = src_bytes.try_into().map(NoteId::from_bytes) else {
                    continue;
                };
                rows.push(BacklinkRow {
                    src,
                    span_start: start as usize,
                    span_end: end as usize,
                });
            }
        }
        Ok(rows)
    }

    pub fn tags(&self) -> Result<Vec<TagCount>, IndexError> {
        let mut statement = self.conn.prepare(
            "SELECT tag, COUNT(*) FROM tags GROUP BY tag
             ORDER BY COUNT(*) DESC, tag ASC",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok(TagCount {
                    tag: row.get(0)?,
                    count: row.get::<_, i64>(1)? as u64,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    pub fn unresolved_targets(&self) -> Result<Vec<String>, IndexError> {
        let mut statement = self.conn.prepare(
            "SELECT DISTINCT target_key FROM links
             WHERE target_key != ''
               AND target_key NOT IN (SELECT lookup_path FROM notes)
               AND target_key NOT IN (SELECT lookup_name FROM notes)
             ORDER BY target_key",
        )?;
        let rows = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Canonical, deterministic serialization of everything indexed.
    pub fn dump(&self) -> Result<String, IndexError> {
        struct DumpNote {
            line: String,
            id: Vec<u8>,
        }

        let mut out = String::new();

        let mut notes = self.conn.prepare(
            "SELECT path_key, path, title, is_markdown, size, hash, word_count, id
             FROM notes ORDER BY path_key",
        )?;
        let note_rows: Vec<DumpNote> = notes
            .query_map([], |row| {
                let words: Option<i64> = row.get(6)?;
                Ok(DumpNote {
                    line: format!(
                        "note {} | {} | {} | md={} size={} hash={} words={words:?}",
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        hex(&row.get::<_, Vec<u8>>(5)?),
                    ),
                    id: row.get(7)?,
                })
            })?
            .collect::<Result<_, _>>()?;

        for DumpNote { line, id } in note_rows {
            out.push_str(&line);
            out.push('\n');
            let mut links = self.conn.prepare(
                "SELECT ord, target_key, raw_target, kind, heading, block, alias,
                        span_start, span_end
                 FROM links WHERE src = ?1 ORDER BY ord",
            )?;
            let link_rows: Vec<String> = links
                .query_map(params![id], |row| {
                    Ok(format!(
                        "  link {} -> '{}' ({}) kind={} h={:?} b={:?} a={:?} @{}..{}",
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                })?
                .collect::<Result<_, _>>()?;
            for line in link_rows {
                out.push_str(&line);
                out.push('\n');
            }

            for (label, sql) in [
                (
                    "tag",
                    "SELECT tag || ' ff=' || from_frontmatter FROM tags
                     WHERE note_id = ?1 ORDER BY tag, from_frontmatter",
                ),
                (
                    "fm",
                    "SELECT key || '=' || value_json FROM frontmatter
                     WHERE note_id = ?1 ORDER BY key",
                ),
                (
                    "heading",
                    "SELECT ord || ' H' || level || ' ' || text FROM headings
                     WHERE note_id = ?1 ORDER BY ord",
                ),
            ] {
                let mut statement = self.conn.prepare(sql)?;
                let rows: Vec<String> = statement
                    .query_map(params![id], |row| row.get(0))?
                    .collect::<Result<_, _>>()?;
                for row in rows {
                    out.push_str(&format!("  {label} {row}\n"));
                }
            }
        }
        Ok(out)
    }
}

fn note_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRecord> {
    let id_bytes: Vec<u8> = row.get(0)?;
    let path_text: String = row.get(1)?;
    Ok(NoteRecord {
        id: NoteId::from_bytes(id_bytes.try_into().unwrap_or([0; 16])),
        path: NotePath::new(&path_text)
            .unwrap_or_else(|_| NotePath::new("invalid").expect("static path is valid")),
        title: row.get(2)?,
        is_markdown: row.get::<_, i64>(3)? != 0,
        size: row.get::<_, i64>(4)? as u64,
        word_count: row.get::<_, Option<i64>>(5)?.map(|words| words as u64),
    })
}

/// Normalize a link target the same way note paths are keyed: unify
/// separators, NFC + casefold, strip a `.md`/`.markdown` extension.
pub(super) fn normalize_target(target: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let unified = target.replace('\\', "/");
    let trimmed = unified.trim().trim_matches('/');
    let normalized: String = trimmed.nfc().collect::<String>().to_lowercase();
    normalized
        .strip_suffix(".md")
        .or_else(|| normalized.strip_suffix(".markdown"))
        .unwrap_or(&normalized)
        .to_owned()
}

/// Resolution keys for a vault file: `(lookup_path, lookup_name)`.
/// Markdown drops its extension (`[[note]]`); attachments keep theirs
/// (`![[image.png]]`).
fn lookup_keys(path: &NotePath) -> (String, String) {
    let key = path.key();
    if path.is_markdown() {
        let without_ext = key
            .strip_suffix(".md")
            .or_else(|| key.strip_suffix(".markdown"))
            .unwrap_or(&key);
        let name = without_ext.rsplit('/').next().unwrap_or(without_ext);
        (without_ext.to_owned(), name.to_owned())
    } else {
        let name = key.rsplit('/').next().unwrap_or(&key);
        (key.clone(), name.to_owned())
    }
}

fn link_kind_code(kind: onyx_md::LinkKind) -> i64 {
    match kind {
        onyx_md::LinkKind::Wiki => 0,
        onyx_md::LinkKind::WikiEmbed => 1,
        onyx_md::LinkKind::Markdown => 2,
        onyx_md::LinkKind::MarkdownEmbed => 3,
        onyx_md::LinkKind::External => 4,
    }
}

fn stat_mtime_ns(stat: &FileStat) -> i64 {
    stat.mtime
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as i64)
        .unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
