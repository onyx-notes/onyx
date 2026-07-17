//! The Onyx sync server: a zero-knowledge encrypted oplog.
//!
//! What the server does: authenticate devices (Ed25519 challenge–response,
//! no passwords ever cross the wire), store opaque encrypted CRDT ops per
//! vault with a monotonically increasing delivery cursor, and serve them
//! back. That's the whole job — search, graph, merge, and rendering all
//! happen on clients, because the server cannot read a byte of note
//! content or a single filename.
//!
//! The dependency graph proves it: this crate does not link the vault
//! engine or the markdown parser (CI-enforced via cargo-deny).

mod auth;
mod db;
mod routes;
mod ws;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};
use parking_lot::Mutex;

pub use db::Db;

pub struct ServerState {
    pub db: Db,
    /// Per-vault live-push hubs: subscribers get the new head seq on every
    /// append and pull the ops over HTTP (tiny frames, one delivery path).
    hubs: Mutex<HashMap<[u8; 16], tokio::sync::broadcast::Sender<u64>>>,
}

impl ServerState {
    fn new(db: Db) -> Self {
        Self {
            db,
            hubs: Mutex::new(HashMap::new()),
        }
    }

    /// The broadcast hub for a vault (created on first use).
    pub(crate) fn hub(&self, vault: [u8; 16]) -> tokio::sync::broadcast::Sender<u64> {
        self.hubs
            .lock()
            .entry(vault)
            .or_insert_with(|| tokio::sync::broadcast::channel(64).0)
            .clone()
    }
}

/// Build the application router. Separated from `main` so tests drive it
/// in-process with no sockets.
pub fn app(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/v1/health", get(routes::health))
        .route("/v1/devices", post(auth::register_device))
        .route("/v1/auth/challenge", post(auth::challenge))
        .route("/v1/auth/verify", post(auth::verify))
        .route("/v1/vaults", post(routes::join_vault))
        .route("/v1/vaults/{vault}/ops", post(routes::push_ops))
        .route("/v1/vaults/{vault}/ops", get(routes::pull_ops))
        .route("/v1/vaults/{vault}/ws", get(ws::live))
        .route(
            "/v1/enroll/{code}",
            post(routes::enroll_create).get(routes::enroll_request),
        )
        .route(
            "/v1/enroll/{code}/response",
            post(routes::enroll_respond).get(routes::enroll_claim),
        )
        .route(
            "/v1/shares/{id}",
            axum::routing::put(routes::put_share)
                .get(routes::get_share)
                .delete(routes::delete_share),
        )
        .route("/s/{id}", get(routes::share_viewer))
        .route(
            "/v1/vaults/{vault}/blobs/{hash}",
            axum::routing::put(routes::put_blob)
                .head(routes::head_blob)
                .get(routes::get_blob),
        )
        .with_state(state)
}

pub fn state(data_dir: &Path) -> Result<Arc<ServerState>, db::DbError> {
    Ok(Arc::new(ServerState::new(Db::open(
        &data_dir.join("onyx-server.db"),
    )?)))
}

pub fn state_in_memory() -> Result<Arc<ServerState>, db::DbError> {
    Ok(Arc::new(ServerState::new(Db::open_in_memory()?)))
}
