//! Vault-relative note paths and stable note identities.
//!
//! Path handling is where cross-platform note apps quietly corrupt data:
//! macOS stores NFD, users type NFC; Windows and macOS compare
//! case-insensitively, Linux doesn't. Onyx therefore normalizes every path
//! to NFC at the boundary and derives *identity* from a casefolded key, so
//! `Ünïcode Note.md` is one note everywhere.

use std::fmt;

use unicode_normalization::UnicodeNormalization;

/// Why a path was rejected.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path is empty")]
    Empty,
    #[error("path must be relative to the vault root: {0}")]
    Absolute(String),
    #[error("path contains a `.` or `..` segment: {0}")]
    DotSegment(String),
    #[error("path contains an empty segment: {0}")]
    EmptySegment(String),
    #[error("path contains a NUL byte")]
    NulByte,
}

/// A validated, NFC-normalized, `/`-separated path relative to the vault
/// root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NotePath {
    raw: String,
}

impl NotePath {
    /// Parse and normalize. Accepts `\` separators (Windows input) and
    /// trailing slashes; rejects absolute paths, `.`/`..` segments, and
    /// empty segments.
    pub fn new(input: &str) -> Result<Self, PathError> {
        if input.contains('\0') {
            return Err(PathError::NulByte);
        }
        let unified = input.replace('\\', "/");
        let trimmed = unified.trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(PathError::Empty);
        }
        if trimmed.starts_with('/') || has_windows_drive(trimmed) {
            return Err(PathError::Absolute(input.to_owned()));
        }
        for segment in trimmed.split('/') {
            if segment.is_empty() {
                return Err(PathError::EmptySegment(input.to_owned()));
            }
            if segment == "." || segment == ".." {
                return Err(PathError::DotSegment(input.to_owned()));
            }
        }

        Ok(Self {
            raw: trimmed.nfc().collect(),
        })
    }

    /// The normalized path as written (display form, original case).
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The identity key: casefolded normalized path. Two paths with equal
    /// keys are the same note on case-insensitive filesystems, so Onyx
    /// treats them as the same note on *every* platform (matching
    /// Obsidian's link semantics).
    pub fn key(&self) -> String {
        // Unicode simple casefolding approximated by to_lowercase(); full
        // casefolding differs only in rare locale-specific letters.
        self.raw.to_lowercase()
    }

    /// Stable 128-bit note identity within a vault.
    pub fn note_id(&self, vault_salt: &[u8; 16]) -> NoteId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(vault_salt);
        hasher.update(self.key().as_bytes());
        let hash = hasher.finalize();
        let mut id = [0u8; 16];
        id.copy_from_slice(&hash.as_bytes()[..16]);
        NoteId(id)
    }

    /// Lowercased extension without the dot, if any.
    pub fn extension(&self) -> Option<String> {
        let name = self.file_name();
        let dot = name.rfind('.')?;
        (dot > 0).then(|| name[dot + 1..].to_lowercase())
    }

    /// Final path segment.
    pub fn file_name(&self) -> &str {
        self.raw.rsplit('/').next().expect("paths are non-empty")
    }

    /// File name without its extension — the note's title in Obsidian
    /// semantics.
    pub fn stem(&self) -> &str {
        let name = self.file_name();
        match name.rfind('.') {
            Some(dot) if dot > 0 => &name[..dot],
            _ => name,
        }
    }

    /// Parent path, or `None` at the vault root.
    pub fn parent(&self) -> Option<NotePath> {
        let (parent, _) = self.raw.rsplit_once('/')?;
        Some(NotePath {
            raw: parent.to_owned(),
        })
    }

    /// Whether any segment starts with a dot (`.onyx`, `.git`,
    /// `.obsidian`, `.DS_Store`, …). Such paths are invisible to the vault.
    pub fn is_hidden(&self) -> bool {
        self.raw.split('/').any(|segment| segment.starts_with('.'))
    }

    /// Whether this is a markdown note (vs. an attachment).
    pub fn is_markdown(&self) -> bool {
        matches!(self.extension().as_deref(), Some("md" | "markdown"))
    }
}

