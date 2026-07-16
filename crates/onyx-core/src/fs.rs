//! The filesystem seam: everything above it (vault, watcher reconciliation,
//! future encryption layer) is oblivious to what actually stores the bytes.
//!
//! Two implementations: `RealFs` for production, `MemFs` for fast, hermetic
//! tests. A `CryptoFs` decorator slots in here later without touching any
//! caller.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use parking_lot::RwLock;

use crate::paths::NotePath;

/// Cheap file metadata used for change detection (`mtime` + `size` first,
/// content hash only on mismatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    pub size: u64,
    pub mtime: SystemTime,
}

/// Vault-scoped filesystem operations. All paths are vault-relative
/// [`NotePath`]s; implementations own the mapping to real storage.
pub trait VaultFs: Send + Sync {
    fn read(&self, path: &NotePath) -> io::Result<Vec<u8>>;

    /// Write atomically: the file at `path` either keeps its old content or
    /// has exactly `data` — never a torn mix — even across a crash.
    fn write_atomic(&self, path: &NotePath, data: &[u8]) -> io::Result<()>;

    fn remove(&self, path: &NotePath) -> io::Result<()>;

    fn rename(&self, from: &NotePath, to: &NotePath) -> io::Result<()>;

    fn stat(&self, path: &NotePath) -> io::Result<FileStat>;

    /// All files in the vault, recursively. No filtering: callers apply
    /// ignore rules. Symlinks are not followed (cycle and double-index
    /// safety).
    fn list(&self) -> io::Result<Vec<(NotePath, FileStat)>>;

    fn exists(&self, path: &NotePath) -> bool;
}

// ---------------------------------------------------------------------------
// RealFs
// ---------------------------------------------------------------------------

/// Production filesystem rooted at a vault directory.
pub struct RealFs {
    root: PathBuf,
}

impl RealFs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, path: &NotePath) -> PathBuf {
        // NotePath validation already rejects absolute paths and dot
        // segments, so a simple join cannot escape the root.
        self.root.join(path.as_str())
    }
}

impl VaultFs for RealFs {
    fn read(&self, path: &NotePath) -> io::Result<Vec<u8>> {
        std::fs::read(self.resolve(path))
    }

    fn write_atomic(&self, path: &NotePath, data: &[u8]) -> io::Result<()> {
        let target = self.resolve(path);
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("path has no parent"))?;
        std::fs::create_dir_all(parent)?;

        // Temp file in the *same directory* guarantees the rename is atomic
        // (same filesystem) and invisible to vault listings (dot prefix).
        let mut temp = tempfile::Builder::new()
            .prefix(".onyx-write-")
            .tempfile_in(parent)?;
        io::Write::write_all(&mut temp, data)?;
        temp.as_file().sync_all()?;
        temp.persist(&target).map_err(|error| error.error)?;

        // fsync the directory so the rename itself survives a crash.
        // Windows can't open directories this way; rename durability is
        // handled by the filesystem there.
        #[cfg(unix)]
        {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }

    fn remove(&self, path: &NotePath) -> io::Result<()> {
        std::fs::remove_file(self.resolve(path))
    }

    fn rename(&self, from: &NotePath, to: &NotePath) -> io::Result<()> {
        let target = self.resolve(to);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(self.resolve(from), target)
    }

    fn stat(&self, path: &NotePath) -> io::Result<FileStat> {
        let metadata = std::fs::metadata(self.resolve(path))?;
        Ok(FileStat {
            size: metadata.len(),
            mtime: metadata.modified()?,
        })
    }

    fn list(&self) -> io::Result<Vec<(NotePath, FileStat)>> {
        let mut files = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if file_type.is_symlink() {
                    continue;
                }
                let absolute = entry.path();
                if file_type.is_dir() {
                    stack.push(absolute);
                    continue;
                }
                let Ok(relative) = absolute.strip_prefix(&self.root) else {
                    continue;
                };
                let Some(relative_str) = relative.to_str() else {
                    // Non-UTF-8 names can't be notes; skip rather than fail
                    // the whole scan.
                    continue;
                };
                let Ok(note_path) = NotePath::new(relative_str) else {
                    continue;
                };
                let metadata = entry.metadata()?;
                files.push((
                    note_path,
                    FileStat {
                        size: metadata.len(),
                        mtime: metadata.modified()?,
                    },
                ));
            }
        }
        Ok(files)
    }

    fn exists(&self, path: &NotePath) -> bool {
        self.resolve(path).is_file()
    }
}

