//! The `onyx://` custom protocol: the bulk-data lane.
//!
//! Tauri `invoke` JSON-serializes every payload; streaming raw bytes through
//! a URI scheme avoids that tax and gives the webview native fetch caching.
//! Routes:
//!
//! - `onyx://…/file/<percent-encoded vault path>` — raw bytes of any vault
//!   file (note bodies, embedded images), content-type by extension.

use onyx_core::NotePath;
use tauri::http::{Response, StatusCode, header};
use tauri::{Manager, UriSchemeContext, UriSchemeResponder};

use crate::state::AppState;

pub fn handle(
    context: UriSchemeContext<'_, tauri::Wry>,
    request: tauri::http::Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = context.app_handle().clone();
    // Serve off the IPC thread: file reads must never block the UI process.
    std::thread::spawn(move || {
        let response = serve(&app, request.uri().path());
        responder.respond(response);
    });
}

fn serve(app: &tauri::AppHandle, uri_path: &str) -> Response<Vec<u8>> {
    if let Some(plugin_id) = uri_path.strip_prefix("/plugin/") {
        return serve_plugin_page(plugin_id);
    }
    if let Some(plugin_id) = uri_path.strip_prefix("/plugin-code/") {
        return serve_plugin_code(app, plugin_id);
    }
    let Some(encoded) = uri_path.strip_prefix("/file/") else {
        return error_response(StatusCode::NOT_FOUND, "unknown route");
    };
    let Ok(note) = NotePath::new(&percent_decode(encoded)) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid path");
    };

    let state = app.state::<AppState>();
    let bytes = {
        let guard = state.engine.lock();
        let Some(engine) = guard.as_ref() else {
            return error_response(StatusCode::CONFLICT, "no vault open");
        };
        engine.vault().read_bytes(&note)
    };

    match bytes {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type(&note))
            // Vault files change under us; the frontend re-fetches on
            // vault events, so don't let the webview cache stale bodies.
            .header(header::CACHE_CONTROL, "no-cache")
            .body(bytes)
            .expect("static response construction"),
        Err(_) => error_response(StatusCode::NOT_FOUND, "not found"),
    }
}

fn error_response(status: StatusCode, message: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(message.as_bytes().to_vec())
        .expect("static response construction")
}

fn content_type(path: &NotePath) -> &'static str {
    match path.extension().as_deref() {
        Some("md" | "markdown") => "text/markdown; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 {
            if let Some(byte) = bytes
                .get(i + 1..i + 3)
                .and_then(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
            {
                decoded.push(byte);
                i += 3;
                continue;
            }
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_percent_sequences() {
        assert_eq!(
            percent_decode("folder%2Fnote%20one.md"),
            "folder/note one.md"
        );
        assert_eq!(percent_decode("plain.md"), "plain.md");
        // Invalid sequences pass through.
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
        assert_eq!(percent_decode("trail%"), "trail%");
    }

    #[test]
    fn content_types_by_extension() {
        let md = NotePath::new("a.md").unwrap();
        assert!(content_type(&md).starts_with("text/markdown"));
        let png = NotePath::new("i.PNG").unwrap();
        assert_eq!(content_type(&png), "image/png");
        let unknown = NotePath::new("x.bin").unwrap();
        assert_eq!(content_type(&unknown), "application/octet-stream");
    }
}

/// Plugin ids are constrained to a safe charset (they become paths).
fn valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The sandbox bootstrap page: defines the `onyx` plugin API (postMessage
/// RPC to the host broker), then loads the plugin's code. Served under the
/// custom scheme so the iframe gets a DIFFERENT origin from the app —
/// isolation by the browser's own origin model, plus the sandbox attribute.
fn serve_plugin_page(plugin_id: &str) -> Response<Vec<u8>> {
    if !valid_plugin_id(plugin_id) {
        return error_response(StatusCode::BAD_REQUEST, "invalid plugin id");
    }
    let html = PLUGIN_BOOTSTRAP.replace("__PLUGIN_ID__", plugin_id);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(html.into_bytes())
        .expect("static response construction")
}

const PLUGIN_BOOTSTRAP: &str = r#"<!doctype html><meta charset="utf-8"><script>
(() => {
  const PLUGIN_ID = "__PLUGIN_ID__";
  const pending = new Map();
  let nextId = 1;
  const commands = new Map();
  const call = (method, params) => new Promise((resolve, reject) => {
    const id = nextId++;
    pending.set(id, { resolve, reject });
    parent.postMessage({ onyxPlugin: PLUGIN_ID, id, method, params }, "*");
  });
  addEventListener("message", (event) => {
    const message = event.data;
    if (!message) return;
    if (message.onyxReply !== undefined && pending.has(message.id)) {
      const promise = pending.get(message.id);
      pending.delete(message.id);
      message.ok ? promise.resolve(message.value) : promise.reject(new Error(message.error));
    } else if (message.onyxRunCommand !== undefined) {
      const run = commands.get(message.commandId);
      if (run) Promise.resolve(run()).catch((error) => call("notice", { message: String(error) }));
    }
  });
  globalThis.onyx = {
    vault: {
      read: (path) => call("vault.read", { path }),
      write: (path, content) => call("vault.write", { path, content }),
      list: () => call("vault.list", {}),
    },
    commands: {
      register: (command) => {
        commands.set(command.id, command.run);
        return call("commands.register", { id: command.id, name: command.name });
      },
    },
    notice: (message) => call("notice", { message }),
  };
})();
</script><script src="/plugin-code/__PLUGIN_ID__"></script>"#;

fn serve_plugin_code(app: &tauri::AppHandle, plugin_id: &str) -> Response<Vec<u8>> {
    if !valid_plugin_id(plugin_id) {
        return error_response(StatusCode::BAD_REQUEST, "invalid plugin id");
    }
    let state = app.state::<AppState>();
    let root = {
        let guard = state.engine.lock();
        let Some(engine) = guard.as_ref() else {
            return error_response(StatusCode::CONFLICT, "no vault open");
        };
        engine.root().to_path_buf()
    };
    let code_path = root.join(".onyx/plugins").join(plugin_id).join("main.js");
    match std::fs::read(code_path) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/javascript; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(bytes)
            .expect("static response construction"),
        Err(_) => error_response(StatusCode::NOT_FOUND, "plugin code not found"),
    }
}

#[cfg(test)]
mod plugin_route_tests {
    use super::*;

    #[test]
    fn plugin_id_validation() {
        assert!(valid_plugin_id("word-count"));
        assert!(valid_plugin_id("a1"));
        assert!(!valid_plugin_id(""));
        assert!(!valid_plugin_id("UPPER"));
        assert!(!valid_plugin_id("../escape"));
        assert!(!valid_plugin_id("space id"));
        assert!(!valid_plugin_id(&"x".repeat(65)));
    }

    #[test]
    fn bootstrap_embeds_plugin_id() {
        let response = serve_plugin_page("my-plugin");
        let html = String::from_utf8(response.into_body()).unwrap();
        assert!(html.contains(r#"const PLUGIN_ID = "my-plugin";"#));
        assert!(html.contains("/plugin-code/my-plugin"));
        assert!(!html.contains("__PLUGIN_ID__"));
    }
}
