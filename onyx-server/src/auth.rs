//! Device authentication: Ed25519 challenge–response.
//!
//! Devices register a public key; to get a session they sign a fresh
//! server nonce. No passwords ever cross the wire — the server couldn't
//! verify one anyway (it has no user secrets, only device public keys).

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use data_encoding::HEXLOWER;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::ServerState;

fn random32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS randomness must be available");
    bytes
}

fn hex_decode<const N: usize>(text: &str) -> Option<[u8; N]> {
    HEXLOWER
        .decode(text.as_bytes())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
}

type AuthError = (StatusCode, String);

fn bad_request(message: &str) -> AuthError {
    (StatusCode::BAD_REQUEST, message.to_owned())
}

fn internal(error: impl std::fmt::Display) -> AuthError {
    (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
}

// ---------------------------------------------------------------------------
// POST /v1/devices — register a device public key
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RegisterRequest {
    /// Hex-encoded Ed25519 public key (32 bytes).
    pub public_key: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterResponse {
    pub device_id: String,
}

pub async fn register_device(
    State(state): State<Arc<ServerState>>,
    Json(request): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, AuthError> {
    let public: [u8; 32] =
        hex_decode(&request.public_key).ok_or_else(|| bad_request("invalid public key"))?;
    // Reject keys that aren't valid curve points up front.
    VerifyingKey::from_bytes(&public).map_err(|_| bad_request("invalid public key"))?;

    // Device id is derived from the key: stable, collision-free, unforgeable.
    let mut device_id = [0u8; 16];
    device_id.copy_from_slice(&blake3::hash(&public).as_bytes()[..16]);

    state
        .db
        .register_device(device_id, &public)
        .map_err(internal)?;
    Ok(Json(RegisterResponse {
        device_id: HEXLOWER.encode(&device_id),
    }))
}

// ---------------------------------------------------------------------------
// POST /v1/auth/challenge → POST /v1/auth/verify
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeRequest {
    pub device_id: String,
}

#[derive(Serialize)]
pub struct ChallengeResponse {
    pub challenge: String,
}

pub async fn challenge(
    State(state): State<Arc<ServerState>>,
    Json(request): Json<ChallengeRequest>,
) -> Result<Json<ChallengeResponse>, AuthError> {
    let device_id: [u8; 16] =
        hex_decode(&request.device_id).ok_or_else(|| bad_request("invalid device id"))?;
    if state
        .db
        .device_public_key(device_id)
        .map_err(internal)?
        .is_none()
    {
        return Err((StatusCode::NOT_FOUND, "unknown device".into()));
    }

    let nonce = random32();
    state
        .db
        .store_challenge(nonce, device_id)
        .map_err(internal)?;
    Ok(Json(ChallengeResponse {
        challenge: HEXLOWER.encode(&nonce),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyRequest {
    pub device_id: String,
    pub challenge: String,
    /// Hex Ed25519 signature over the raw 32-byte challenge.
    pub signature: String,
}

#[derive(Serialize)]
pub struct VerifyResponse {
    pub token: String,
}

pub async fn verify(
    State(state): State<Arc<ServerState>>,
    Json(request): Json<VerifyRequest>,
) -> Result<Json<VerifyResponse>, AuthError> {
    let device_id: [u8; 16] =
        hex_decode(&request.device_id).ok_or_else(|| bad_request("invalid device id"))?;
    let nonce: [u8; 32] =
        hex_decode(&request.challenge).ok_or_else(|| bad_request("invalid challenge"))?;
    let signature_bytes: [u8; 64] =
        hex_decode(&request.signature).ok_or_else(|| bad_request("invalid signature"))?;

    // Single-use, unexpired, bound to this device.
    if !state
        .db
        .consume_challenge(nonce, device_id)
        .map_err(internal)?
    {
        return Err((
            StatusCode::UNAUTHORIZED,
            "unknown or expired challenge".into(),
        ));
    }

    let public = state
        .db
        .device_public_key(device_id)
        .map_err(internal)?
        .ok_or((StatusCode::UNAUTHORIZED, "unknown device".into()))?;
    let key = VerifyingKey::from_bytes(&public)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid key".to_owned()))?;
    key.verify(&nonce, &Signature::from_bytes(&signature_bytes))
        .map_err(|_| (StatusCode::UNAUTHORIZED, "bad signature".to_owned()))?;

    let token = random32();
    state
        .db
        .create_session(*blake3::hash(&token).as_bytes(), device_id)
        .map_err(internal)?;
    Ok(Json(VerifyResponse {
        token: HEXLOWER.encode(&token),
    }))
}

// ---------------------------------------------------------------------------
// Bearer-token authentication for protected routes
// ---------------------------------------------------------------------------

/// Resolve `Authorization: Bearer <hex token>` to a device id.
pub fn authenticate(state: &ServerState, headers: &HeaderMap) -> Result<[u8; 16], AuthError> {
    let token_hex = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or((StatusCode::UNAUTHORIZED, "missing bearer token".to_owned()))?;
    let token: [u8; 32] =
        hex_decode(token_hex).ok_or((StatusCode::UNAUTHORIZED, "malformed token".to_owned()))?;
    state
        .db
        .session_device(*blake3::hash(&token).as_bytes())
        .map_err(internal)?
        .ok_or((StatusCode::UNAUTHORIZED, "unknown session".to_owned()))
}
