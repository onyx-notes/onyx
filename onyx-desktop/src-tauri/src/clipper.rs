//! Web clipper endpoint: a tiny localhost HTTP server the browser
//! extension POSTs clipped pages to. The extension does the Readability +
//! HTML→markdown extraction in the page; this side just turns the result
//! into a note. A random token (shown in settings, pasted into the
//! extension) gates writes so no other localhost page can inject notes.
//!
//! Deliberately minimal HTTP (only POST /clip + its CORS preflight) on a
//! fixed loopback port — no async runtime, no web framework.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use onyx_core::NotePath;
use parking_lot::Mutex;
use serde::Deserialize;

use crate::engine::Engine;

/// Loopback-only port the extension targets.
pub const CLIPPER_PORT: u16 = 47_600;

#[derive(Debug, Deserialize)]
pub struct Clip {
    pub title: String,
    pub url: String,
    /// Clean markdown produced by the extension (Readability + Turndown).
    pub markdown: String,
    /// Optional destination folder (defaults to "clippings").
    #[serde(default)]
    pub folder: String,
}

/// Turn a clip into `(vault path, note content)`. Pure and tested.
pub fn clip_to_note(clip: &Clip) -> (NotePath, String) {
    let folder = if clip.folder.trim().is_empty() {
        "clippings".to_owned()
    } else {
        sanitize_component(&clip.folder)
    };
    let stem = {
        let base = sanitize_component(&clip.title);
        if base.is_empty() {
            "clipping".to_owned()
        } else {
            base
        }
    };
    // NotePath validation can't fail for a sanitized `folder/stem.md`, but
    // fall back defensively.
    let path = NotePath::new(&format!("{folder}/{stem}.md"))
        .unwrap_or_else(|_| NotePath::new("clippings/clipping.md").expect("static"));

    let content = format!(
        "---\ntitle: {}\nsource: {}\nclipped: true\n---\n\n{}\n",
        yaml_escape(&clip.title),
        yaml_escape(&clip.url),
        clip.markdown.trim()
    );
    (path, content)
}

