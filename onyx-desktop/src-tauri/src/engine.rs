//! The engine: one open vault wired to its index, search, and
//! quick-switcher.
//!
//! Deliberately free of Tauri types so it can be tested headless and reused
//! by the mobile shells. The Tauri layer owns windows, IPC, and event
//! emission; the engine owns correctness.
//!
//! Update discipline: the vault's own writes update the index *synchronously*
//! in the writing call (the write journal then swallows the watcher echo).
//! External edits arrive via watcher events. Both paths converge on
//! [`Engine::apply_event`], so there is exactly one way state changes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use onyx_core::{
    CryptoFs, Index, NotePath, PathTranslator, QuickSwitcher, RealFs, SearchIndex, Vault,
    VaultConfig, VaultEvent, VaultFs,
};
use onyx_crypto::{KdfParams, Keyfile, VaultKey};

/// Marker + key material for encrypted vaults.
const KEYFILE_PATH: &str = ".onyx/vault.keyfile";

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Vault(#[from] onyx_core::VaultError),
    #[error("{0}")]
    Index(#[from] onyx_core::IndexError),
    #[error("{0}")]
    Search(#[from] onyx_core::SearchError),
    #[error("invalid path: {0}")]
    Path(#[from] onyx_core::PathError),
    #[error("no vault is open")]
    NoVault,
    #[error("this vault is encrypted — a passphrase is required")]
    PassphraseRequired,
    #[error("wrong passphrase or corrupted keyfile")]
    WrongPassphrase,
    #[error("a vault already exists at this location")]
    VaultExists,
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Sync(#[from] onyx_sync::SyncError),
    #[error("sync is not enabled for this vault")]
    SyncDisabled,
}

/// Is the directory an encrypted Onyx vault?
pub fn is_encrypted(root: &Path) -> bool {
    root.join(KEYFILE_PATH).is_file()
}

/// Per-vault sync state: the sidecar store, the op-encryption key, this
/// device's CRDT peer id, an in-memory doc cache, and the vault manifest
/// (per-doc liveness — the tombstone ledger).
pub struct SyncState {
    store: onyx_sync::SyncStore,
    key: VaultKey,
    peer: u64,
    docs: std::collections::HashMap<[u8; 16], onyx_sync::SyncDoc>,
    manifest: Option<onyx_sync::SyncDoc>,
    manifest_dirty: bool,
}

fn hex16(id: &[u8; 16]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex16(hex: &str) -> Option<[u8; 16]> {
    if hex.len() != 32 {
        return None;
    }
    let mut id = [0u8; 16];
    for (index, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(id)
}

impl SyncState {
    pub fn new(store: onyx_sync::SyncStore, key: VaultKey, peer: u64) -> Self {
        Self {
            store,
            key,
            peer,
            docs: std::collections::HashMap::new(),
            manifest: None,
            manifest_dirty: false,
        }
    }

    fn ensure_manifest(&mut self) -> Result<&onyx_sync::SyncDoc, onyx_sync::SyncError> {
        if self.manifest.is_none() {
            let doc = match self.store.load_doc(onyx_sync::MANIFEST_DOC_ID, self.peer)? {
                Some((doc, _)) => doc,
                None => onyx_sync::SyncDoc::new(self.peer),
            };
            self.manifest = Some(doc);
        }
        Ok(self.manifest.as_ref().expect("just ensured"))
    }

    fn manifest_live(&mut self, id: &[u8; 16]) -> Result<Option<bool>, onyx_sync::SyncError> {
        let hex = hex16(id);
        Ok(self.ensure_manifest()?.is_live(&hex))
    }

    fn manifest_set_live(&mut self, id: &[u8; 16], live: bool) -> Result<(), onyx_sync::SyncError> {
        let hex = hex16(id);
        self.ensure_manifest()?.set_live(&hex, live)?;
        self.manifest_dirty = true;
        Ok(())
    }

    fn save_manifest_if_dirty(&mut self) -> Result<(), onyx_sync::SyncError> {
        if self.manifest_dirty {
            if let Some(manifest) = &self.manifest {
                // The manifest has no file; its materialized hash is unused.
                self.store
                    .save_doc(onyx_sync::MANIFEST_DOC_ID, manifest, [0; 32])?;
            }
            self.manifest_dirty = false;
        }
        Ok(())
    }
}

/// One encrypted update ready to push, remembering the version it covers
/// so it's only marked pushed if THAT export succeeded (edits racing the
/// push stay dirty).
pub struct PendingPush {
    pub doc_id: [u8; 16],
    pub ciphertext: Vec<u8>,
    version: Vec<u8>,
}

/// One encrypted attachment ready for blob upload.
pub struct BlobUpload {
    pub path: String,
    pub blob_hash: String,
    pub ciphertext: Vec<u8>,
    plaintext_hash: [u8; 32],
}

/// `photo.png` → `photo (conflict).png` — the keep-both rename for
/// concurrently-modified binaries.
fn conflict_path(path: &str) -> String {
    match path.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} (conflict).{ext}"),
        _ => format!("{path} (conflict)"),
    }
}

pub struct Engine {
    root: PathBuf,
    vault: Vault,
    index: Index,
    search: SearchIndex,
    quick: QuickSwitcher,
    /// Present for encrypted vaults: translates on-disk ciphertext names
    /// to vault paths for the watcher.
    crypto: Option<Arc<CryptoFs>>,
    /// Present when sync is enabled for this vault.
    sync: Option<SyncState>,
    /// Search commits are debounced by the caller; this tracks dirtiness.
    search_dirty: bool,
}

impl Engine {
    /// Open a plaintext vault. Fails with [`EngineError::PassphraseRequired`]
    /// if the directory is actually an encrypted vault.
    pub fn open(root: &Path) -> Result<Self, EngineError> {
        if is_encrypted(root) {
            return Err(EngineError::PassphraseRequired);
        }
        let fs: Arc<dyn VaultFs> = Arc::new(RealFs::new(root));
        // Plaintext vaults persist their caches on disk.
        let index = Index::open(&root.join(".onyx/index.db"), [0; 16])?;
        let search = SearchIndex::open_in_dir(&root.join(".onyx/tantivy"))?;
        Self::build(root, fs, None, index, search)
    }

    /// Unlock and open an encrypted vault.
    pub fn open_encrypted(root: &Path, passphrase: &str) -> Result<Self, EngineError> {
        let keyfile =
            std::fs::read(root.join(KEYFILE_PATH)).map_err(|_| EngineError::PassphraseRequired)?;
        let key = Keyfile::open(&keyfile, passphrase).map_err(|_| EngineError::WrongPassphrase)?;
        Self::open_with_key(root, key)
    }

    /// Create a new encrypted vault at an empty (or fresh) directory.
    pub fn create_encrypted(root: &Path, passphrase: &str) -> Result<Self, EngineError> {
        if is_encrypted(root) {
            return Err(EngineError::VaultExists);
        }
        std::fs::create_dir_all(root.join(".onyx"))?;
        let key = VaultKey::generate();
        let keyfile = Keyfile::seal(&key, passphrase, KdfParams::DESKTOP)
            .map_err(|_| EngineError::WrongPassphrase)?;
        std::fs::write(root.join(KEYFILE_PATH), keyfile)?;
        Self::open_with_key(root, key)
    }

    fn open_with_key(root: &Path, key: VaultKey) -> Result<Self, EngineError> {
        let crypto = Arc::new(CryptoFs::new(Arc::new(RealFs::new(root)), key));
        let fs: Arc<dyn VaultFs> = crypto.clone();
        // Encrypted vaults keep ALL derived state in RAM — a plaintext
        // index or search directory on disk would defeat the encryption.
        let index = Index::open_in_memory([0; 16])?;
        let search = SearchIndex::open_in_ram()?;
        Self::build(root, fs, Some(crypto), index, search)
    }

    fn build(
        root: &Path,
        fs: Arc<dyn VaultFs>,
        crypto: Option<Arc<CryptoFs>>,
        mut index: Index,
        mut search: SearchIndex,
    ) -> Result<Self, EngineError> {
        let vault = Vault::new(fs, VaultConfig::default());
        index.reconcile(&vault)?;

        let mut quick = QuickSwitcher::new();
        for record in index.all_notes()? {
            quick.upsert(record.id, &record.title, record.path.as_str(), &[]);
            if record.is_markdown {
                let body = vault.read_text(&record.path)?;
                search.upsert(record.id, record.path.as_str(), &record.title, &body, &[])?;
            }
        }
        search.commit()?;

        Ok(Self {
            root: root.to_path_buf(),
            vault,
            index,
            search,
            quick,
            crypto,
            sync: None,
            search_dirty: false,
        })
    }

    pub fn is_encrypted_vault(&self) -> bool {
        self.crypto.is_some()
    }

    /// The vault key for encrypted vaults (sync derives its identity from
    /// it); `None` for plaintext vaults.
    pub fn crypto_key(&self) -> Option<VaultKey> {
        self.crypto
            .as_ref()
            .map(|crypto| crypto.vault_key().clone())
    }

    /// Watcher path translator for encrypted vaults (`None` for plaintext).
    pub fn path_translator(&self) -> Option<PathTranslator> {
        self.crypto.as_ref().map(|crypto| {
            let crypto = Arc::clone(crypto);
            let translator: PathTranslator = Arc::new(move |sealed| crypto.open_path(sealed));
            translator
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    pub fn quick(&self) -> &QuickSwitcher {
        &self.quick
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<onyx_core::SearchHit>, EngineError> {
        Ok(self.search.search(query, limit)?)
    }

    /// The single state-update path: apply one vault event to index,
    /// quick-switcher, and full-text search. Returns whether this was our
    /// own write echoing back (callers skip UI refresh for those).
    pub fn apply_event(&mut self, event: &VaultEvent) -> Result<bool, EngineError> {
        if self.vault.is_own_echo(event) {
            return Ok(true);
        }
        self.index.handle_event(&self.vault, event)?;

        match event {
            VaultEvent::Created(path) | VaultEvent::Modified(path) => {
                let id = self.vault.note_id(path);
                match self.index.note(id)? {
                    Some(record) => {
                        self.quick
                            .upsert(id, &record.title, record.path.as_str(), &[]);
                        if record.is_markdown {
                            let body = self.vault.read_text(&record.path)?;
                            self.search.upsert(
                                id,
                                record.path.as_str(),
                                &record.title,
                                &body,
                                &[],
                            )?;
                            self.search_dirty = true;
                        }
                    }
                    // Vanished before we processed the event.
                    None => self.forget(id)?,
                }
            }
            VaultEvent::Removed(path) => {
                let id = self.vault.note_id(path);
                self.forget(id)?;
            }
            VaultEvent::BulkChange => {
                // Reconcile already ran in handle_event; rebuild the
                // in-memory views from the reconciled index.
                self.quick = QuickSwitcher::new();
                for record in self.index.all_notes()? {
                    self.quick
                        .upsert(record.id, &record.title, record.path.as_str(), &[]);
                    if record.is_markdown {
                        let body = self.vault.read_text(&record.path)?;
                        self.search.upsert(
                            record.id,
                            record.path.as_str(),
                            &record.title,
                            &body,
                            &[],
                        )?;
                    }
                }
                self.search_dirty = true;
            }
        }
        Ok(false)
    }

    fn forget(&mut self, id: onyx_core::NoteId) -> Result<(), EngineError> {
        self.quick.remove(id);
        self.search.remove(id)?;
        self.search_dirty = true;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Sync
    // ------------------------------------------------------------------

    pub fn enable_sync(&mut self, sync: SyncState) {
        self.sync = Some(sync);
    }

    pub fn sync_enabled(&self) -> bool {
        self.sync.is_some()
    }

    /// The server delivery cursor (last op seq we've applied).
    pub fn sync_cursor(&self) -> u64 {
        self.sync
            .as_ref()
            .and_then(|sync| sync.store.meta("cursor").ok().flatten())
            .and_then(|bytes| bytes.try_into().ok().map(u64::from_le_bytes))
            .unwrap_or(0)
    }

    pub fn set_sync_cursor(&mut self, cursor: u64) -> Result<(), EngineError> {
        let sync = self.sync.as_mut().ok_or(EngineError::SyncDisabled)?;
        sync.store.set_meta("cursor", &cursor.to_le_bytes())?;
        Ok(())
    }

    /// The outbox pass: fold any file changes into their CRDT docs (the
    /// index's content hashes make this cheap for unchanged notes), then
    /// export an encrypted update for every doc the server hasn't seen.
    pub fn sync_collect(&mut self) -> Result<Vec<PendingPush>, EngineError> {
        use std::collections::hash_map::Entry;

        let sync = self.sync.as_mut().ok_or(EngineError::SyncDisabled)?;

        // 1. Fold changed/new files into docs.
        for record in self.index.all_notes()? {
            if !record.is_markdown {
                continue; // attachments: chunk sync in a later leg
            }
            let doc_id = *record.id.as_bytes();
            let stored_hash = sync.store.materialized_hash(doc_id)?;
            if stored_hash == Some(record.hash) {
                continue; // unchanged since last fold — no file read
            }
            let content = self.vault.read_text(&record.path)?;
            let content_hash = *blake3::hash(content.as_bytes()).as_bytes();
            if stored_hash == Some(content_hash) {
                continue; // index hash lagged; content actually unchanged
            }

            let doc = match sync.docs.entry(doc_id) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let doc = match sync.store.load_doc(doc_id, sync.peer)? {
                        Some((doc, _)) => doc,
                        None => {
                            let doc = onyx_sync::SyncDoc::new(sync.peer);
                            doc.set_path(record.path.as_str())?;
                            doc
                        }
                    };
                    entry.insert(doc)
                }
            };
            doc.set_text(&content)?;
            sync.store.save_doc(doc_id, doc, content_hash)?;
            // Recreating a tombstoned path resurrects it everywhere.
            if sync.manifest_live(&doc_id)? == Some(false) {
                sync.manifest_set_live(&doc_id, true)?;
            }
        }

        // 2. Tombstone scan: a doc whose file vanished was deleted locally.
        for doc_id in sync.store.all_doc_ids()? {
            if doc_id == onyx_sync::MANIFEST_DOC_ID || sync.manifest_live(&doc_id)? == Some(false) {
                continue;
            }
            let path_text = {
                let doc = match sync.docs.entry(doc_id) {
                    Entry::Occupied(entry) => entry.into_mut(),
                    Entry::Vacant(entry) => {
                        let Some((doc, _)) = sync.store.load_doc(doc_id, sync.peer)? else {
                            continue;
                        };
                        entry.insert(doc)
                    }
                };
                doc.path()
            };
            let Some(path_text) = path_text else { continue };
            let Ok(note_path) = NotePath::new(&path_text) else {
                continue;
            };
            if !self.vault.fs().exists(&note_path) {
                sync.manifest_set_live(&doc_id, false)?;
            }
        }
        // Persist manifest changes so they export in this same cycle.
        sync.save_manifest_if_dirty()?;

        // 3. Export everything not yet pushed (tombstoned note docs are
        // skipped — the tombstone itself travels via the manifest doc).
        let mut pushes = Vec::new();
        for doc_id in sync.store.all_doc_ids()? {
            // The manifest lives in its own slot, never the doc cache
            // (two live copies of one CRDT would silently diverge).
            if doc_id == onyx_sync::MANIFEST_DOC_ID {
                let pushed = sync.store.pushed_version(doc_id)?;
                let exported = {
                    let manifest = sync.ensure_manifest()?;
                    let current = manifest.version();
                    if current != pushed {
                        Some((current, manifest.export_from(&pushed)?))
                    } else {
                        None
                    }
                };
                if let Some((current, update)) = exported {
                    pushes.push(PendingPush {
                        doc_id,
                        ciphertext: onyx_crypto::encrypt(&sync.key, &update),
                        version: current,
                    });
                }
                continue;
            }
            if sync.manifest_live(&doc_id)? == Some(false) {
                continue;
            }
            let doc = match sync.docs.entry(doc_id) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let Some((doc, _)) = sync.store.load_doc(doc_id, sync.peer)? else {
                        continue;
                    };
                    entry.insert(doc)
                }
            };
            let pushed = sync.store.pushed_version(doc_id)?;
            let current = doc.version();
            if current != pushed {
                let update = doc.export_from(&pushed)?;
                pushes.push(PendingPush {
                    doc_id,
                    ciphertext: onyx_crypto::encrypt(&sync.key, &update),
                    version: current,
                });
            }
        }
        Ok(pushes)
    }

    /// Record a successful push: only the exported versions are marked, so
    /// edits that raced the push remain in the outbox.
    pub fn sync_mark_pushed(&mut self, pushes: &[PendingPush]) -> Result<(), EngineError> {
        let sync = self.sync.as_mut().ok_or(EngineError::SyncDisabled)?;
        for push in pushes {
            sync.store.set_pushed_version(push.doc_id, &push.version)?;
        }
        Ok(())
    }

    /// Apply remote ops: decrypt, merge (folding any un-synced local file
    /// edits first so nothing is overwritten), materialize changed docs to
    /// disk. Returns the vault paths whose files changed.
    pub fn sync_apply_remote(
        &mut self,
        ops: &[onyx_proto::StoredOp],
    ) -> Result<Vec<String>, EngineError> {
        use std::collections::hash_map::Entry;

        let sync = self.sync.as_mut().ok_or(EngineError::SyncDisabled)?;
        let mut changed_paths = Vec::new();

        for op in ops {
            let Ok(update) = onyx_crypto::decrypt(&sync.key, &op.ciphertext) else {
                tracing::warn!(seq = op.seq, "undecryptable op skipped (wrong key?)");
                continue;
            };

            // Manifest ops: merge, then act on liveness transitions.
            if op.doc_id == onyx_sync::MANIFEST_DOC_ID {
                let transitions = {
                    let manifest = sync.ensure_manifest()?;
                    let before: std::collections::HashMap<String, bool> =
                        manifest.liveness().into_iter().collect();
                    if manifest.import(&update).is_err() {
                        tracing::warn!(seq = op.seq, "malformed manifest update skipped");
                        continue;
                    }
                    manifest
                        .liveness()
                        .into_iter()
                        .filter(|(key, live)| before.get(key) != Some(live))
                        .collect::<Vec<_>>()
                };
                sync.manifest_dirty = true;

                for (hex, live) in transitions {
                    let Some(doc_id) = decode_hex16(&hex) else {
                        continue;
                    };
                    let path_text = match sync.docs.entry(doc_id) {
                        Entry::Occupied(entry) => entry.into_mut().path(),
                        Entry::Vacant(entry) => {
                            match sync.store.load_doc(doc_id, sync.peer)? {
                                Some((doc, _)) => entry.insert(doc).path(),
                                None => None, // doc unknown yet — its ops will follow
                            }
                        }
                    };
                    let Some(path_text) = path_text else { continue };
                    let Ok(note_path) = NotePath::new(&path_text) else {
                        continue;
                    };

                    if !live {
                        // Remote delete: honor it only if the local file has
                        // no un-synced edits; otherwise resurrect (edits win
                        // over deletes, per plan).
                        let Ok(file_content) = self.vault.read_text(&note_path) else {
                            continue; // already gone locally
                        };
                        let file_hash = *blake3::hash(file_content.as_bytes()).as_bytes();
                        if sync.store.materialized_hash(doc_id)? == Some(file_hash) {
                            self.vault.remove(&note_path)?;
                            self.index.handle_event(
                                &self.vault,
                                &VaultEvent::Removed(note_path.clone()),
                            )?;
                            let id = self.vault.note_id(&note_path);
                            self.quick.remove(id);
                            self.search.remove(id)?;
                            self.search_dirty = true;
                            changed_paths.push(path_text);
                        } else {
                            sync.manifest_set_live(&doc_id, true)?;
                        }
                    } else if !self.vault.fs().exists(&note_path) {
                        // Remote resurrect: rematerialize from doc state.
                        let text = match sync.docs.get(&doc_id) {
                            Some(doc) => doc.text(),
                            None => continue,
                        };
                        self.vault.write(&note_path, text.as_bytes())?;
                        self.index
                            .handle_event(&self.vault, &VaultEvent::Created(note_path.clone()))?;
                        let id = self.vault.note_id(&note_path);
                        if let Some(record) = self.index.note(id)? {
                            self.quick
                                .upsert(id, &record.title, record.path.as_str(), &[]);
                            self.search.upsert(
                                id,
                                record.path.as_str(),
                                &record.title,
                                &text,
                                &[],
                            )?;
                            self.search_dirty = true;
                        }
                        sync.store.save_doc(
                            doc_id,
                            sync.docs.get(&doc_id).expect("checked above"),
                            *blake3::hash(text.as_bytes()).as_bytes(),
                        )?;
                        changed_paths.push(path_text);
                    }
                }
                continue;
            }

            let doc = match sync.docs.entry(op.doc_id) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let doc = match sync.store.load_doc(op.doc_id, sync.peer)? {
                        Some((doc, _)) => doc,
                        None => onyx_sync::SyncDoc::new(sync.peer),
                    };
                    entry.insert(doc)
                }
            };

            // Fold un-synced local file edits BEFORE merging, so the CRDT
            // merge (not a file overwrite) resolves concurrency.
            if let Some(path_text) = doc.path() {
                if let Ok(note_path) = NotePath::new(&path_text) {
                    if let Ok(file_content) = self.vault.read_text(&note_path) {
                        let file_hash = *blake3::hash(file_content.as_bytes()).as_bytes();
                        let stored_hash = sync.store.materialized_hash(op.doc_id)?;
                        if stored_hash != Some(file_hash) {
                            doc.set_text(&file_content)?;
                        }
                    }
                }
            }

            if doc.import(&update).is_err() {
                tracing::warn!(seq = op.seq, "malformed CRDT update skipped");
                continue;
            }

            let Some(path_text) = doc.path() else {
                tracing::warn!(seq = op.seq, "op for doc with no path metadata");
                continue;
            };
            let Ok(note_path) = NotePath::new(&path_text) else {
                tracing::warn!(path = %path_text, "invalid path in synced doc");
                continue;
            };

            let merged = doc.text();
            let merged_hash = *blake3::hash(merged.as_bytes()).as_bytes();

            // Tombstoned docs update state only — no file materializes.
            // (doc's borrow of the cache ends here; it is re-fetched below.)
            let dead = sync.manifest_live(&op.doc_id)? == Some(false);
            if dead {
                let doc = sync.docs.get(&op.doc_id).expect("cached above");
                sync.store.save_doc(op.doc_id, doc, merged_hash)?;
                continue;
            }

            let on_disk = self.vault.read_text(&note_path).ok();
            if on_disk.as_deref() != Some(merged.as_str()) {
                self.vault.write(&note_path, merged.as_bytes())?;
                self.index
                    .handle_event(&self.vault, &VaultEvent::Modified(note_path.clone()))?;
                let id = self.vault.note_id(&note_path);
                if let Some(record) = self.index.note(id)? {
                    self.quick
                        .upsert(id, &record.title, record.path.as_str(), &[]);
                    self.search
                        .upsert(id, record.path.as_str(), &record.title, &merged, &[])?;
                    self.search_dirty = true;
                }
                changed_paths.push(path_text);
            }
            let doc = sync.docs.get(&op.doc_id).expect("cached above");
            sync.store.save_doc(op.doc_id, doc, merged_hash)?;
        }
        sync.save_manifest_if_dirty()?;
        Ok(changed_paths)
    }

    // ------------------------------------------------------------------
    // Attachment sync (content-addressed encrypted blobs)
    // ------------------------------------------------------------------

    /// Attachments whose content changed since last sync: encrypted and
    /// ready for blob upload. Also tombstones attachments whose files
    /// vanished (deletion propagates via the manifest attachment map).
    pub fn attachments_to_upload(&mut self) -> Result<Vec<BlobUpload>, EngineError> {
        let sync = self.sync.as_mut().ok_or(EngineError::SyncDisabled)?;
        let mut uploads = Vec::new();

        for record in self.index.all_notes()? {
            if record.is_markdown {
                continue;
            }
            let path = record.path.as_str().to_owned();
            let stored = sync.store.attachment(&path)?;
            if stored.as_ref().map(|(hash, _, _)| *hash) == Some(record.hash) {
                continue;
            }
            let content = self.vault.read_bytes(&record.path)?;
            let plaintext_hash = *blake3::hash(&content).as_bytes();
            if stored.as_ref().map(|(hash, _, _)| *hash) == Some(plaintext_hash) {
                continue;
            }
            let ciphertext = onyx_crypto::encrypt(&sync.key, &content);
            let blob_hash = blake3::hash(&ciphertext).to_hex().to_string();
            uploads.push(BlobUpload {
                path,
                blob_hash,
                ciphertext,
                plaintext_hash,
            });
        }

        // Deletion scan: previously-synced attachments whose file is gone.
        for path in sync.store.all_attachment_paths()? {
            let Ok(note_path) = NotePath::new(&path) else {
                continue;
            };
            if !self.vault.fs().exists(&note_path) {
                sync.ensure_manifest()?.set_attachment(&path, "")?;
                sync.manifest_dirty = true;
                sync.store.remove_attachment(&path)?;
            }
        }
        Ok(uploads)
    }

    /// Record completed uploads in the manifest + sidecar. Call only after
    /// the blobs are confirmed on the server (receivers fetch by hash).
    pub fn attachments_mark_uploaded(&mut self, uploads: &[BlobUpload]) -> Result<(), EngineError> {
        let sync = self.sync.as_mut().ok_or(EngineError::SyncDisabled)?;
        for upload in uploads {
            sync.ensure_manifest()?
                .set_attachment(&upload.path, &upload.blob_hash)?;
            sync.manifest_dirty = true;
            sync.store.set_attachment(
                &upload.path,
                upload.plaintext_hash,
                &upload.blob_hash,
                true, // pending until the merged manifest confirms our blob
            )?;
        }
        Ok(())
    }

    /// Apply attachment deletions from the (already-merged) manifest.
    /// Returns changed paths.
    pub fn apply_attachment_deletes(&mut self) -> Result<Vec<String>, EngineError> {
        let sync = self.sync.as_mut().ok_or(EngineError::SyncDisabled)?;
        let entries = sync.ensure_manifest()?.attachments();
        let mut changed = Vec::new();
        for (path, blob_hash) in entries {
            if !blob_hash.is_empty() {
                continue;
            }
            let Ok(note_path) = NotePath::new(&path) else {
                continue;
            };
            // Only delete what we know we synced (a brand-new local file at
            // the same path must not be destroyed by an old tombstone).
            let Some((synced_hash, _, _)) = sync.store.attachment(&path)? else {
                continue;
            };
            if let Ok(content) = self.vault.read_bytes(&note_path) {
                if *blake3::hash(&content).as_bytes() == synced_hash {
                    self.vault.remove(&note_path)?;
                    self.index
                        .handle_event(&self.vault, &VaultEvent::Removed(note_path.clone()))?;
                    self.quick.remove(self.vault.note_id(&note_path));
                    changed.push(path.clone());
                }
            }
            sync.store.remove_attachment(&path)?;
        }
        Ok(changed)
    }

    /// Attachments the manifest says exist but we don't have (or have an
    /// outdated copy of): `(path, blob_hash)` pairs to download.
    pub fn attachments_needed(&mut self) -> Result<Vec<(String, String)>, EngineError> {
        let sync = self.sync.as_mut().ok_or(EngineError::SyncDisabled)?;
        let mut needed = Vec::new();
        for (path, blob_hash) in sync.ensure_manifest()?.attachments() {
            if blob_hash.is_empty() {
                continue;
            }
            let stored = sync.store.attachment(&path)?;
            if stored.as_ref().map(|(_, blob, _)| blob.clone()) == Some(blob_hash.clone()) {
                // The merged manifest agrees with our blob: acknowledge.
                if stored.is_some_and(|(_, _, pending)| pending) {
                    sync.store.ack_attachment(&path)?;
                }
                continue;
            }
            needed.push((path, blob_hash));
        }
        Ok(needed)
    }

    /// Store a downloaded attachment blob. If the local file was modified
    /// while a different version synced, the local version is kept beside
    /// it as a conflict copy (keep-both, never silent loss). Returns the
    /// paths changed.
    pub fn attachment_store(
        &mut self,
        path: &str,
        blob_hash: &str,
        ciphertext: &[u8],
    ) -> Result<Vec<String>, EngineError> {
        let sync = self.sync.as_mut().ok_or(EngineError::SyncDisabled)?;
        let plaintext = onyx_crypto::decrypt(&sync.key, ciphertext)
            .map_err(|error| EngineError::Sync(onyx_sync::SyncError::Corrupt(error.to_string())))?;
        let note_path = NotePath::new(path)?;
        let mut changed = Vec::new();

        if let Ok(existing) = self.vault.read_bytes(&note_path) {
            let existing_hash = *blake3::hash(&existing).as_bytes();
            let stored = sync.store.attachment(path)?;
            if existing_hash == *blake3::hash(&plaintext).as_bytes() {
                // Already identical: record and done.
                sync.store
                    .set_attachment(path, existing_hash, blob_hash, false)?;
                return Ok(changed);
            }
            // Keep-both: the local file was modified after the last sync
            // record and a different remote version is landing — rename
            // ours aside first.
            //
            // Concurrent edits that BOTH already uploaded resolve by
            // deterministic LWW (documented v1 semantics for binaries;
            // races within one sync window are rare for attachments). The
            // causal fix — per-attachment CRDT docs reusing the note-doc
            // machinery — is scheduled for the sync polish pass.
            let locally_dirty = stored
                .as_ref()
                .is_some_and(|(hash, _, _)| *hash != existing_hash);
            if locally_dirty {
                let conflict = conflict_path(path);
                if let Ok(conflict_note) = NotePath::new(&conflict) {
                    self.vault.rename(&note_path, &conflict_note)?;
                    self.index
                        .handle_event(&self.vault, &VaultEvent::Created(conflict_note))?;
                    changed.push(conflict);
                }
            }
        }

        self.vault.write(&note_path, &plaintext)?;
        self.index
            .handle_event(&self.vault, &VaultEvent::Modified(note_path.clone()))?;
        let id = self.vault.note_id(&note_path);
        if let Some(record) = self.index.note(id)? {
            self.quick
                .upsert(id, &record.title, record.path.as_str(), &[]);
        }
        sync.store
            .set_attachment(path, *blake3::hash(&plaintext).as_bytes(), blob_hash, false)?;
        changed.push(path.to_owned());
        Ok(changed)
    }

    /// Flush pending search changes (debounced by the event loop).
    pub fn commit_search_if_dirty(&mut self) -> Result<(), EngineError> {
        if self.search_dirty {
            self.search.commit()?;
            self.search_dirty = false;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Write operations (used by IPC commands; index updates synchronously)
    // ------------------------------------------------------------------

    pub fn write_note(&mut self, path: &NotePath, content: &str) -> Result<(), EngineError> {
        let existed = self.vault.fs().exists(path);
        self.vault.write(path, content.as_bytes())?;
        let event = if existed {
            VaultEvent::Modified(path.clone())
        } else {
            VaultEvent::Created(path.clone())
        };
        // Bypass echo detection: we *want* this update applied here; the
        // journal entry exists to swallow the upcoming watcher echo.
        self.index.handle_event(&self.vault, &event)?;
        let id = self.vault.note_id(path);
        if let Some(record) = self.index.note(id)? {
            self.quick
                .upsert(id, &record.title, record.path.as_str(), &[]);
            if record.is_markdown {
                self.search
                    .upsert(id, record.path.as_str(), &record.title, content, &[])?;
                self.search_dirty = true;
            }
        }
        Ok(())
    }

    pub fn delete_note(&mut self, path: &NotePath) -> Result<(), EngineError> {
        self.vault.remove(path)?;
        let id = self.vault.note_id(path);
        self.index
            .handle_event(&self.vault, &VaultEvent::Removed(path.clone()))?;
        self.forget(id)
    }

    pub fn rename_note(&mut self, from: &NotePath, to: &NotePath) -> Result<(), EngineError> {
        self.vault.rename(from, to)?;
        let from_id = self.vault.note_id(from);
        self.index
            .handle_event(&self.vault, &VaultEvent::Removed(from.clone()))?;
        self.forget(from_id)?;
        // Reuse the write path's indexing via a synthetic Created event.
        self.index
            .handle_event(&self.vault, &VaultEvent::Created(to.clone()))?;
        let to_id = self.vault.note_id(to);
        if let Some(record) = self.index.note(to_id)? {
            self.quick
                .upsert(to_id, &record.title, record.path.as_str(), &[]);
            if record.is_markdown {
                let body = self.vault.read_text(&record.path)?;
                self.search
                    .upsert(to_id, record.path.as_str(), &record.title, &body, &[])?;
                self.search_dirty = true;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(path: &str) -> NotePath {
        NotePath::new(path).unwrap()
    }

    fn open_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let target = dir.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, content).unwrap();
        }
        let engine = Engine::open(dir.path()).unwrap();
        (dir, engine)
    }

    #[test]
    fn open_indexes_existing_vault() {
        let (_dir, engine) = open_with(&[
            ("a.md", "# Alpha\nsearchable-alpha content [[Beta]]"),
            ("sub/Beta.md", "# Beta"),
            (".obsidian/app.json", "{}"),
        ]);
        assert_eq!(engine.index().note_count().unwrap(), 2);
        assert_eq!(engine.search("searchable-alpha", 5).unwrap().len(), 1);
        // Quick-switcher matches titles = filename stems (Obsidian semantics).
        assert_eq!(engine.quick().query("beta", 5).len(), 1);
    }

    #[test]
    fn write_note_is_immediately_visible_everywhere() {
        let (_dir, mut engine) = open_with(&[]);
        engine
            .write_note(&note("fresh.md"), "# Fresh\nbrand-new-token")
            .unwrap();
        engine.commit_search_if_dirty().unwrap();

        assert_eq!(engine.index().note_count().unwrap(), 1);
        assert_eq!(engine.search("brand-new-token", 5).unwrap().len(), 1);
        assert_eq!(engine.quick().query("fresh", 5).len(), 1);
        // And the bytes are really on disk.
        assert_eq!(
            engine.vault().read_text(&note("fresh.md")).unwrap(),
            "# Fresh\nbrand-new-token"
        );
    }

    #[test]
    fn own_write_watcher_echo_is_detected_once() {
        let (dir, mut engine) = open_with(&[]);
        engine.write_note(&note("a.md"), "content").unwrap();
        // The watcher will deliver this event; apply_event must flag it as
        // our echo (and not double-apply).
        let echo = engine
            .apply_event(&VaultEvent::Created(note("a.md")))
            .unwrap();
        assert!(echo);
        // A genuinely external change is not an echo.
        std::fs::write(dir.path().join("a.md"), "external edit").unwrap();
        let echo = engine
            .apply_event(&VaultEvent::Modified(note("a.md")))
            .unwrap();
        assert!(!echo);
    }

    #[test]
    fn external_event_updates_all_views() {
        let (dir, mut engine) = open_with(&[]);
        std::fs::write(dir.path().join("External.md"), "# External\nxyzzy-token").unwrap();
        engine
            .apply_event(&VaultEvent::Created(note("External.md")))
            .unwrap();
        engine.commit_search_if_dirty().unwrap();
        assert_eq!(engine.search("xyzzy-token", 5).unwrap().len(), 1);
        assert_eq!(engine.quick().query("external", 5).len(), 1);
    }

    #[test]
    fn delete_and_rename_propagate() {
        let (_dir, mut engine) = open_with(&[("old.md", "# Old\nfindme-token")]);
        engine
            .rename_note(&note("old.md"), &note("new.md"))
            .unwrap();
        engine.commit_search_if_dirty().unwrap();

        assert!(engine.quick().query("old", 5).is_empty());
        assert_eq!(engine.quick().query("new", 5).len(), 1);
        let hits = engine.search("findme-token", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "new.md");

        engine.delete_note(&note("new.md")).unwrap();
        engine.commit_search_if_dirty().unwrap();
        assert_eq!(engine.index().note_count().unwrap(), 0);
        assert!(engine.search("findme-token", 5).unwrap().is_empty());
    }

    #[test]
    fn encrypted_vault_full_lifecycle() {
        let dir = tempfile::tempdir().unwrap();

        // Create, write, verify everything works.
        {
            let mut engine = Engine::create_encrypted(dir.path(), "correct horse").unwrap();
            assert!(engine.is_encrypted_vault());
            engine
                .write_note(
                    &note("Secret Plans.md"),
                    "# Plans\nclassified-token [[Other]]",
                )
                .unwrap();
            engine.commit_search_if_dirty().unwrap();
            assert_eq!(engine.search("classified-token", 5).unwrap().len(), 1);
            assert_eq!(engine.quick().query("secret", 5).len(), 1);
        }

        // Nothing legible on disk: no plaintext names, no plaintext index.
        let mut plaintext_leaks = Vec::new();
        for entry in walk(dir.path()) {
            let name = entry.to_string_lossy().to_lowercase();
            if name.contains("secret") || name.contains("plans") {
                plaintext_leaks.push(entry.clone());
            }
            if entry.is_file() {
                let bytes = std::fs::read(&entry).unwrap();
                assert!(
                    !bytes
                        .windows("classified-token".len())
                        .any(|window| window == b"classified-token"),
                    "plaintext content leaked into {entry:?}"
                );
            }
        }
        assert!(
            plaintext_leaks.is_empty(),
            "leaked names: {plaintext_leaks:?}"
        );
        assert!(!dir.path().join(".onyx/index.db").exists());
        assert!(!dir.path().join(".onyx/tantivy").exists());

        // Wrong passphrase fails; right one unlocks with content intact.
        assert!(matches!(
            Engine::open_encrypted(dir.path(), "wrong"),
            Err(EngineError::WrongPassphrase)
        ));
        let engine = Engine::open_encrypted(dir.path(), "correct horse").unwrap();
        assert_eq!(engine.index().note_count().unwrap(), 1);
        assert_eq!(engine.search("classified-token", 5).unwrap().len(), 1);
        assert_eq!(
            engine.vault().read_text(&note("Secret Plans.md")).unwrap(),
            "# Plans\nclassified-token [[Other]]"
        );

        // Plain open refuses and demands a passphrase.
        assert!(matches!(
            Engine::open(dir.path()),
            Err(EngineError::PassphraseRequired)
        ));
    }

    #[test]
    fn create_encrypted_refuses_existing_encrypted_vault() {
        let dir = tempfile::tempdir().unwrap();
        let _first = Engine::create_encrypted(dir.path(), "pw").unwrap();
        assert!(matches!(
            Engine::create_encrypted(dir.path(), "other"),
            Err(EngineError::VaultExists)
        ));
    }

    fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path.clone());
                }
                files.push(path);
            }
        }
        files
    }

    #[test]
    fn reopen_reuses_persisted_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "# A\npersisted-token").unwrap();
        {
            let _first = Engine::open(dir.path()).unwrap();
        }
        // Second open: index.db and tantivy dir already exist.
        let engine = Engine::open(dir.path()).unwrap();
        assert_eq!(engine.index().note_count().unwrap(), 1);
        assert_eq!(engine.search("persisted-token", 5).unwrap().len(), 1);
    }
}

