//! Markdown extraction for Onyx.
//!
//! This crate is the *semantic authority* on what counts as a link, tag,
//! heading, or frontmatter in a note. It is pure (no I/O) and works on
//! byte offsets into the original source so callers can map every extracted
//! element back to its exact location.
//!
//! Extraction follows Obsidian's dialect: `[[wikilinks]]` with
//! `#heading`, `#^block`, and `|alias` parts, `![[embeds]]`, markdown links,
//! hierarchical `#tags`, YAML frontmatter, and `%%comments%%`. Elements
//! inside fenced code blocks, inline code spans, comments, and HTML comments
//! are ignored.
//!
//! Known divergences from CommonMark, chosen to match note-taking usage:
//! setext headings and indented code blocks are not recognized.

mod frontmatter;
mod scanner;

use std::ops::Range;

pub use frontmatter::Frontmatter;

/// Everything Onyx indexes about a single note, extracted in one pass.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedNote {
    /// Parsed YAML frontmatter, if present and valid.
    pub frontmatter: Option<Frontmatter>,
    /// All links (wiki, markdown, embeds, external), in source order.
    pub links: Vec<LinkRef>,
    /// Inline `#tags`, in source order. Frontmatter tags are separate.
    pub tags: Vec<TagRef>,
    /// ATX headings, in source order.
    pub headings: Vec<Heading>,
    /// Unicode-aware word count of the body (excludes frontmatter).
    pub word_count: usize,
    /// Byte range of the body (source minus frontmatter block).
    pub body_range: Range<usize>,
}

impl ExtractedNote {
    /// Tags declared in frontmatter (`tags:` as list or comma/space string),
    /// normalized without a leading `#`.
    pub fn frontmatter_tags(&self) -> Vec<String> {
        self.frontmatter
            .as_ref()
            .map(Frontmatter::tags)
            .unwrap_or_default()
    }

    /// Aliases declared in frontmatter (`aliases:` as list or string).
    pub fn frontmatter_aliases(&self) -> Vec<String> {
        self.frontmatter
            .as_ref()
            .map(Frontmatter::aliases)
            .unwrap_or_default()
    }
}

/// The kind of link occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkKind {
    /// `[[target]]`
    Wiki,
    /// `![[target]]` — transclusion/embed.
    WikiEmbed,
    /// `[text](target)` pointing inside the vault.
    Markdown,
    /// `![alt](target)` pointing inside the vault.
    MarkdownEmbed,
    /// `[text](https://…)` or any target with a URI scheme.
    External,
}

/// A single link occurrence in a note.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LinkRef {
    pub kind: LinkKind,
    /// Link target path/name as written (percent-decoded for markdown
    /// links). Empty for same-file references like `[[#heading]]`.
    pub target: String,
    /// Heading part: `[[note#heading]]` / `[note](note.md#heading)`.
    /// Nested heading paths (`a#b`) are kept verbatim.
    pub heading: Option<String>,
    /// Block reference: `[[note#^block-id]]` (without the `^`).
    pub block: Option<String>,
    /// Display text: wikilink `|alias` or markdown link text.
    pub alias: Option<String>,
    /// Byte span of the entire link syntax in the source.
    pub span: Range<usize>,
}

/// A single inline `#tag` occurrence (without the `#`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TagRef {
    /// The tag, hierarchical segments preserved: `project/onyx`.
    pub tag: String,
    /// Byte span including the `#`.
    pub span: Range<usize>,
}

/// An ATX heading.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Heading {
    /// 1–6.
    pub level: u8,
    /// Trimmed heading text with any closing `#` sequence removed.
    pub text: String,
    /// Byte span of the whole heading line (without trailing newline).
    pub span: Range<usize>,
}

/// Extract links, tags, headings, frontmatter and word count from a note.
///
/// Never fails: malformed constructs are simply not extracted, and invalid
/// frontmatter is treated as body text — matching how a note-taking app must
/// behave on arbitrary user input.
pub fn extract(source: &str) -> ExtractedNote {
    let (frontmatter, body_start) = frontmatter::parse(source);
    let body = &source[body_start..];

    let scan = scanner::scan(body, body_start);

    ExtractedNote {
        frontmatter,
        links: scan.links,
        tags: scan.tags,
        headings: scan.headings,
        word_count: count_words(body),
        body_range: body_start..source.len(),
    }
}

/// Unicode-aware word count: whitespace-separated tokens containing at least
/// one alphanumeric character.
fn count_words(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_count_ignores_pure_punctuation() {
        assert_eq!(count_words("hello world -- *** foo1"), 3);
        assert_eq!(count_words(""), 0);
        assert_eq!(count_words("   \n\t "), 0);
    }
}
