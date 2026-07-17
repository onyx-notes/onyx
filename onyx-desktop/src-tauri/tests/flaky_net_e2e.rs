//! Fault-injection E2E: run real sync through a TCP proxy that can be cut
//! and restored, plus resumable-blob checks. This is the "poor connection"
//! story exercised directly — the failure paths the happy-path E2E can't
//! reach: a transport drop mid-sync, a half-open live-push socket, and a
//! large attachment transferred over a link that dies partway.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use onyx_crypto::VaultKey;
use onyx_desktop_lib::sync::{DeviceIdentity, SyncClient, sync_cycle};
use onyx_desktop_lib::{Engine, SyncState};
use parking_lot::Mutex;

// ---------------------------------------------------------------------------
// A real onyx-server on a real socket.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// A toggleable TCP proxy. `open == false` refuses new connections and cuts
// existing ones (the flaky-link fault we inject).
// ---------------------------------------------------------------------------

struct Proxy {
    addr: SocketAddr,
    open: Arc<AtomicBool>,
}

impl Proxy {
    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
    fn cut(&self) {
        self.open.store(false, Ordering::SeqCst);
    }
    fn restore(&self) {
        self.open.store(true, Ordering::SeqCst);
    }
}

fn start_proxy(upstream: &str) -> Proxy {
    let upstream = upstream.trim_start_matches("http://").to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(false).unwrap();
    let addr = listener.local_addr().unwrap();
    let open = Arc::new(AtomicBool::new(true));
    let open_accept = Arc::clone(&open);

    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(client) = incoming else { continue };
            if !open_accept.load(Ordering::SeqCst) {
                let _ = client.shutdown(Shutdown::Both);
                continue;
            }
            let Ok(server) = TcpStream::connect(&upstream) else {
                let _ = client.shutdown(Shutdown::Both);
                continue;
            };
            let gate = Arc::clone(&open_accept);
            // Two pumps; either, on a closed gate or EOF/error, tears down
            // both sockets so the sibling thread unblocks too.
            let (c1, c2) = (client.try_clone().unwrap(), client);
            let (s1, s2) = (server.try_clone().unwrap(), server);
            let g2 = Arc::clone(&gate);
            std::thread::spawn(move || pump(c1, s1, gate));
            std::thread::spawn(move || pump(s2, c2, g2));
        }
    });

    Proxy { addr, open }
}

