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
