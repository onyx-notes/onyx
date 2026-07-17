//! Full-stack sync E2E: two desktop engines, a real onyx-server on a real
//! TCP socket, the real blocking HTTP client, real markdown files on disk.
//!
//! This is the plan's headline promise exercised end to end: edit the same
//! note on two devices, sync through a server that only ever sees
//! ciphertext, and lose nothing.

use std::net::SocketAddr;

use onyx_crypto::VaultKey;
use onyx_desktop_lib::{Engine, SyncState};
use parking_lot::Mutex;

use onyx_desktop_lib::sync::{DeviceIdentity, SyncClient, sync_cycle};

/// Boot a real server on an ephemeral port; returns its base URL.
fn start_server() -> String {
    let state = onyx_server::state_in_memory().unwrap();
    let app = onyx_server::app(state);
    let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    let addr = addr_rx.recv().unwrap();
    format!("http://{addr}")
}

struct TestDevice {
    _vault_dir: tempfile::TempDir,
    device_dir: tempfile::TempDir,
    engine: Mutex<Option<Engine>>,
    client: SyncClient,
}

fn device(server: &str, seed_files: &[(&str, &str)]) -> TestDevice {
    let vault_dir = tempfile::tempdir().unwrap();
    let device_dir = tempfile::tempdir().unwrap();
    for (path, content) in seed_files {
        std::fs::write(vault_dir.path().join(path), content).unwrap();
    }

    let identity = DeviceIdentity::load_or_create(&device_dir.path().join("device.key")).unwrap();
    let peer = identity.peer();
    let mut engine = Engine::open(vault_dir.path()).unwrap();
    let store = onyx_sync::SyncStore::open(&vault_dir.path().join(".onyx/sync.db")).unwrap();
    engine.enable_sync(SyncState::new(store, VaultKey::from_bytes([11; 32]), peer));

    let client = SyncClient::new(server, identity).unwrap();
    TestDevice {
        _vault_dir: vault_dir,
        device_dir,
        engine: Mutex::new(Some(engine)),
        client,
    }
}

impl TestDevice {
    fn vault_path(&self) -> &std::path::Path {
        self._vault_dir.path()
    }

    fn cycle(&mut self, vault_id: [u8; 16]) -> Vec<String> {
        sync_cycle(&self.engine, &mut self.client, vault_id).unwrap()
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.vault_path().join(name)).unwrap()
    }
}

#[test]
fn two_devices_full_stack_sync() {
    let server = start_server();
    let vault_id = [7u8; 16];

    let mut alice = device(&server, &[("plan.md", "# Plan\nAlice's first draft\n")]);
    let mut bob = device(&server, &[]);
    assert_ne!(
        alice.device_dir.path().join("device.key"),
        bob.device_dir.path().join("device.key")
    );

    alice.client.join(vault_id).unwrap();
    bob.client.join(vault_id).unwrap();

    // Alice pushes; Bob's cycle materializes the file on his disk.
    alice.cycle(vault_id);
    let changed = bob.cycle(vault_id);
    assert_eq!(changed, vec!["plan.md".to_owned()]);
    assert_eq!(bob.read("plan.md"), "# Plan\nAlice's first draft\n");

    // Concurrent edits on both devices' real files.
    std::fs::write(
        alice.vault_path().join("plan.md"),
        "# Plan (Alice's title)\nAlice's first draft\n",
    )
    .unwrap();
    std::fs::write(
        bob.vault_path().join("plan.md"),
        "# Plan\nAlice's first draft\nBob's step two\n",
    )
    .unwrap();
    // The engines discover the external edits via reconcile (the watcher
    // isn't running in this headless test).
    {
        let mut guard = alice.engine.lock();
        let engine = guard.as_mut().unwrap();
        engine
            .apply_event(&onyx_core::VaultEvent::BulkChange)
            .unwrap();
    }
    {
        let mut guard = bob.engine.lock();
        let engine = guard.as_mut().unwrap();
        engine
            .apply_event(&onyx_core::VaultEvent::BulkChange)
            .unwrap();
    }

    // Two rounds of cycles propagate + merge everywhere.
    alice.cycle(vault_id);
    bob.cycle(vault_id);
    alice.cycle(vault_id);
    bob.cycle(vault_id);

    let text_alice = alice.read("plan.md");
    let text_bob = bob.read("plan.md");
    assert_eq!(text_alice, text_bob, "devices must converge");
    assert!(text_alice.contains("(Alice's title)"), "{text_alice}");
    assert!(text_alice.contains("Bob's step two"), "{text_alice}");

    // A new note born on Bob's device arrives on Alice's disk.
    std::fs::write(bob.vault_path().join("new-from-bob.md"), "fresh note\n").unwrap();
    {
        let mut guard = bob.engine.lock();
        guard
            .as_mut()
            .unwrap()
            .apply_event(&onyx_core::VaultEvent::BulkChange)
            .unwrap();
    }
    bob.cycle(vault_id);
    let changed = alice.cycle(vault_id);
    assert_eq!(changed, vec!["new-from-bob.md".to_owned()]);
    assert_eq!(alice.read("new-from-bob.md"), "fresh note\n");

    // And it's fully live on Alice: indexed and searchable.
    {
        let mut guard = alice.engine.lock();
        let engine = guard.as_mut().unwrap();
        engine.commit_search_if_dirty().unwrap();
        assert_eq!(engine.index().note_count().unwrap(), 2);
        assert_eq!(engine.search("fresh", 5).unwrap().len(), 1);
    }

    // Deletes propagate: Bob deletes his note, it disappears from Alice.
    {
        let mut guard = bob.engine.lock();
        guard
            .as_mut()
            .unwrap()
            .delete_note(&onyx_core::NotePath::new("new-from-bob.md").unwrap())
            .unwrap();
    }
    bob.cycle(vault_id);
    let changed = alice.cycle(vault_id);
    assert!(changed.contains(&"new-from-bob.md".to_owned()));
    assert!(!alice.vault_path().join("new-from-bob.md").exists());
    {
        let mut guard = alice.engine.lock();
        let engine = guard.as_mut().unwrap();
        assert_eq!(engine.index().note_count().unwrap(), 1);
    }
}