#[cfg(test)]
mod sync_tests {
    use onyx_crypto::VaultKey;
    use onyx_sync::SyncStore;

    use super::*;

    fn note(path: &str) -> NotePath {
        NotePath::new(path).unwrap()
    }

    fn sync_engine(dir: &std::path::Path, peer: u64) -> Engine {
        let mut engine = Engine::open(dir).unwrap();
        let store = SyncStore::open(&dir.join(".onyx/sync.db")).unwrap();
        engine.enable_sync(SyncState::new(store, VaultKey::from_bytes([9; 32]), peer));
        engine
    }

    /// Wrap pending pushes as server-stored ops (what a pull would return).
    fn as_stored(pushes: &[PendingPush], seq_start: u64) -> Vec<onyx_proto::StoredOp> {
        pushes
            .iter()
            .enumerate()
            .map(|(index, push)| onyx_proto::StoredOp {
                seq: seq_start + index as u64,
                doc_id: push.doc_id,
                ciphertext: push.ciphertext.clone(),
            })
            .collect()
    }

    #[test]
    fn notes_flow_between_engines() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        std::fs::write(dir_a.path().join("shared.md"), "# Shared\nfrom device A\n").unwrap();

        let mut a = sync_engine(dir_a.path(), 1);
        let mut b = sync_engine(dir_b.path(), 2);

