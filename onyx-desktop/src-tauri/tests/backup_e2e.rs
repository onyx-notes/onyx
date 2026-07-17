//! Backup E2E against a real (filesystem) OpenDAL destination: snapshot,
//! convergent dedup, incremental upload, and standalone disaster restore.

use std::collections::HashMap;

use onyx_crypto::VaultKey;
use onyx_desktop_lib::backup::{
    BackupDestination, backup_key, list_snapshots, restore, run_backup,
};

fn fs_destination(root: &std::path::Path) -> BackupDestination {
    BackupDestination {
        name: "test".into(),
        kind: "fs".into(),
        config: HashMap::from([("root".to_owned(), root.to_string_lossy().into_owned())]),
    }
}

#[test]
fn backup_dedup_and_restore_roundtrip() {
    let store = tempfile::tempdir().unwrap();
    let destination = fs_destination(store.path());
    let key = VaultKey::from_bytes([21; 32]);

    let binary: Vec<u8> = (0..4096u32).map(|byte| (byte % 251) as u8).collect();
    let files: Vec<(String, Vec<u8>)> = vec![
        ("notes/alpha.md".into(), b"# Alpha\ncontent".to_vec()),
        ("notes/beta.md".into(), b"# Beta".to_vec()),
        ("assets/pic.png".into(), binary.clone()),
    ];

    // First backup: everything uploads.
    let first = run_backup(&key, &files, &destination).unwrap();
    assert_eq!(first.files, 3);
    assert_eq!(first.uploaded, 3);
    assert_eq!(first.skipped, 0);

    // Second backup, unchanged: full dedup, zero bytes moved.
    let second = run_backup(&key, &files, &destination).unwrap();
    assert_eq!(second.uploaded, 0);
    assert_eq!(second.skipped, 3);
    assert_eq!(second.bytes_uploaded, 0);

    // One file changes: exactly one chunk uploads.
    let mut updated = files.clone();
    updated[0].1 = b"# Alpha\nedited content".to_vec();
    let third = run_backup(&key, &updated, &destination).unwrap();
    assert_eq!(third.uploaded, 1);
    assert_eq!(third.skipped, 2);

    // Three snapshots listed, newest first.
    let snapshots = list_snapshots(&destination).unwrap();
    assert_eq!(snapshots.len(), 3);
    assert!(snapshots[0] >= snapshots[1] && snapshots[1] >= snapshots[2]);

    // Disaster restore of the NEWEST snapshot into an empty directory:
    // needs only the destination + key.
    let recovered = tempfile::tempdir().unwrap();
    let count = restore(&key, &destination, snapshots[0], recovered.path()).unwrap();
    assert_eq!(count, 3);
    assert_eq!(
        std::fs::read(recovered.path().join("notes/alpha.md")).unwrap(),
        b"# Alpha\nedited content"
    );
    assert_eq!(
        std::fs::read(recovered.path().join("assets/pic.png")).unwrap(),
        binary
    );

    // The OLDEST snapshot still restores the original content (chunks are
    // immutable; history is preserved).
    let older = tempfile::tempdir().unwrap();
    restore(&key, &destination, snapshots[2], older.path()).unwrap();
    assert_eq!(
        std::fs::read(older.path().join("notes/alpha.md")).unwrap(),
        b"# Alpha\ncontent"
    );

    // Nothing plaintext at the destination.
    let mut stack = vec![store.path().to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes.windows(7).any(|window| window == b"# Alpha"),
                "plaintext leaked into backup storage: {path:?}"
            );
        }
    }

    // Wrong key: restore fails, never silently returns garbage.
    let wrong = VaultKey::from_bytes([22; 32]);
    assert!(restore(&wrong, &destination, snapshots[0], recovered.path()).is_err());
}

#[test]
fn backup_keys_derive_and_persist() {
    // Plaintext vault: key persists at .onyx/backup.key across calls.
    let vault_dir = tempfile::tempdir().unwrap();
    let first = backup_key(vault_dir.path(), None).unwrap();
    let second = backup_key(vault_dir.path(), None).unwrap();
    let probe = onyx_crypto::encrypt_convergent(&first, b"probe");
    assert_eq!(probe, onyx_crypto::encrypt_convergent(&second, b"probe"));

    // Encrypted vault: derived deterministically, nothing stored.
    let vault_key = VaultKey::from_bytes([9; 32]);
    let scratch = tempfile::tempdir().unwrap();
    let derived_a = backup_key(scratch.path(), Some(&vault_key)).unwrap();
    let derived_b = backup_key(scratch.path(), Some(&vault_key)).unwrap();
    assert_eq!(
        onyx_crypto::encrypt_convergent(&derived_a, b"probe"),
        onyx_crypto::encrypt_convergent(&derived_b, b"probe")
    );
    assert!(!scratch.path().join(".onyx/backup.key").exists());
}
