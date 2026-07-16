//! `CryptoFs`: encryption at rest as a filesystem decorator.
//!
//! Wraps any [`VaultFs`] and encrypts both file contents (chunked AEAD
//! container) and file/directory names (deterministic SIV tokens, so the
//! watcher maps the same ciphertext name to the same note every time).
//! Everything above the `VaultFs` trait — vault, watcher, indexer, engine —
//! is completely oblivious.
//!
//! Layout rules:
//! - Visible files: every path component becomes a base32 token; files get
//!   an `.onyxenc` suffix. `folder/Note.md` → `KRSXG…/JBSWY….onyxenc`.
//! - Hidden paths (`.onyx/…`) pass through unencrypted: they hold the
//!   keyfile, index snapshots, and settings — app data, not notes.
//!   (The index database for encrypted vaults lives in RAM; its encrypted
//!   snapshot file is itself a container.)
//!
//! Known metadata leaks, accepted and documented: directory tree shape,
//! approximate (ciphertext) file sizes, modification times, and name
//! equality within the vault.

use std::io;

use onyx_crypto::VaultKey;

use crate::fs::{FileStat, VaultFs};
use crate::paths::NotePath;

const ENCRYPTED_SUFFIX: &str = ".onyxenc";

pub struct CryptoFs {
    inner: std::sync::Arc<dyn VaultFs>,
    key: VaultKey,
}

impl CryptoFs {
    pub fn new(inner: std::sync::Arc<dyn VaultFs>, key: VaultKey) -> Self {
        Self { inner, key }
    }

    /// Plaintext vault path → ciphertext storage path.
    fn seal_path(&self, path: &NotePath) -> Result<NotePath, io::Error> {
        if path.is_hidden() {
            return Ok(path.clone());
        }
        let sealed: Vec<String> = path
            .as_str()
            .split('/')
            .map(|component| onyx_crypto::encrypt_name(&self.key, component))
            .collect();
        let mut joined = sealed.join("/");
        joined.push_str(ENCRYPTED_SUFFIX);
        NotePath::new(&joined).map_err(|error| io::Error::other(error.to_string()))
    }

    /// Ciphertext storage path → plaintext vault path. `None` for foreign
    /// files that aren't ours (wrong key, stray files in the directory).
    pub fn open_path(&self, sealed: &NotePath) -> Option<NotePath> {
        if sealed.is_hidden() {
            return Some(sealed.clone());
        }
        let raw = sealed.as_str().strip_suffix(ENCRYPTED_SUFFIX)?;
        let opened: Option<Vec<String>> = raw
            .split('/')
            .map(|token| {
                // Directory components carry no suffix; file component had
                // it stripped above. Both are bare tokens.
                onyx_crypto::decrypt_name(&self.key, token).ok()
            })
            .collect();
        NotePath::new(&opened?.join("/")).ok()
    }
}

