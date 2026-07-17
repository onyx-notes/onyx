//! Onyx desktop: thin Tauri adapter over the engine.
//!
//! Three IPC lanes, chosen per payload (see the architecture plan):
//! commands (JSON control plane), `onyx://` protocol (bulk bytes), and
//! events (tiny change notifications pushed Rust → JS).

pub mod agent;
mod ai;
pub mod backup;
mod clipper;
mod commands;
mod engine;
mod protocol;
pub mod publish;
pub mod rag;
mod secrets;
mod settings;
mod state;
pub mod sync;

pub use engine::{Engine, EngineError, SyncState};

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
            commands::vault_status,
            commands::create_encrypted_vault,
            commands::lock_vault,
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
            commands::get_settings,
            commands::update_settings,
            commands::import_obsidian_settings,
            commands::daily_note,
            commands::render_note,
            commands::note_headings,
            commands::sync_enable,
            commands::sync_join,
            commands::sync_status,
            commands::get_backup_config,
            commands::set_backup_config,
            commands::backup_now,
            commands::list_backup_snapshots,
            commands::list_plugins,
            commands::set_plugin_enabled,
            commands::vault_stats,
            commands::graph_payload,
            commands::get_ai_config,
            commands::set_ai_config,
            commands::ai_chat,
            commands::ai_request_log,
            commands::enroll_start,
            commands::enroll_wait,
            commands::enroll_confirm,
            commands::enroll_cancel,
            commands::enroll_approve_device,
            commands::rag_reindex,
            commands::rag_status,
            commands::agent_run,
            commands::agent_apply,
            commands::plugin_registry,
            commands::install_plugin,
            commands::uninstall_plugin,
            commands::keychain_available,
            commands::note_history,
            commands::note_version_content,
            commands::restore_note_version,
            commands::run_query_block,
            commands::create_share,
            commands::list_shares,
            commands::revoke_share,
            commands::publish_site,
            commands::clipper_info,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Onyx");
}