        // A pushes; B applies: the note appears on B's disk AND in B's index.
        let pushes = a.sync_collect().unwrap();
        assert_eq!(pushes.len(), 1);
        let changed = b.sync_apply_remote(&as_stored(&pushes, 1)).unwrap();
        a.sync_mark_pushed(&pushes).unwrap();
        assert_eq!(changed, vec!["shared.md".to_owned()]);
        assert_eq!(
            std::fs::read_to_string(dir_b.path().join("shared.md")).unwrap(),
            "# Shared\nfrom device A\n"
        );
        assert_eq!(b.index().note_count().unwrap(), 1);

        // Nothing further to push from A (collect is idempotent)…
        assert!(a.sync_collect().unwrap().is_empty());
        // …and B's apply didn't create phantom outbox entries beyond its
        // own copy of the doc (B pushes its imported state once).
        let b_pushes = b.sync_collect().unwrap();
        b.sync_mark_pushed(&b_pushes).unwrap();

        // Concurrent edits on both devices.
        b.write_note(
            &note("shared.md"),
            "# Shared\nfrom device A\nB's addition\n",
        )
        .unwrap();
        a.write_note(&note("shared.md"), "# Shared (A's title)\nfrom device A\n")
            .unwrap();

        // Exchange through the "server".
        let from_b = b.sync_collect().unwrap();
        let changed_a = a.sync_apply_remote(&as_stored(&from_b, 10)).unwrap();
        b.sync_mark_pushed(&from_b).unwrap();
        assert_eq!(changed_a.len(), 1);