fn crypto_error(error: onyx_crypto::CryptoError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

impl VaultFs for CryptoFs {
    fn read(&self, path: &NotePath) -> io::Result<Vec<u8>> {
        let sealed = self.seal_path(path)?;
        let bytes = self.inner.read(&sealed)?;
        if path.is_hidden() {
            return Ok(bytes);
        }
        onyx_crypto::decrypt(&self.key, &bytes).map_err(crypto_error)
    }

    fn write_atomic(&self, path: &NotePath, data: &[u8]) -> io::Result<()> {
        let sealed = self.seal_path(path)?;
        if path.is_hidden() {
            return self.inner.write_atomic(&sealed, data);
        }
        let ciphertext = onyx_crypto::encrypt(&self.key, data);
        self.inner.write_atomic(&sealed, &ciphertext)
    }

    fn remove(&self, path: &NotePath) -> io::Result<()> {
        self.inner.remove(&self.seal_path(path)?)
    }

    fn rename(&self, from: &NotePath, to: &NotePath) -> io::Result<()> {
        self.inner
            .rename(&self.seal_path(from)?, &self.seal_path(to)?)
    }

    fn stat(&self, path: &NotePath) -> io::Result<FileStat> {
        // Ciphertext size, not plaintext — consistent across all callers,
        // which is all change detection needs.
        self.inner.stat(&self.seal_path(path)?)
    }

    fn list(&self) -> io::Result<Vec<(NotePath, FileStat)>> {
        Ok(self
            .inner
            .list()?
            .into_iter()
            .filter_map(|(sealed, stat)| Some((self.open_path(&sealed)?, stat)))
            .collect())
    }

    fn exists(&self, path: &NotePath) -> bool {
        self.seal_path(path)
            .map(|sealed| self.inner.exists(&sealed))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use onyx_crypto::VaultKey;

    use super::*;
    use crate::fs::MemFs;
    use crate::vault::{Vault, VaultConfig};

    fn setup() -> (Arc<MemFs>, CryptoFs) {
        let inner = Arc::new(MemFs::new());
        let key = VaultKey::from_bytes([7; 32]);
        (inner.clone(), CryptoFs::new(inner, key))
    }

    fn path(text: &str) -> NotePath {
        NotePath::new(text).unwrap()
    }

    #[test]
    fn roundtrip_and_names_are_opaque_on_disk() {
        let (inner, fs) = setup();
        fs.write_atomic(&path("folder/Secret Note.md"), b"top secret")
            .unwrap();

        assert_eq!(
            fs.read(&path("folder/Secret Note.md")).unwrap(),
            b"top secret"
        );

        // What actually hit the inner fs: no plaintext names, no plaintext
        // content.
        let raw = inner.list().unwrap();
        assert_eq!(raw.len(), 1);
        let stored = raw[0].0.as_str();
        assert!(!stored.to_lowercase().contains("secret"));
        assert!(stored.ends_with(".onyxenc"));
        let raw_bytes = inner.read(&raw[0].0).unwrap();
        assert!(!raw_bytes.windows(10).any(|window| window == b"top secret"));
    }

    #[test]
    fn list_translates_names_back() {
        let (_, fs) = setup();
        fs.write_atomic(&path("a.md"), b"1").unwrap();
        fs.write_atomic(&path("dir/b.md"), b"2").unwrap();
        let mut listed: Vec<String> = fs
            .list()
            .unwrap()
            .into_iter()
            .map(|(p, _)| p.as_str().to_owned())
            .collect();
        listed.sort();
        assert_eq!(listed, vec!["a.md", "dir/b.md"]);
    }

    #[test]
    fn deterministic_paths_stable_across_instances() {
        let inner = Arc::new(MemFs::new());
        let first = CryptoFs::new(inner.clone(), VaultKey::from_bytes([7; 32]));
        first.write_atomic(&path("note.md"), b"x").unwrap();
        drop(first);
        // Re-open (new instance, same key): same file is found.
        let second = CryptoFs::new(inner, VaultKey::from_bytes([7; 32]));
        assert!(second.exists(&path("note.md")));
        assert_eq!(second.read(&path("note.md")).unwrap(), b"x");
    }

    #[test]
    fn hidden_paths_pass_through() {
        let (inner, fs) = setup();
        fs.write_atomic(&path(".onyx/settings.json"), b"{}")
            .unwrap();
        // Stored verbatim, readable without decryption.
        assert_eq!(inner.read(&path(".onyx/settings.json")).unwrap(), b"{}");
        assert_eq!(fs.read(&path(".onyx/settings.json")).unwrap(), b"{}");
    }

    #[test]
    fn wrong_key_cannot_read_and_skips_listing() {
        let inner = Arc::new(MemFs::new());
        let right = CryptoFs::new(inner.clone(), VaultKey::from_bytes([7; 32]));
        right.write_atomic(&path("note.md"), b"secret").unwrap();

        let wrong = CryptoFs::new(inner, VaultKey::from_bytes([8; 32]));
        // Names don't decrypt with the wrong key → invisible in listing.
        assert!(wrong.list().unwrap().is_empty());
        assert!(!wrong.exists(&path("note.md")));
    }

    #[test]
    fn foreign_files_in_directory_are_ignored() {
        let (inner, fs) = setup();
        fs.write_atomic(&path("mine.md"), b"ok").unwrap();
        inner
            .write_atomic(&path("stray-plaintext.txt"), b"not ours")
            .unwrap();
        let listed = fs.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0.as_str(), "mine.md");
    }

    #[test]
    fn rename_and_remove_work_through_encryption() {
        let (_, fs) = setup();
        fs.write_atomic(&path("old.md"), b"content").unwrap();
        fs.rename(&path("old.md"), &path("sub/new.md")).unwrap();
        assert!(!fs.exists(&path("old.md")));
        assert_eq!(fs.read(&path("sub/new.md")).unwrap(), b"content");
        fs.remove(&path("sub/new.md")).unwrap();
        assert!(!fs.exists(&path("sub/new.md")));
    }

    #[test]
    fn full_vault_stack_works_over_encryption() {
        let (_, fs) = setup();
        let vault = Vault::new(Arc::new(fs), VaultConfig::default());
        vault.write(&path("a.md"), b"# Alpha\n[[b]] #tag").unwrap();
        vault.write(&path("b.md"), b"# Beta").unwrap();

        let mut index = crate::index::Index::open_in_memory([0; 16]).unwrap();
        index.rebuild(&vault).unwrap();
        assert_eq!(index.note_count().unwrap(), 2);
        let b_id = vault.note_id(&path("b.md"));
        assert_eq!(index.backlinks(b_id).unwrap().len(), 1);
        assert_eq!(index.tags().unwrap()[0].tag, "tag");
    }
}