impl fmt::Display for NotePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.raw)
    }
}

impl TryFrom<&str> for NotePath {
    type Error = PathError;
    fn try_from(input: &str) -> Result<Self, Self::Error> {
        Self::new(input)
    }
}

fn has_windows_drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Stable 128-bit note identity: `blake3(vault_salt ‖ casefold(nfc(path)))`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NoteId([u8; 16]);

impl NoteId {
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for NoteId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for NoteId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "NoteId({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_nfd_to_nfc() {
        // "ü" as NFD (u + combining diaeresis) vs NFC.
        let nfd = NotePath::new("u\u{0308}ber.md").unwrap();
        let nfc = NotePath::new("über.md").unwrap();
        assert_eq!(nfd, nfc);
        assert_eq!(nfd.key(), nfc.key());
    }

    #[test]
    fn casefold_key_unifies_case() {
        let upper = NotePath::new("Notes/README.md").unwrap();
        let lower = NotePath::new("notes/readme.md").unwrap();
        assert_ne!(upper, lower); // display form differs…
        assert_eq!(upper.key(), lower.key()); // …identity does not
        assert_eq!(upper.note_id(&[0; 16]), lower.note_id(&[0; 16]));
    }

    #[test]
    fn note_id_depends_on_salt() {
        let path = NotePath::new("a.md").unwrap();
        assert_ne!(path.note_id(&[0; 16]), path.note_id(&[1; 16]));
    }

    #[test]
    fn backslashes_and_trailing_slashes_normalize() {
        let path = NotePath::new(r"folder\sub\note.md").unwrap();
        assert_eq!(path.as_str(), "folder/sub/note.md");
        assert_eq!(NotePath::new("dir/").unwrap().as_str(), "dir");
    }

    #[test]
    fn rejects_invalid() {
        assert_eq!(NotePath::new(""), Err(PathError::Empty));
        assert_eq!(NotePath::new("/"), Err(PathError::Empty));
        assert!(matches!(NotePath::new("/abs"), Err(PathError::Absolute(_))));
        assert!(matches!(
            NotePath::new("C:/win"),
            Err(PathError::Absolute(_))
        ));
        assert!(matches!(
            NotePath::new("a/../b"),
            Err(PathError::DotSegment(_))
        ));
        assert!(matches!(
            NotePath::new("./a"),
            Err(PathError::DotSegment(_))
        ));
        assert!(matches!(
            NotePath::new("a//b"),
            Err(PathError::EmptySegment(_))
        ));
        assert_eq!(NotePath::new("a\0b"), Err(PathError::NulByte));
    }

    #[test]
    fn file_name_stem_extension_parent() {
        let path = NotePath::new("dir/Note.Name.md").unwrap();
        assert_eq!(path.file_name(), "Note.Name.md");
        assert_eq!(path.stem(), "Note.Name");
        assert_eq!(path.extension().as_deref(), Some("md"));
        assert_eq!(path.parent().unwrap().as_str(), "dir");
        assert_eq!(path.parent().unwrap().parent(), None);
    }

    #[test]
    fn dotfile_has_no_extension_and_is_hidden() {
        let path = NotePath::new(".gitignore").unwrap();
        assert_eq!(path.extension(), None);
        assert_eq!(path.stem(), ".gitignore");
        assert!(path.is_hidden());
        assert!(NotePath::new(".obsidian/app.json").unwrap().is_hidden());
        assert!(NotePath::new("a/.trash/b.md").unwrap().is_hidden());
        assert!(!NotePath::new("visible/note.md").unwrap().is_hidden());
    }

    #[test]
    fn markdown_detection() {
        assert!(NotePath::new("a.md").unwrap().is_markdown());
        assert!(NotePath::new("a.MD").unwrap().is_markdown());
        assert!(NotePath::new("a.markdown").unwrap().is_markdown());
        assert!(!NotePath::new("a.png").unwrap().is_markdown());
        assert!(!NotePath::new("md").unwrap().is_markdown());
    }
}