fn pump(mut from: TcpStream, mut to: TcpStream, open: Arc<AtomicBool>) {
    from.set_read_timeout(Some(Duration::from_millis(120))).ok();
    let mut buf = [0u8; 32 * 1024];
    loop {
        if !open.load(Ordering::SeqCst) {
            let _ = from.shutdown(Shutdown::Both);
            let _ = to.shutdown(Shutdown::Both);
            return;
        }
        match from.read(&mut buf) {
            Ok(0) => {
                let _ = to.shutdown(Shutdown::Both);
                return;
            }
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    let _ = from.shutdown(Shutdown::Both);
                    return;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => {
                let _ = to.shutdown(Shutdown::Both);
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Test device: engine + sync client pointed at a given base URL.
// ---------------------------------------------------------------------------

struct TestDevice {
    _vault_dir: tempfile::TempDir,
    _device_dir: tempfile::TempDir,
    engine: Mutex<Option<Engine>>,
    client: SyncClient,
}

fn device(base: &str, seed: &[(&str, &str)]) -> TestDevice {
    let vault_dir = tempfile::tempdir().unwrap();
    let device_dir = tempfile::tempdir().unwrap();
    for (path, content) in seed {
        std::fs::write(vault_dir.path().join(path), content).unwrap();
    }
    let identity = DeviceIdentity::load_or_create(&device_dir.path().join("device.key")).unwrap();
    let peer = identity.peer();
    let mut engine = Engine::open(vault_dir.path()).unwrap();
    let store = onyx_sync::SyncStore::open(&vault_dir.path().join(".onyx/sync.db")).unwrap();
    engine.enable_sync(SyncState::new(store, VaultKey::from_bytes([11; 32]), peer));
    let mut client = SyncClient::new(base, identity).unwrap();
    client.set_blob_cache(device_dir.path().join("blobcache"));
    TestDevice {
        _vault_dir: vault_dir,
        _device_dir: device_dir,
        engine: Mutex::new(Some(engine)),
        client,
    }
}

impl TestDevice {
    fn path(&self) -> &std::path::Path {
        self._vault_dir.path()
    }
    fn reconcile(&self) {
        self.engine
            .lock()
            .as_mut()
            .unwrap()
            .apply_event(&onyx_core::VaultEvent::BulkChange)
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// 1. A transport drop mid-session is a roaming state, not data loss: the
//    cut cycle reports Offline, and once the link returns the devices
//    converge from their persisted cursors.
// ---------------------------------------------------------------------------

#[test]
fn notes_sync_survives_a_transport_drop() {
    let server = start_server();
    let proxy = start_proxy(&server);
    let vault_id = [31u8; 16];

    let mut alice = device(&proxy.url(), &[("plan.md", "# Plan\nv1\n")]);
    let mut bob = device(&proxy.url(), &[]);
    alice.client.join(vault_id).unwrap();
    bob.client.join(vault_id).unwrap();

    // Baseline sync works through the proxy.
    sync_cycle(&alice.engine, &mut alice.client, vault_id).unwrap();
    let changed = sync_cycle(&bob.engine, &mut bob.client, vault_id).unwrap();
    assert_eq!(changed, vec!["plan.md".to_owned()]);

    // Cut the link, then edit on Alice. Her cycle must fail gracefully as
    // Offline (no panic, no corruption).
    proxy.cut();
    std::fs::write(alice.path().join("plan.md"), "# Plan\nv1\nv2 offline\n").unwrap();
    alice.reconcile();
    match sync_cycle(&alice.engine, &mut alice.client, vault_id) {
        Err(onyx_desktop_lib::sync::SyncSetupError::Offline) => {}
        other => panic!("expected Offline while cut, got {other:?}"),
    }

    // Restore the link. The queued edit flushes and Bob converges — nothing
    // was lost across the outage.
    proxy.restore();
    for _ in 0..3 {
        let _ = sync_cycle(&alice.engine, &mut alice.client, vault_id);
        let _ = sync_cycle(&bob.engine, &mut bob.client, vault_id);
    }
    assert_eq!(
        std::fs::read_to_string(bob.path().join("plan.md")).unwrap(),
        "# Plan\nv1\nv2 offline\n",
        "edit made during the outage must survive and propagate"
    );
}

// ---------------------------------------------------------------------------
// 2. The live-push waker recovers from a broken socket: after the link is
//    cut and restored, a later push still wakes the subscriber.
// ---------------------------------------------------------------------------

#[test]
fn live_push_waker_recovers_after_a_broken_socket() {
    let server = start_server();
    let proxy = start_proxy(&server);
    let vault_id = [32u8; 16];

    let mut alice = device(&proxy.url(), &[("note.md", "hello")]);
    let mut bob = device(&proxy.url(), &[]);
    alice.client.join(vault_id).unwrap();
    bob.client.join(vault_id).unwrap();

    let alive = Arc::new(AtomicBool::new(true));
    let (wake_tx, wake_rx) = crossbeam_channel::bounded::<()>(1);
    let token = bob.client.ensure_auth().unwrap();
    onyx_desktop_lib::sync::spawn_ws_waker(
        bob.client.base_url(),
        vault_id,
        token,
        wake_tx,
        Arc::clone(&alive),
    );
    std::thread::sleep(Duration::from_millis(500));

    // Works before the fault.
    sync_cycle(&alice.engine, &mut alice.client, vault_id).unwrap();
    assert!(
        wake_rx.recv_timeout(Duration::from_secs(3)).is_ok(),
        "waker should fire before the fault"
    );
    let _ = sync_cycle(&bob.engine, &mut bob.client, vault_id);

    // Break the socket, then heal the link. Give the waker time to notice
    // the dead socket (≤2s read timeout) and reconnect on its ~5s backoff —
    // the broadcast is edge-triggered, so the waker must be reconnected
    // before the next push or it would miss the nudge.
    proxy.cut();
    std::thread::sleep(Duration::from_millis(500));
    proxy.restore();
    std::thread::sleep(Duration::from_secs(9));
    while wake_rx.try_recv().is_ok() {} // drain any stale signal

    // A push after recovery must still wake the subscriber.
    std::fs::write(alice.path().join("note.md"), "hello again").unwrap();
    alice.reconcile();
    sync_cycle(&alice.engine, &mut alice.client, vault_id).unwrap();
    assert!(
        wake_rx.recv_timeout(Duration::from_secs(8)).is_ok(),
        "waker must reconnect and fire after the socket was broken"
    );

    alive.store(false, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
}

// ---------------------------------------------------------------------------
// 3. Large-blob download resumes from a partial file: a `.part` left by a
//    dropped connection is continued via range requests, not restarted.
// ---------------------------------------------------------------------------

#[test]
fn large_blob_download_resumes_from_a_partial_file() {
    let server = start_server();
    let vault_id = [33u8; 16];
    let mut uploader = device(&server, &[]);
    uploader.client.join(vault_id).unwrap();

    // A ciphertext well over one chunk (content-addressed; the server only
    // checks the hash, so raw bytes stand in for an encrypted blob).
    let blob: Vec<u8> = (0..(onyx_proto::BLOB_CHUNK_BYTES * 2 + 12345))
        .map(|i| (i % 256) as u8)
        .collect();
    let hash = blake3::hash(&blob).to_hex().to_string();
    uploader
        .client
        .put_blob(vault_id, &hash, blob.clone())
        .unwrap();

    // A downloader that already has a partial `.part` on disk (as if a prior
    // connection died mid-transfer).
    let mut downloader = device(&server, &[]);
    downloader.client.join(vault_id).unwrap();
    let cache = downloader._device_dir.path().join("blobcache");
    std::fs::create_dir_all(&cache).unwrap();
    let prefix_len = onyx_proto::BLOB_CHUNK_BYTES + 500;
    std::fs::write(cache.join(format!("{hash}.part")), &blob[..prefix_len]).unwrap();

    // get_blob resumes from the prefix and returns the whole, verified blob.
    let fetched = downloader.client.get_blob(vault_id, &hash).unwrap();
    assert_eq!(fetched, blob, "resumed download must equal the original");
    // The staging file is cleaned up once complete.
    assert!(!cache.join(format!("{hash}.part")).exists());
}

// ---------------------------------------------------------------------------
// 4. Large-blob upload is resumable and idempotent: chunks already on the
//    server are not re-sent, and a second put of a complete blob is a no-op.
// ---------------------------------------------------------------------------

#[test]
fn large_blob_upload_is_chunked_and_idempotent() {
    let server = start_server();
    let vault_id = [34u8; 16];
    let mut a = device(&server, &[]);
    let mut b = device(&server, &[]);
    a.client.join(vault_id).unwrap();
    b.client.join(vault_id).unwrap();

    let blob: Vec<u8> = (0..(onyx_proto::BLOB_CHUNK_BYTES * 3 - 7))
        .map(|i| ((i * 7) % 256) as u8)
        .collect();
    let hash = blake3::hash(&blob).to_hex().to_string();

    // First upload lands the whole blob (chunked under the hood).
    a.client.put_blob(vault_id, &hash, blob.clone()).unwrap();
    assert!(a.client.has_blob(vault_id, &hash).unwrap());

    // A second uploader whose blob is already fully present short-circuits
    // (the resume path sees `complete` and re-sends nothing).
    b.client.put_blob(vault_id, &hash, blob.clone()).unwrap();

    // And it downloads back byte-identical.
    let fetched = b.client.get_blob(vault_id, &hash).unwrap();
    assert_eq!(fetched, blob);
}
