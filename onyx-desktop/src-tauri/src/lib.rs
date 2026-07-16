//! Onyx desktop: thin Tauri adapter over the engine.
//!
//! Three IPC lanes, chosen per payload (see the architecture plan):
//! commands (JSON control plane), `onyx://` protocol (bulk bytes), and
//! events (tiny change notifications pushed Rust → JS).

mod commands;
mod engine;
mod protocol;
mod state;

pub use engine::{Engine, EngineError};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,onyx_core=debug".into()),
        )
        .init();

    tauri::Builder::default()
        .manage(state::AppState::default())
        .register_asynchronous_uri_scheme_protocol("onyx", protocol::handle)
        .invoke_handler(tauri::generate_handler![
            commands::open_vault,
            commands::list_notes,
            commands::read_note,
            commands::write_note,
            commands::delete_note,
            commands::rename_note,
            commands::search_notes,
            commands::quick_open,
            commands::backlinks,
            commands::resolve_target,
            commands::vault_tags,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Onyx");
}