        let from_a = a.sync_collect().unwrap();
        let changed_b = b.sync_apply_remote(&as_stored(&from_a, 20)).unwrap();
        a.sync_mark_pushed(&from_a).unwrap();
        assert_eq!(changed_b.len(), 1);

        // Both merged, neither edit lost.
        let text_a = a.vault().read_text(&note("shared.md")).unwrap();
        let text_b = b.vault().read_text(&note("shared.md")).unwrap();
        assert_eq!(text_a, text_b);
        assert!(text_a.contains("B's addition"));
        assert!(text_a.contains("(A's title)"));
    }

    #[test]
    fn own_ops_replayed_from_server_are_harmless() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "content").unwrap();
        let mut engine = sync_engine(dir.path(), 1);

        let pushes = engine.sync_collect().unwrap();
        engine.sync_mark_pushed(&pushes).unwrap();
        // The pull returns our own ops: applying them must be a no-op.
        let changed = engine.sync_apply_remote(&as_stored(&pushes, 1)).unwrap();
        assert!(changed.is_empty());
        assert!(engine.sync_collect().unwrap().is_empty());
    }

    #[test]
    fn deletes_propagate_between_engines() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        std::fs::write(dir_a.path().join("doomed.md"), "delete me").unwrap();
        std::fs::write(dir_a.path().join("keeper.md"), "keep me").unwrap();

        let mut a = sync_engine(dir_a.path(), 1);
        let mut b = sync_engine(dir_b.path(), 2);

        // Initial replication A → B.
        let pushes = a.sync_collect().unwrap();
        b.sync_apply_remote(&as_stored(&pushes, 1)).unwrap();
        a.sync_mark_pushed(&pushes).unwrap();
        let b_ack = b.sync_collect().unwrap();
        b.sync_mark_pushed(&b_ack).unwrap();
        assert!(dir_b.path().join("doomed.md").exists());

        // A deletes the note (through the engine, like the UI does).
        a.write_note(&note("dummy-touch.md"), "x").unwrap(); // unrelated churn
        a.delete_note(&note("doomed.md")).unwrap();

        // A's next collect tombstones it; B applies and the file dies.
        let pushes = a.sync_collect().unwrap();
        let changed = b.sync_apply_remote(&as_stored(&pushes, 100)).unwrap();
        a.sync_mark_pushed(&pushes).unwrap();
        assert!(changed.contains(&"doomed.md".to_owned()));
        assert!(!dir_b.path().join("doomed.md").exists());
        assert!(dir_b.path().join("keeper.md").exists());
        // …and it's gone from B's index too.
        let gone_id = b.vault().note_id(&note("doomed.md"));
        assert!(b.index().note(gone_id).unwrap().is_none());

        // The delete does NOT boomerang back to resurrect on A.
        let from_b = b.sync_collect().unwrap();
        let changed_a = a.sync_apply_remote(&as_stored(&from_b, 200)).unwrap();
        b.sync_mark_pushed(&from_b).unwrap();
        assert!(!changed_a.contains(&"doomed.md".to_owned()));
        assert!(!dir_a.path().join("doomed.md").exists());
    }

    #[test]
    fn concurrent_edit_beats_delete() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        std::fs::write(dir_a.path().join("contested.md"), "original").unwrap();

        let mut a = sync_engine(dir_a.path(), 1);
        let mut b = sync_engine(dir_b.path(), 2);
        let pushes = a.sync_collect().unwrap();
        b.sync_apply_remote(&as_stored(&pushes, 1)).unwrap();
        a.sync_mark_pushed(&pushes).unwrap();
        let b_ack = b.sync_collect().unwrap();
        b.sync_mark_pushed(&b_ack).unwrap();

        // Concurrently: A deletes, B edits.
        a.delete_note(&note("contested.md")).unwrap();
        b.write_note(&note("contested.md"), "original plus B's edit")
            .unwrap();

        // A's tombstone reaches B — but B has un-synced local edits, so B
        // resurrects instead of deleting.
        let from_a = a.sync_collect().unwrap();
        b.sync_apply_remote(&as_stored(&from_a, 100)).unwrap();
        a.sync_mark_pushed(&from_a).unwrap();
        assert!(dir_b.path().join("contested.md").exists());

        // B's resurrection + content flows back to A: the file returns.
        let from_b = b.sync_collect().unwrap();
        let changed_a = a.sync_apply_remote(&as_stored(&from_b, 200)).unwrap();
        b.sync_mark_pushed(&from_b).unwrap();
        assert!(changed_a.contains(&"contested.md".to_owned()));
        assert_eq!(
            std::fs::read_to_string(dir_a.path().join("contested.md")).unwrap(),
            "original plus B's edit"
        );
    }

    #[test]
    fn delete_then_recreate_resurrects() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        std::fs::write(dir_a.path().join("phoenix.md"), "first life").unwrap();

        let mut a = sync_engine(dir_a.path(), 1);
        let mut b = sync_engine(dir_b.path(), 2);
        let pushes = a.sync_collect().unwrap();
        b.sync_apply_remote(&as_stored(&pushes, 1)).unwrap();
        a.sync_mark_pushed(&pushes).unwrap();

        // Delete, sync the tombstone over, then recreate on A.
        a.delete_note(&note("phoenix.md")).unwrap();
        let from_a = a.sync_collect().unwrap();
        b.sync_apply_remote(&as_stored(&from_a, 100)).unwrap();
        a.sync_mark_pushed(&from_a).unwrap();
        assert!(!dir_b.path().join("phoenix.md").exists());

        a.write_note(&note("phoenix.md"), "second life").unwrap();
        let from_a = a.sync_collect().unwrap();
        let changed = b.sync_apply_remote(&as_stored(&from_a, 200)).unwrap();
        a.sync_mark_pushed(&from_a).unwrap();
        assert!(changed.contains(&"phoenix.md".to_owned()));
        assert_eq!(
            std::fs::read_to_string(dir_b.path().join("phoenix.md")).unwrap(),
            "second life"
        );
    }

    #[test]
    fn cursor_roundtrip_and_wrong_key_ops_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = sync_engine(dir.path(), 1);
        assert_eq!(engine.sync_cursor(), 0);
        engine.set_sync_cursor(17).unwrap();
        assert_eq!(engine.sync_cursor(), 17);

        // An op encrypted with a different key is skipped, not fatal.
        let foreign = onyx_proto::StoredOp {
            seq: 1,
            doc_id: [5; 16],
            ciphertext: onyx_crypto::encrypt(&VaultKey::from_bytes([8; 32]), b"data"),
        };
        let changed = engine.sync_apply_remote(&[foreign]).unwrap();
        assert!(changed.is_empty());
    }
}
