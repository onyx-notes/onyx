//! Global quick-capture: a system-wide shortcut that shows a small always-
//! on-top window for jotting a thought straight into today's daily note,
//! without switching to the main window.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

const CAPTURE_LABEL: &str = "capture";

/// Register CmdOrCtrl+Shift+Space to toggle the capture window.
pub fn register_quick_capture(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let shortcut = Shortcut::new(Some(Modifiers::SHIFT | Modifiers::SUPER), Code::Space);
    let app_for_handler = app.clone();

    app.global_shortcut()
        .on_shortcut(shortcut, move |_app, _sc, event| {
            if event.state() == ShortcutState::Pressed {
                toggle_capture_window(&app_for_handler);
            }
        })?;
    Ok(())
}

fn toggle_capture_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(CAPTURE_LABEL) {
        // Already open: focus it (or hide if it already had focus).
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    // The capture window loads the SPA with ?capture=1 so the frontend
    // renders the minimal capture UI instead of the full app.
    let result = WebviewWindowBuilder::new(
        app,
        CAPTURE_LABEL,
        WebviewUrl::App("index.html?capture=1".into()),
    )
    .title("Quick capture")
    .inner_size(480.0, 200.0)
    .always_on_top(true)
    .decorations(false)
    .center()
    .build();
    if let Err(error) = result {
        tracing::warn!(%error, "failed to open capture window");
    }
}
