//! End-to-end: two devices sync an encrypted note through the real server
//! router (in-process, no sockets).
//!
//! This test is the zero-knowledge story in miniature: devices register
//! keys, authenticate by signature, push/pull opaque ciphertext, and the
//! CRDT merge happens client-side after decryption. At no point does the
//! server see a plaintext byte — asserted directly against its database
//! payloads.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use data_encoding::HEXLOWER;
use ed25519_dalek::{Signer, SigningKey};
use http_body_util::BodyExt;
use onyx_crypto::VaultKey;
use onyx_sync::SyncDoc;
use tower::ServiceExt;

struct Device {
    device_id: String,
    token: String,
}

async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: Vec<u8>,
    json: bool,
) -> (StatusCode, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if json {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body)).expect("request builds"))
        .await
        .expect("request succeeds");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body reads")
        .to_bytes()
        .to_vec();
    (status, bytes)
}

async fn json_request(
    app: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let (status, bytes) =
        request(app, method, uri, token, body.to_string().into_bytes(), true).await;
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

/// Register + authenticate a fresh device.
async fn enroll(app: &Router) -> Device {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).unwrap();
    let signing = SigningKey::from_bytes(&seed);
    let public_hex = HEXLOWER.encode(signing.verifying_key().as_bytes());

    let (status, registered) = json_request(
        app,
        "POST",
        "/v1/devices",
        None,
        serde_json::json!({ "public_key": public_hex }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let device_id = registered["deviceId"].as_str().unwrap().to_owned();

    let (status, challenged) = json_request(
        app,
        "POST",
        "/v1/auth/challenge",
        None,
        serde_json::json!({ "deviceId": device_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let challenge_hex = challenged["challenge"].as_str().unwrap().to_owned();
    let challenge = HEXLOWER.decode(challenge_hex.as_bytes()).unwrap();
    let signature = HEXLOWER.encode(&signing.sign(&challenge).to_bytes());

    let (status, verified) = json_request(
        app,
        "POST",
        "/v1/auth/verify",
        None,
        serde_json::json!({
            "deviceId": device_id,
            "challenge": challenge_hex,
            "signature": signature,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let token = verified["token"].as_str().unwrap().to_owned();

    Device { device_id, token }
}

#[tokio::test]
async fn two_devices_sync_an_encrypted_note() {
    let state = onyx_server::state_in_memory().unwrap();
    let app = onyx_server::app(state);

    // Both devices share the vault key (paired out of band) and vault id.
    let vault_key = VaultKey::from_bytes([42; 32]);
    let vault_id = HEXLOWER.encode(&[7u8; 16]);

    let alice = enroll(&app).await;
    let bob = enroll(&app).await;
    assert_ne!(alice.device_id, bob.device_id);

    for device in [&alice, &bob] {
        let (status, _) = json_request(
            &app,
            "POST",
            "/v1/vaults",
            Some(&device.token),
            serde_json::json!({ "vaultId": vault_id }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    // --- Alice creates a note and pushes the encrypted update. ---
    let alice_doc = SyncDoc::from_text(1, "# Meeting\nAlice's original notes\n").unwrap();
    let update = alice_doc.export_from(&[]).unwrap();
    let push = onyx_proto::PushOps {
        version: onyx_proto::PROTOCOL_VERSION,
        ops: vec![onyx_proto::EncOp::incremental(
            [1; 16],
            &update,
            onyx_crypto::encrypt(&vault_key, &update),
        )],
    };
    let (status, ack_bytes) = request(
        &app,
        "POST",
        &format!("/v1/vaults/{vault_id}/ops"),
        Some(&alice.token),
        onyx_proto::encode(&push).unwrap(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let ack: onyx_proto::PushAck = onyx_proto::decode(&ack_bytes).unwrap();
    assert_eq!(ack.head_seq, 1);

    // --- Bob pulls, decrypts, merges. ---
    let (status, batch_bytes) = request(
        &app,
        "GET",
        &format!("/v1/vaults/{vault_id}/ops?since=0"),
        Some(&bob.token),
        Vec::new(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let batch: onyx_proto::OpsBatch = onyx_proto::decode(&batch_bytes).unwrap();
    assert_eq!(batch.ops.len(), 1);

    let bob_doc = SyncDoc::new(2);
    for op in &batch.ops {
        let plaintext = onyx_crypto::decrypt(&vault_key, &op.ciphertext).unwrap();
        bob_doc.import(&plaintext).unwrap();
    }
    assert_eq!(bob_doc.text(), "# Meeting\nAlice's original notes\n");

    // --- Concurrent edits on both sides, exchanged through the server. ---
    bob_doc
        .set_text("# Meeting\nAlice's original notes\nBob's addendum\n")
        .unwrap();
    alice_doc
        .set_text("# Meeting (edited)\nAlice's original notes\n")
        .unwrap();

    let bob_update = bob_doc.export_from(&alice_doc.version()).unwrap();
    let push = onyx_proto::PushOps {
        version: onyx_proto::PROTOCOL_VERSION,
        ops: vec![onyx_proto::EncOp::incremental(
            [1; 16],
            &bob_update,
            onyx_crypto::encrypt(&vault_key, &bob_update),
        )],
    };
    let (status, _) = request(
        &app,
        "POST",
        &format!("/v1/vaults/{vault_id}/ops"),
        Some(&bob.token),
        onyx_proto::encode(&push).unwrap(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Alice pulls from her cursor (seq 1 was her own push).
    let (_, batch_bytes) = request(
        &app,
        "GET",
        &format!("/v1/vaults/{vault_id}/ops?since={}", ack.head_seq),
        Some(&alice.token),
        Vec::new(),
        false,
    )
    .await;
    let batch: onyx_proto::OpsBatch = onyx_proto::decode(&batch_bytes).unwrap();
    for op in &batch.ops {
        let plaintext = onyx_crypto::decrypt(&vault_key, &op.ciphertext).unwrap();
        alice_doc.import(&plaintext).unwrap();
    }

    // Merge keeps both sides' concurrent edits.
    let merged = alice_doc.text();
    assert!(merged.contains("(edited)"), "alice's edit lost: {merged}");
    assert!(
        merged.contains("Bob's addendum"),
        "bob's edit lost: {merged}"
    );
}

/// Register + join a vault, returning the authenticated device.
async fn member(app: &Router, vault_hex: &str) -> Device {
    let device = enroll(app).await;
    json_request(
        app,
        "POST",
        "/v1/vaults",
        Some(&device.token),
        serde_json::json!({ "vaultId": vault_hex }),
    )
    .await;
    device
}

#[tokio::test]
async fn chunked_blob_uploads_resume_and_serve_ranges() {
    let state = onyx_server::state_in_memory().unwrap();
    let app = onyx_server::app(state);
    let vault_hex = HEXLOWER.encode(&[21u8; 16]);
    let device = member(&app, &vault_hex).await;

    // Three unequal chunks; the hash addresses the whole ciphertext.
    let chunks = [vec![0xA1; 120], vec![0xB2; 80], vec![0xC3; 40]];
    let full: Vec<u8> = chunks.iter().flatten().copied().collect();
    let hash = blake3::hash(&full).to_hex().to_string();
    let size = full.len();
    let base = format!("/v1/vaults/{vault_hex}/blobs/{hash}");

    // Upload chunk 0, then SKIP chunk 1 (simulate a drop) and send chunk 2.
    for idx in [0usize, 2] {
        let (status, _) = request(
            &app,
            "PUT",
            &format!("{base}/chunks/{idx}?total=3&size={size}"),
            Some(&device.token),
            chunks[idx].clone(),
            false,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "partial chunk accepted, not complete"
        );
    }
    // Not yet downloadable.
    let (status, _) = request(&app, "GET", &base, Some(&device.token), Vec::new(), false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Resume: the status endpoint reports the gap so a client re-sends only 1.
    let (status, status_bytes) = request(
        &app,
        "GET",
        &format!("{base}/status"),
        Some(&device.token),
        Vec::new(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let blob_status: onyx_proto::BlobStatus = onyx_proto::decode(&status_bytes).unwrap();
    assert_eq!(blob_status.present, vec![0, 2]);
    assert!(!blob_status.complete);

    // Send the missing chunk 1 → completes and hash-verifies (201).
    let (status, _) = request(
        &app,
        "PUT",
        &format!("{base}/chunks/1?total=3&size={size}"),
        Some(&device.token),
        chunks[1].clone(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // HEAD reports the size for range planning.
    let (status, _) = request(&app, "HEAD", &base, Some(&device.token), Vec::new(), false).await;
    assert_eq!(status, StatusCode::OK);

    // Ranged download reassembles the exact bytes across chunk boundaries.
    let mut assembled = Vec::new();
    let mut offset = 0usize;
    while offset < size {
        let last = (offset + 90 - 1).min(size - 1);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(&base)
                    .header("authorization", format!("Bearer {}", device.token))
                    .header("range", format!("bytes={offset}-{last}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let piece = response.into_body().collect().await.unwrap().to_bytes();
        assembled.extend_from_slice(&piece);
        offset = last + 1;
    }
    assert_eq!(assembled, full, "ranged download must reassemble exactly");

    // An unsatisfiable range → 416.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&base)
                .header("authorization", format!("Bearer {}", device.token))
                .header("range", format!("bytes={}-{}", size + 10, size + 20))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
}

#[tokio::test]
async fn push_is_idempotent_over_the_wire() {
    let state = onyx_server::state_in_memory().unwrap();
    let app = onyx_server::app(Arc::clone(&state));
    let vault_bytes = [22u8; 16];
    let vault_hex = HEXLOWER.encode(&vault_bytes);
    let device = member(&app, &vault_hex).await;

    let doc = SyncDoc::from_text(1, "content").unwrap();
    let update = doc.export_from(&[]).unwrap();
    let op = onyx_proto::EncOp::incremental([1; 16], &update, vec![0xDE, 0xAD]);
    let push = onyx_proto::PushOps {
        version: onyx_proto::PROTOCOL_VERSION,
        ops: vec![op],
    };
    let body = onyx_proto::encode(&push).unwrap();

    // Push twice (a flaky link that dropped the first ack).
    for _ in 0..2 {
        let (status, ack_bytes) = request(
            &app,
            "POST",
            &format!("/v1/vaults/{vault_hex}/ops"),
            Some(&device.token),
            body.clone(),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let ack: onyx_proto::PushAck = onyx_proto::decode(&ack_bytes).unwrap();
        assert_eq!(ack.head_seq, 1, "resend must not advance the head");
    }
    // Exactly one row was stored.
    let (ops, head) = state.db.ops_since(vault_bytes, 0, 100).unwrap();
    assert_eq!(head, 1);
    assert_eq!(ops.len(), 1);
}

#[tokio::test]
async fn checkpoint_compaction_preserves_convergence_for_a_fresh_device() {
    let state = onyx_server::state_in_memory().unwrap();
    let app = onyx_server::app(Arc::clone(&state));
    let vault_bytes = [23u8; 16];
    let vault_hex = HEXLOWER.encode(&vault_bytes);
    let vault_key = VaultKey::from_bytes([55; 32]);
    let doc_id = [1u8; 16];
    let author = member(&app, &vault_hex).await;

    // Author many incremental edits — enough to cross the checkpoint
    // threshold — pushing each as its own op.
    let doc = SyncDoc::new(1);
    let mut since = Vec::new();
    for i in 0..300 {
        doc.set_text(&format!("line {i}\n")).unwrap();
        let update = doc.export_from(&since).unwrap();
        since = doc.version();
        let op = onyx_proto::EncOp::incremental(
            doc_id,
            &update,
            onyx_crypto::encrypt(&vault_key, &update),
        );
        let push = onyx_proto::PushOps {
            version: onyx_proto::PROTOCOL_VERSION,
            ops: vec![op],
        };
        let (status, _) = request(
            &app,
            "POST",
            &format!("/v1/vaults/{vault_hex}/ops"),
            Some(&author.token),
            onyx_proto::encode(&push).unwrap(),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    // A caught-up pull now carries a checkpoint hint for the heavy doc.
    let (_, batch_bytes) = request(
        &app,
        "GET",
        &format!("/v1/vaults/{vault_hex}/ops?since=0"),
        Some(&author.token),
        Vec::new(),
        false,
    )
    .await;
    let batch: onyx_proto::OpsBatch = onyx_proto::decode(&batch_bytes).unwrap();
    assert!(
        batch.checkpoint_hints.contains(&doc_id),
        "server should ask to checkpoint the heavy doc"
    );

    // The author answers with a full-state checkpoint.
    let full = doc.export_from(&[]).unwrap();
    let checkpoint =
        onyx_proto::EncOp::checkpoint(doc_id, &full, onyx_crypto::encrypt(&vault_key, &full));
    let push = onyx_proto::PushOps {
        version: onyx_proto::PROTOCOL_VERSION,
        ops: vec![checkpoint],
    };
    request(
        &app,
        "POST",
        &format!("/v1/vaults/{vault_hex}/ops"),
        Some(&author.token),
        onyx_proto::encode(&push).unwrap(),
        false,
    )
    .await;

    // The backlog is pruned: only the checkpoint op survives for this doc,
    // and the hint is gone.
    let (_, batch_bytes) = request(
        &app,
        "GET",
        &format!("/v1/vaults/{vault_hex}/ops?since=0"),
        Some(&author.token),
        Vec::new(),
        false,
    )
    .await;
    let batch: onyx_proto::OpsBatch = onyx_proto::decode(&batch_bytes).unwrap();
    assert_eq!(batch.ops.len(), 1, "300 ops compacted to one checkpoint");
    assert!(batch.checkpoint_hints.is_empty());

    // A BRAND-NEW device (cursor 0, missed every pruned op) still converges
    // from the checkpoint alone.
    let newcomer = member(&app, &vault_hex).await;
    let (_, batch_bytes) = request(
        &app,
        "GET",
        &format!("/v1/vaults/{vault_hex}/ops?since=0"),
        Some(&newcomer.token),
        Vec::new(),
        false,
    )
    .await;
    let batch: onyx_proto::OpsBatch = onyx_proto::decode(&batch_bytes).unwrap();
    let fresh = SyncDoc::new(2);
    for op in &batch.ops {
        let plaintext = onyx_crypto::decrypt(&vault_key, &op.ciphertext).unwrap();
        fresh.import(&plaintext).unwrap();
    }
    assert_eq!(
        fresh.text(),
        doc.text(),
        "fresh device must converge post-prune"
    );
}

#[tokio::test]
async fn auth_is_actually_enforced() {
    let state = onyx_server::state_in_memory().unwrap();
    let app = onyx_server::app(state);
    let vault_id = HEXLOWER.encode(&[9u8; 16]);

    // No token → 401.
    let (status, _) = request(
        &app,
        "GET",
        &format!("/v1/vaults/{vault_id}/ops?since=0"),
        None,
        Vec::new(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Garbage token → 401.
    let (status, _) = request(
        &app,
        "GET",
        &format!("/v1/vaults/{vault_id}/ops?since=0"),
        Some(&"ab".repeat(32)),
        Vec::new(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Authenticated but not a member → 403.
    let outsider = enroll(&app).await;
    let member = enroll(&app).await;
    let (status, _) = json_request(
        &app,
        "POST",
        "/v1/vaults",
        Some(&member.token),
        serde_json::json!({ "vaultId": vault_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        "GET",
        &format!("/v1/vaults/{vault_id}/ops?since=0"),
        Some(&outsider.token),
        Vec::new(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Wrong signature on a valid challenge → 401 and no session.
    let (_, registered) = json_request(
        &app,
        "POST",
        "/v1/devices",
        None,
        serde_json::json!({ "public_key": HEXLOWER.encode(
            SigningKey::from_bytes(&[1; 32]).verifying_key().as_bytes()
        )}),
    )
    .await;
    let device_id = registered["deviceId"].as_str().unwrap().to_owned();
    let (_, challenged) = json_request(
        &app,
        "POST",
        "/v1/auth/challenge",
        None,
        serde_json::json!({ "deviceId": device_id }),
    )
    .await;
    let challenge_hex = challenged["challenge"].as_str().unwrap();
    let challenge = HEXLOWER.decode(challenge_hex.as_bytes()).unwrap();
    // Signed by the WRONG key.
    let forged = HEXLOWER.encode(&SigningKey::from_bytes(&[2; 32]).sign(&challenge).to_bytes());
    let (status, _) = json_request(
        &app,
        "POST",
        "/v1/auth/verify",
        None,
        serde_json::json!({
            "deviceId": device_id,
            "challenge": challenge_hex,
            "signature": forged,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Challenge is single-use: replaying the consumed challenge fails even
    // with a correct signature.
    let good = HEXLOWER.encode(&SigningKey::from_bytes(&[1; 32]).sign(&challenge).to_bytes());
    let (status, _) = json_request(
        &app,
        "POST",
        "/v1/auth/verify",
        None,
        serde_json::json!({
            "deviceId": device_id,
            "challenge": challenge_hex,
            "signature": good,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn server_never_stores_plaintext() {
    let state = onyx_server::state_in_memory().unwrap();
    let app = onyx_server::app(Arc::clone(&state));
    let vault_key = VaultKey::from_bytes([3; 32]);
    let vault_id_bytes = [5u8; 16];
    let vault_id = HEXLOWER.encode(&vault_id_bytes);

    let device = enroll(&app).await;
    json_request(
        &app,
        "POST",
        "/v1/vaults",
        Some(&device.token),
        serde_json::json!({ "vaultId": vault_id }),
    )
    .await;

    let secret = "EXTREMELY-SECRET-CONTENT-marker";
    let doc = SyncDoc::from_text(1, secret).unwrap();
    let update = doc.export_from(&[]).unwrap();
    let push = onyx_proto::PushOps {
        version: onyx_proto::PROTOCOL_VERSION,
        ops: vec![onyx_proto::EncOp::incremental(
            [1; 16],
            &update,
            onyx_crypto::encrypt(&vault_key, &update),
        )],
    };
    request(
        &app,
        "POST",
        &format!("/v1/vaults/{vault_id}/ops"),
        Some(&device.token),
        onyx_proto::encode(&push).unwrap(),
        false,
    )
    .await;

    // Inspect what the server actually holds: the op ciphertext must not
    // contain the plaintext marker.
    let (ops, head) = state.db.ops_since(vault_id_bytes, 0, 100).unwrap();
    assert_eq!(head, 1);
    let marker = secret.as_bytes();
    for op in ops {
        assert!(
            !op.ciphertext
                .windows(marker.len())
                .any(|window| window == marker),
            "plaintext leaked into server storage"
        );
    }
}