#[test]
fn live_push_wakes_subscribers_immediately() {
    let server = start_server();
    let vault_id = [3u8; 16];

    let mut alice = device(&server, &[("note.md", "hello")]);
    let mut bob = device(&server, &[]);
    alice.client.join(vault_id).unwrap();
    bob.client.join(vault_id).unwrap();

    // Bob subscribes to live push.
    let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (wake_tx, wake_rx) = crossbeam_channel::bounded::<()>(1);
    let token = bob.client.ensure_auth().unwrap();
    onyx_desktop_lib::sync::spawn_ws_waker(
        bob.client.base_url(),
        vault_id,
        token,
        wake_tx,
        std::sync::Arc::clone(&alive),
    );
    // Give the WS a moment to connect.
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Alice pushes; Bob's waker must fire well under a second.
    alice.cycle(vault_id);
    let woken = wake_rx.recv_timeout(std::time::Duration::from_secs(3));
    assert!(woken.is_ok(), "live push did not wake the subscriber");

    // The nudge leads to a cycle that materializes the note.
    let changed = bob.cycle(vault_id);
    assert_eq!(changed, vec!["note.md".to_owned()]);

    // Clean shutdown: the waker exits promptly once alive clears.
    alive.store(false, std::sync::atomic::Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(100));
}

#[test]
fn attachments_sync_between_devices() {
    let server = start_server();
    let vault_id = [8u8; 16];

    let mut alice = device(&server, &[]);
    let mut bob = device(&server, &[]);
    alice.client.join(vault_id).unwrap();
    bob.client.join(vault_id).unwrap();

    // A binary attachment (not valid UTF-8) travels A → B intact.
    let png: Vec<u8> = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0xFF, 0x00]
        .iter()
        .copied()
        .chain((0..2000).map(|byte| (byte % 251) as u8))
        .collect();
    std::fs::create_dir_all(alice.vault_path().join("assets")).unwrap();
    std::fs::write(alice.vault_path().join("assets/pic.png"), &png).unwrap();
    reconcile(&alice);

    alice.cycle(vault_id);
    let changed = bob.cycle(vault_id);
    assert!(changed.contains(&"assets/pic.png".to_owned()));
    assert_eq!(
        std::fs::read(bob.vault_path().join("assets/pic.png")).unwrap(),
        png
    );

    // Modification propagates (idempotent for Alice).
    let png_v2: Vec<u8> = png.iter().map(|byte| byte.wrapping_add(1)).collect();
    std::fs::write(bob.vault_path().join("assets/pic.png"), &png_v2).unwrap();
    reconcile(&bob);
    bob.cycle(vault_id);
    let changed = alice.cycle(vault_id);
    assert!(changed.contains(&"assets/pic.png".to_owned()));
    assert_eq!(
        std::fs::read(alice.vault_path().join("assets/pic.png")).unwrap(),
        png_v2
    );

    // Concurrent binary modification: keep-both, never silent loss.
    std::fs::write(alice.vault_path().join("assets/pic.png"), b"alice version").unwrap();
    std::fs::write(bob.vault_path().join("assets/pic.png"), b"bob version").unwrap();
    reconcile(&alice);
    reconcile(&bob);
    // Several rounds: uploads, LWW merge, conflict copy creation on the
    // losing side, and propagation of the conflict copy back out.
    for _ in 0..4 {
        alice.cycle(vault_id);
        bob.cycle(vault_id);
    }
    // Both versions survive on BOTH devices (main file + conflict copy).
    for device in [&alice, &bob] {
        let main = std::fs::read(device.vault_path().join("assets/pic.png")).unwrap();
        let conflict = std::fs::read(device.vault_path().join("assets/pic (conflict).png"))
            .expect("conflict copy must exist everywhere after propagation");
        let kept = [main, conflict];
        assert!(kept.contains(&b"alice version".to_vec()), "alice's lost");
        assert!(kept.contains(&b"bob version".to_vec()), "bob's lost");
    }

    // Deletion propagates.
    std::fs::remove_file(alice.vault_path().join("assets/pic.png")).unwrap();
    reconcile(&alice);
    alice.cycle(vault_id);
    bob.cycle(vault_id);
    assert!(!bob.vault_path().join("assets/pic.png").exists());
}

/// Let the engine notice external file changes (headless: no watcher).
fn reconcile(device: &TestDevice) {
    let mut guard = device.engine.lock();
    guard
        .as_mut()
        .unwrap()
        .apply_event(&onyx_core::VaultEvent::BulkChange)
        .unwrap();
}
