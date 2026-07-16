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

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

pub use db::Db;

pub struct ServerState {
    pub db: Db,
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
        .with_state(state)
}

pub fn state(data_dir: &Path) -> Result<Arc<ServerState>, db::DbError> {
    Ok(Arc::new(ServerState {
        db: Db::open(&data_dir.join("onyx-server.db"))?,
    }))
}

pub fn state_in_memory() -> Result<Arc<ServerState>, db::DbError> {
    Ok(Arc::new(ServerState {
        db: Db::open_in_memory()?,
    }))
}