/// Make a string safe as a single path component: drop separators and
/// filesystem-hostile characters, collapse whitespace, cap length.
fn sanitize_component(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' | '\n' | '\r' => {
                out.push(' ')
            }
            '.' if out.is_empty() => {} // no leading dots (hidden files)
            _ => out.push(c),
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .chars()
        .take(120)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn yaml_escape(text: &str) -> String {
    // Quote to survive colons/hashes; escape embedded quotes.
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A running clipper server. Dropping it stops the listener thread.
pub struct Clipper {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    port: u16,
}

impl Clipper {
    #[cfg(test)]
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Clipper {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        // Nudge the accept loop with a throwaway connection.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start the clipper on loopback. `on_clip` is called with a validated
/// clip; it returns the created path (for the extension's confirmation).
pub fn spawn(token: String, engine: Arc<Mutex<Option<Engine>>>) -> std::io::Result<Clipper> {
    spawn_on(CLIPPER_PORT, token, engine)
}

/// Like [`spawn`] but on a specific port (0 = OS-assigned; used by tests).
pub fn spawn_on(
    port: u16,
    token: String,
    engine: Arc<Mutex<Option<Engine>>>,
) -> std::io::Result<Clipper> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let bound_port = listener.local_addr()?.port();
    listener.set_nonblocking(false)?;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);

    let thread = std::thread::Builder::new()
        .name("onyx-clipper".into())
        .spawn(move || {
            for incoming in listener.incoming() {
                if stop_thread.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                let Ok(stream) = incoming else { continue };
                let _ = handle(stream, &token, &engine);
            }
        })?;

    Ok(Clipper {
        stop,
        thread: Some(thread),
        port: bound_port,
    })
}

fn handle(
    mut stream: TcpStream,
    token: &str,
    engine: &Arc<Mutex<Option<Engine>>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    // Headers.
    let mut content_length = 0usize;
    let mut auth = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = trimmed.strip_prefix("X-Onyx-Token:") {
            auth = value.trim().to_owned();
        }
    }

    // CORS preflight from the extension.
    if method == "OPTIONS" {
        return write_response(&mut stream, 204, "", true);
    }
    if method != "POST" || path != "/clip" {
        return write_response(&mut stream, 404, "not found", true);
    }
    if auth != token {
        return write_response(&mut stream, 401, "bad token", true);
    }

    let mut body = vec![0u8; content_length.min(4 * 1024 * 1024)];
    reader.read_exact(&mut body)?;

    let clip: Clip = match serde_json::from_slice(&body) {
        Ok(clip) => clip,
        Err(_) => return write_response(&mut stream, 400, "bad json", true),
    };
    let (note_path, content) = clip_to_note(&clip);

    let result = {
        let mut guard = engine.lock();
        match guard.as_mut() {
            Some(engine) => engine.write_note(&note_path, &content).map(|_| ()),
            None => return write_response(&mut stream, 409, "no vault open", true),
        }
    };
    match result {
        Ok(()) => write_response(
            &mut stream,
            200,
            &format!("{{\"path\":\"{}\"}}", note_path.as_str()),
            true,
        ),
        Err(error) => write_response(&mut stream, 500, &error.to_string(), true),
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    body: &str,
    cors: bool,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Internal Server Error",
    };
    let cors_headers = if cors {
        "Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type, X-Onyx-Token\r\n"
    } else {
        ""
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         {cors_headers}\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(title: &str, url: &str, md: &str, folder: &str) -> Clip {
        Clip {
            title: title.into(),
            url: url.into(),
            markdown: md.into(),
            folder: folder.into(),
        }
    }

    #[test]
    fn builds_note_with_frontmatter() {
        let (path, content) = clip_to_note(&clip(
            "Great Article",
            "https://example.com/x",
            "# Great Article\n\nBody text.",
            "",
        ));
        assert_eq!(path.as_str(), "clippings/Great Article.md");
        assert!(content.contains("source: \"https://example.com/x\""));
        assert!(content.contains("clipped: true"));
        assert!(content.contains("Body text."));
    }

    #[test]
    fn sanitizes_hostile_titles() {
        let (path, _) = clip_to_note(&clip("a/b:c*?<>|.md", "u", "x", "notes/sub"));
        assert_eq!(path.as_str(), "notes sub/a b c .md.md");
        // No path traversal or separators leaked into the component.
        assert!(!path.as_str().contains(".."));
    }

    #[test]
    fn empty_title_and_folder_fall_back() {
        let (path, _) = clip_to_note(&clip("   ", "u", "body", "  "));
        assert_eq!(path.as_str(), "clippings/clipping.md");
    }

    #[test]
    fn yaml_escaping_survives_colons_and_quotes() {
        let (_, content) = clip_to_note(&clip("Title: with \"quotes\"", "u", "b", ""));
        assert!(content.contains(r#"title: "Title: with \"quotes\"""#));
    }

    #[test]
    fn server_writes_a_note_and_rejects_bad_token() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path()).unwrap();
        let shared = Arc::new(Mutex::new(Some(engine)));
        let clipper = spawn_on(0, "secret-token".into(), Arc::clone(&shared)).unwrap();
        let port = clipper.port();
        std::thread::sleep(std::time::Duration::from_millis(100));

        let post = |token: &str, body: &str| -> String {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                stream,
                "POST /clip HTTP/1.1\r\nHost: localhost\r\nX-Onyx-Token: {token}\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let mut response = String::new();
            std::io::Read::read_to_string(&mut stream, &mut response).unwrap();
            response
        };

        let body = r#"{"title":"Clipped","url":"https://a.b","markdown":"hello"}"#;
        // Bad token → 401, nothing written.
        assert!(post("wrong", body).contains("401"));
        // Good token → 200, note on disk.
        let ok = post("secret-token", body);
        assert!(ok.contains("200"), "{ok}");
        assert!(dir.path().join("clippings/Clipped.md").exists());
        let written = std::fs::read_to_string(dir.path().join("clippings/Clipped.md")).unwrap();
        assert!(written.contains("hello"));
        assert!(written.contains("source: \"https://a.b\""));
    }
}
