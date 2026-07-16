//! Integration tests with the REAL filesystem watcher on a real tempdir.
//!
//! These are timing-sensitive by nature; they use generous timeouts and
//! assert on eventual delivery, not exact timing.

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
fn create_modify_remove_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, receiver) = crossbeam_channel::unbounded();
    let _watcher = VaultWatcher::spawn(dir.path(), test_config(), sender).unwrap();
    // Give the watcher backend a moment to arm (macOS FSEvents needs it).
    std::thread::sleep(Duration::from_millis(300));

    let note = NotePath::new("hello.md").unwrap();

    std::fs::write(dir.path().join("hello.md"), "# hi").unwrap();
    expect_event(
        &receiver,
        "Created",
        |event| matches!(event, VaultEvent::Created(path) if *path == note),
    );

    std::fs::write(dir.path().join("hello.md"), "# hi edited").unwrap();
    expect_event(
        &receiver,
        "Modified",
        |event| matches!(event, VaultEvent::Modified(path) if *path == note),
    );

    std::fs::remove_file(dir.path().join("hello.md")).unwrap();
    expect_event(
        &receiver,
        "Removed",
        |event| matches!(event, VaultEvent::Removed(path) if *path == note),
    );
}

#[test]
fn hidden_directories_are_invisible() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".obsidian")).unwrap();
    let (sender, receiver) = crossbeam_channel::unbounded();
    let _watcher = VaultWatcher::spawn(dir.path(), test_config(), sender).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    std::fs::write(dir.path().join(".obsidian/app.json"), "{}").unwrap();
    std::fs::write(dir.path().join("real.md"), "content").unwrap();

    // The visible file arrives; the hidden one must never appear before it.
    let event = expect_event(&receiver, "some event", |_| true);
    assert!(
        matches!(&event, VaultEvent::Created(path) if path.as_str() == "real.md"),
        "expected real.md first, got {event:?}"
    );
}

#[test]
fn file_storm_collapses_into_bulk_change() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, receiver) = crossbeam_channel::unbounded();
    let _watcher = VaultWatcher::spawn(dir.path(), test_config(), sender).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    // Simulate a git checkout: many files appearing at once.
    for index in 0..60 {
        std::fs::write(dir.path().join(format!("bulk-{index}.md")), "x").unwrap();
    }

    expect_event(&receiver, "BulkChange", |event| {
        matches!(event, VaultEvent::BulkChange)
    });

    // After the storm settles, normal per-file behavior resumes.
    std::thread::sleep(Duration::from_millis(700));
    std::fs::write(dir.path().join("after-storm.md"), "y").unwrap();
    expect_event(
        &receiver,
        "post-storm Created",
        |event| matches!(event, VaultEvent::Created(path) if path.as_str() == "after-storm.md"),
    );
}

#[test]
fn rename_produces_remove_and_create() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("old.md"), "content").unwrap();
    let (sender, receiver) = crossbeam_channel::unbounded();
    let _watcher = VaultWatcher::spawn(dir.path(), test_config(), sender).unwrap();
    std::thread::sleep(Duration::from_millis(300));

    std::fs::rename(dir.path().join("old.md"), dir.path().join("new.md")).unwrap();

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
    let dir = tempfile::tempdir().unwrap();
    let (sender, _receiver) = crossbeam_channel::unbounded();
    let watcher = VaultWatcher::spawn(dir.path(), test_config(), sender).unwrap();
    drop(watcher); // must not hang or panic
}
