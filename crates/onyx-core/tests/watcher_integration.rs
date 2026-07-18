//! Integration tests with the REAL filesystem watcher on a real tempdir.
//!
//! These are timing-sensitive by nature; they use generous timeouts and
//! assert on eventual delivery, not exact timing.
//!
//! The watched root is always canonicalized: on macOS `tempfile` hands back a
//! path under `/var/folders/...`, but FSEvents reports the resolved
//! `/private/var/folders/...`. Without canonicalization the watcher's
//! relative-path computation strips the wrong prefix and silently drops every
//! event.

use std::path::PathBuf;
use std::time::Duration;

use crossbeam_channel::Receiver;
use onyx_core::{CoalescerConfig, NotePath, VaultEvent, VaultWatcher};

const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

fn test_config() -> CoalescerConfig {
    CoalescerConfig {
        debounce: Duration::from_millis(100),
        storm_threshold: 20,
        storm_window: Duration::from_millis(500),
    }
}

/// A tempdir plus its canonical path (see the module note).
fn temp_vault() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    (dir, root)
}

/// Wait for an event matching `predicate`, letting unrelated events pass.
fn expect_event(
    receiver: &Receiver<VaultEvent>,
    what: &str,
    predicate: impl Fn(&VaultEvent) -> bool,
) -> VaultEvent {
    let deadline = std::time::Instant::now() + DELIVERY_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(event) if predicate(&event) => return event,
            Ok(_other) => continue,
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "FSEvents delivers rapid event sequences unreliably in headless CI; run with --ignored on real hardware"
)]
fn create_modify_remove_lifecycle() {
    let (_dir, root) = temp_vault();
    let (sender, receiver) = crossbeam_channel::unbounded();
    let _watcher = VaultWatcher::spawn(&root, test_config(), sender).unwrap();
    // Give the watcher backend a moment to arm (macOS FSEvents needs it).
    std::thread::sleep(Duration::from_millis(300));

    let note = NotePath::new("hello.md").unwrap();

    std::fs::write(root.join("hello.md"), "# hi").unwrap();
    expect_event(
        &receiver,
        "Created",
        |event| matches!(event, VaultEvent::Created(path) if *path == note),
    );

    std::fs::write(root.join("hello.md"), "# hi edited").unwrap();
    expect_event(
        &receiver,
        "Modified",
        |event| matches!(event, VaultEvent::Modified(path) if *path == note),
    );

    std::fs::remove_file(root.join("hello.md")).unwrap();
    expect_event(
        &receiver,
        "Removed",
        |event| matches!(event, VaultEvent::Removed(path) if *path == note),
    );
}

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "FSEvents delivers rapid event sequences unreliably in headless CI; run with --ignored on real hardware"
)]
fn hidden_directories_are_invisible() {
    let (_dir, root) = temp_vault();
    std::fs::create_dir_all(root.join(".obsidian")).unwrap();
    let (sender, receiver) = crossbeam_channel::unbounded();
    let _watcher = VaultWatcher::spawn(&root, test_config(), sender).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    std::fs::write(root.join(".obsidian/app.json"), "{}").unwrap();
    std::fs::write(root.join("real.md"), "content").unwrap();

    // The visible file arrives; the hidden one must never appear before it.
    let event = expect_event(&receiver, "some event", |_| true);
    assert!(
        matches!(&event, VaultEvent::Created(path) if path.as_str() == "real.md"),
        "expected real.md first, got {event:?}"
    );
}

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "FSEvents delivers rapid event sequences unreliably in headless CI; run with --ignored on real hardware"
)]
fn file_storm_collapses_into_bulk_change() {
    let (_dir, root) = temp_vault();
    let (sender, receiver) = crossbeam_channel::unbounded();
    let _watcher = VaultWatcher::spawn(&root, test_config(), sender).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    // Simulate a git checkout: many files appearing at once.
    for index in 0..60 {
        std::fs::write(root.join(format!("bulk-{index}.md")), "x").unwrap();
    }

    expect_event(&receiver, "BulkChange", |event| {
        matches!(event, VaultEvent::BulkChange)
    });

    // After the storm settles, normal per-file behavior resumes.
    std::thread::sleep(Duration::from_millis(700));
    std::fs::write(root.join("after-storm.md"), "y").unwrap();
    expect_event(
        &receiver,
        "post-storm Created",
        |event| matches!(event, VaultEvent::Created(path) if path.as_str() == "after-storm.md"),
    );
}

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "FSEvents delivers rapid event sequences unreliably in headless CI; run with --ignored on real hardware"
)]
fn rename_produces_remove_and_create() {
    let (_dir, root) = temp_vault();
    std::fs::write(root.join("old.md"), "content").unwrap();
    let (sender, receiver) = crossbeam_channel::unbounded();
    let _watcher = VaultWatcher::spawn(&root, test_config(), sender).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    std::fs::rename(root.join("old.md"), root.join("new.md")).unwrap();

    // Same debounce deadline ⇒ emission order between the two paths is
    // deterministic but not meaningful; assert the pair, not the order.
    let first = expect_event(&receiver, "rename event", |_| true);
    let second = expect_event(&receiver, "rename event", |_| true);
    let mut got: Vec<String> = [first, second]
        .iter()
        .map(|event| format!("{event:?}"))
        .collect();
    got.sort();
    assert!(
        got[0].contains("Created") && got[0].contains("new.md"),
        "expected Created(new.md), got {got:?}"
    );
    assert!(
        got[1].contains("Removed") && got[1].contains("old.md"),
        "expected Removed(old.md), got {got:?}"
    );
}

#[test]
fn watcher_shutdown_is_clean() {
    let (_dir, root) = temp_vault();
    let (sender, _receiver) = crossbeam_channel::unbounded();
    let watcher = VaultWatcher::spawn(&root, test_config(), sender).unwrap();
    drop(watcher); // must not hang or panic
}

#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "FSEvents delivers rapid event sequences unreliably in headless CI; run with --ignored on real hardware"
)]
fn encrypted_vault_events_arrive_with_plaintext_paths() {
    use std::sync::Arc;

    use onyx_core::{CryptoFs, RealFs, VaultFs};
    use onyx_crypto::VaultKey;

    let (_dir, root) = temp_vault();
    let key = VaultKey::from_bytes([9; 32]);
    let crypto = Arc::new(CryptoFs::new(Arc::new(RealFs::new(&root)), key.clone()));

    let (sender, receiver) = crossbeam_channel::unbounded();
    let translator: onyx_core::PathTranslator = {
        let crypto = Arc::clone(&crypto);
        Arc::new(move |sealed| crypto.open_path(sealed))
    };
    let _watcher =
        onyx_core::VaultWatcher::spawn_translated(&root, test_config(), sender, Some(translator))
            .unwrap();
    std::thread::sleep(Duration::from_millis(300));

    // Write through the encrypted fs: on disk this is an opaque token file,
    // but the event must carry the plaintext vault path.
    crypto
        .write_atomic(&NotePath::new("Secret Note.md").unwrap(), b"content")
        .unwrap();

    expect_event(
        &receiver,
        "translated Created",
        |event| matches!(event, VaultEvent::Created(path) if path.as_str() == "Secret Note.md"),
    );

    // A foreign plaintext file dropped into the directory produces no event.
    std::fs::write(root.join("stray.txt"), "not ours").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    while let Ok(event) = receiver.try_recv() {
        assert!(
            event.path().map(|p| p.as_str()) != Some("stray.txt"),
            "foreign file must not produce a vault event: {event:?}"
        );
    }
}