// ---------------------------------------------------------------------------
// MemFs
// ---------------------------------------------------------------------------

/// In-memory filesystem for hermetic tests. Paths compare by exact
/// normalized form (like a case-sensitive filesystem).
#[derive(Default)]
pub struct MemFs {
    files: RwLock<BTreeMap<NotePath, (Vec<u8>, SystemTime)>>,
}

impl MemFs {
    pub fn new() -> Self {
        Self::default()
    }
}

impl VaultFs for MemFs {
    fn read(&self, path: &NotePath) -> io::Result<Vec<u8>> {
        self.files
            .read()
            .get(path)
            .map(|(data, _)| data.clone())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn write_atomic(&self, path: &NotePath, data: &[u8]) -> io::Result<()> {
        self.files
            .write()
            .insert(path.clone(), (data.to_vec(), SystemTime::now()));
        Ok(())
    }

    fn remove(&self, path: &NotePath) -> io::Result<()> {
        self.files
            .write()
            .remove(path)
            .map(|_| ())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn rename(&self, from: &NotePath, to: &NotePath) -> io::Result<()> {
        let mut files = self.files.write();
        let entry = files
            .remove(from)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        files.insert(to.clone(), entry);
        Ok(())
    }

    fn stat(&self, path: &NotePath) -> io::Result<FileStat> {
        self.files
            .read()
            .get(path)
            .map(|(data, mtime)| FileStat {
                size: data.len() as u64,
                mtime: *mtime,
            })
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn list(&self) -> io::Result<Vec<(NotePath, FileStat)>> {
        Ok(self
            .files
            .read()
            .iter()
            .map(|(path, (data, mtime))| {
                (
                    path.clone(),
                    FileStat {
                        size: data.len() as u64,
                        mtime: *mtime,
                    },
                )
            })
            .collect())
    }

    fn exists(&self, path: &NotePath) -> bool {
        self.files.read().contains_key(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> NotePath {
        NotePath::new(text).unwrap()
    }

    fn exercise(fs: &dyn VaultFs) {
        let note = path("dir/note.md");
        assert!(!fs.exists(&note));
        fs.write_atomic(&note, b"hello").unwrap();
        assert!(fs.exists(&note));
        assert_eq!(fs.read(&note).unwrap(), b"hello");
        assert_eq!(fs.stat(&note).unwrap().size, 5);

        fs.write_atomic(&note, b"replaced content").unwrap();
        assert_eq!(fs.read(&note).unwrap(), b"replaced content");

        let renamed = path("other/loc.md");
        fs.rename(&note, &renamed).unwrap();
        assert!(!fs.exists(&note));
        assert_eq!(fs.read(&renamed).unwrap(), b"replaced content");

        let listed = fs.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, renamed);

        fs.remove(&renamed).unwrap();
        assert!(!fs.exists(&renamed));
        assert!(fs.read(&renamed).is_err());
    }

    #[test]
    fn memfs_contract() {
        exercise(&MemFs::new());
    }

    #[test]
    fn realfs_contract() {
        let dir = tempfile::tempdir().unwrap();
        exercise(&RealFs::new(dir.path()));
    }

    #[test]
    fn realfs_atomic_write_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let fs = RealFs::new(dir.path());
        fs.write_atomic(&path("a.md"), b"data").unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "only the target file: {names:?}");
    }

    #[test]
    fn realfs_list_skips_symlinks() {
        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().unwrap();
            let fs = RealFs::new(dir.path());
            fs.write_atomic(&path("real.md"), b"x").unwrap();
            std::os::unix::fs::symlink(dir.path().join("real.md"), dir.path().join("link.md"))
                .unwrap();
            // Symlinked dir cycle: root -> root/loop
            std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
            let listed = fs.list().unwrap();
            assert_eq!(listed.len(), 1);
            assert_eq!(listed[0].0.as_str(), "real.md");
        }
    }
}
