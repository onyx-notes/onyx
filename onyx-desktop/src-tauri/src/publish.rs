//! Self-hosted Publish: render a vault folder to a static HTML site you
//! can host anywhere. Replaces Obsidian's paid Publish — the output is
//! plain files, no runtime, no lock-in.
//!
//! Pure and testable: [`build_site`] takes the notes and returns the files
//! to write; the command layer handles disk + attachment copying.

use std::collections::{HashMap, HashSet};

/// A note to publish: vault path + markdown source.
pub struct SourceNote {
    pub path: String,
    pub content: String,
}

/// One output file (relative path under the site root → bytes).
pub struct OutputFile {
    pub path: String,
    pub contents: String,
}

/// Options controlling the generated site.
pub struct SiteOptions {
    pub title: String,
}

/// Render a set of notes into a static site: one `.html` per note, an
/// index page, a shared stylesheet, and resolved inter-note links.
pub fn build_site(notes: &[SourceNote], options: &SiteOptions) -> Vec<OutputFile> {
    // Resolution map: casefolded stem AND full path → output html path.
    let mut resolve: HashMap<String, String> = HashMap::new();
    for note in notes {
        let html_path = to_html_path(&note.path);
        let stem = note_stem(&note.path).to_lowercase();
        let full = strip_md(&note.path).to_lowercase();
        // Full path wins; only fill a stem if unambiguous later.
        resolve.entry(full).or_insert_with(|| html_path.clone());
        resolve.entry(stem).or_insert(html_path);
    }

    let mut files = Vec::with_capacity(notes.len() + 2);
    let mut index_entries: Vec<(String, String)> = Vec::new();

    for note in notes {
        let body_html = onyx_md::to_html(&note.content);
        let linked = rewrite_links(&body_html, &resolve, depth_of(&note.path));
        let title = note_stem(&note.path);
        let out_path = to_html_path(&note.path);
        let page = wrap_page(title, &linked, depth_of(&note.path), &options.title);
        index_entries.push((title.to_owned(), out_path.clone()));
        files.push(OutputFile {
            path: out_path,
            contents: page,
        });
    }

    index_entries.sort_by_key(|entry| entry.0.to_lowercase());
    files.push(OutputFile {
        path: "index.html".into(),
        contents: build_index(&index_entries, &options.title),
    });
    files.push(OutputFile {
        path: "onyx-publish.css".into(),
        contents: STYLESHEET.into(),
    });
    files
}

/// Collect the attachment targets referenced by embeds/images across all
/// notes, so the caller can copy them into the site.
pub fn referenced_attachments(notes: &[SourceNote]) -> HashSet<String> {
    let mut targets = HashSet::new();
    for note in notes {
        for link in onyx_md::extract(&note.content).links {
            let is_image = matches!(
                link.kind,
                onyx_md::LinkKind::WikiEmbed | onyx_md::LinkKind::MarkdownEmbed
            ) && has_image_ext(&link.target);
            if is_image {
                targets.insert(link.target);
            }
        }
    }
    targets
}

fn has_image_ext(target: &str) -> bool {
    matches!(
        target
            .rsplit('.')
            .next()
            .map(|e| e.to_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "avif")
    )
}

fn strip_md(path: &str) -> String {
    path.strip_suffix(".md")
        .or_else(|| path.strip_suffix(".markdown"))
        .unwrap_or(path)
        .to_owned()
}

fn to_html_path(path: &str) -> String {
    format!("{}.html", strip_md(path))
}

fn note_stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".md")
        .or_else(|| name.strip_suffix(".markdown"))
        .unwrap_or(name)
}

fn depth_of(path: &str) -> usize {
    path.matches('/').count()
}

fn relative_prefix(depth: usize) -> String {
    "../".repeat(depth)
}

/// Rewrite `data-target` wikilink anchors and vault-image sources into
/// site-relative links.
fn rewrite_links(html: &str, resolve: &HashMap<String, String>, depth: usize) -> String {
    let prefix = relative_prefix(depth);
    let mut out = html.to_owned();

    // Wikilink anchors: <a class="onyx-wikilink" data-target="X">…</a>
    out = replace_attr(&out, "data-target=\"", |target| {
        let key = strip_md(target).to_lowercase();
        // Fragment part after '#'.
        let (base, frag) = match key.split_once('#') {
            Some((b, f)) => (b.to_owned(), format!("#{f}")),
            None => (key, String::new()),
        };
        match resolve.get(&base) {
            Some(html_path) => format!("href=\"{prefix}{html_path}{frag}\" data-resolved=\"1\""),
            // Unresolved link → non-link span styling via a class marker.
            None => "class=\"onyx-broken\"".to_owned(),
        }
    });

    // Vault image embeds: <img … data-vault-target="X">
    out = replace_attr(&out, "data-vault-target=\"", |target| {
        format!("src=\"{prefix}assets/{target}\"")
    });
    out
}

/// Replace every `attr="value"` occurrence, feeding `value` to `f` and
/// substituting the whole `attr="value"` with `f(value)`.
fn replace_attr(html: &str, attr: &str, f: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find(attr) {
        out.push_str(&rest[..start]);
        let after = &rest[start + attr.len()..];
        if let Some(end) = after.find('"') {
            out.push_str(&f(&after[..end]));
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            break;
        }
    }
    out.push_str(rest);
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn wrap_page(title: &str, body: &str, depth: usize, site_title: &str) -> String {
    let prefix = relative_prefix(depth);
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{} · {}</title>\
         <link rel=\"stylesheet\" href=\"{prefix}onyx-publish.css\"></head>\
         <body><nav><a href=\"{prefix}index.html\">{}</a></nav>\
         <main><h1>{}</h1>{}</main>\
         <footer>Published with Onyx</footer></body></html>",
        escape(title),
        escape(site_title),
        escape(site_title),
        escape(title),
        body
    )
}

fn build_index(entries: &[(String, String)], site_title: &str) -> String {
    let mut list = String::new();
    for (title, path) in entries {
        list.push_str(&format!(
            "<li><a href=\"{}\">{}</a></li>",
            path,
            escape(title)
        ));
    }
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{}</title>\
         <link rel=\"stylesheet\" href=\"onyx-publish.css\"></head>\
         <body><main><h1>{}</h1><ul class=\"note-index\">{}</ul></main>\
         <footer>Published with Onyx</footer></body></html>",
        escape(site_title),
        escape(site_title),
        list
    )
}

const STYLESHEET: &str = "\
:root { color-scheme: dark light; }
body { font-family: system-ui, sans-serif; max-width: 46rem; margin: 0 auto;
       padding: 1rem; line-height: 1.65; background: #16161d; color: #e6e6ec; }
@media (prefers-color-scheme: light) { body { background: #fbfbfd; color: #1d1d26; } }
nav { padding: .5rem 0; opacity: .8; }
nav a { color: #8b7ff5; text-decoration: none; }
main { padding: 1rem 0; }
a.onyx-wikilink { color: #8b7ff5; text-decoration: none; }
a.onyx-wikilink:hover { text-decoration: underline; }
.onyx-broken { color: #71717e; }
pre { background: #00000022; padding: .6rem; border-radius: 6px; overflow-x: auto; }
code { font-family: ui-monospace, monospace; }
img { max-width: 100%; border-radius: 6px; }
table { border-collapse: collapse; } th,td { border: 1px solid #8884; padding: 4px 10px; }
ul.note-index { list-style: none; padding: 0; }
ul.note-index li { padding: 3px 0; }
footer { margin-top: 3rem; font-size: 12px; opacity: .5; }
";

#[cfg(test)]
mod tests {
    use super::*;

    fn note(path: &str, content: &str) -> SourceNote {
        SourceNote {
            path: path.into(),
            content: content.into(),
        }
    }

    fn options() -> SiteOptions {
        SiteOptions {
            title: "My Site".into(),
        }
    }

    fn find<'a>(files: &'a [OutputFile], path: &str) -> &'a OutputFile {
        files.iter().find(|f| f.path == path).expect("file exists")
    }

    #[test]
    fn generates_page_index_and_css() {
        let notes = vec![note("a.md", "# A\nhello")];
        let files = build_site(&notes, &options());
        assert!(files.iter().any(|f| f.path == "a.html"));
        assert!(files.iter().any(|f| f.path == "index.html"));
        assert!(files.iter().any(|f| f.path == "onyx-publish.css"));

        let page = find(&files, "a.html");
        assert!(page.contents.contains("<h1>a</h1>"));
        assert!(page.contents.contains("hello"));
        assert!(page.contents.contains("onyx-publish.css"));
    }

    #[test]
    fn wikilinks_resolve_to_relative_html() {
        let notes = vec![
            note("index note.md", "See [[Target]]."),
            note("folder/Target.md", "# Target"),
        ];
        let files = build_site(&notes, &options());
        let page = find(&files, "index note.html");
        // Resolves to the target's html path, relative from root.
        assert!(
            page.contents.contains("href=\"folder/Target.html\""),
            "{}",
            page.contents
        );
    }

    #[test]
    fn nested_notes_get_correct_relative_prefixes() {
        let notes = vec![
            note("deep/nested/note.md", "[[Home]]"),
            note("Home.md", "# Home"),
        ];
        let files = build_site(&notes, &options());
        let page = find(&files, "deep/nested/note.html");
        // Two directories deep → ../../ prefix to reach Home.html + css.
        assert!(
            page.contents.contains("href=\"../../Home.html\""),
            "{}",
            page.contents
        );
        assert!(page.contents.contains("../../onyx-publish.css"));
    }

    #[test]
    fn unresolved_links_become_broken_spans() {
        let notes = vec![note("a.md", "[[Ghost]]")];
        let files = build_site(&notes, &options());
        let page = find(&files, "a.html");
        assert!(page.contents.contains("onyx-broken"));
        assert!(!page.contents.contains("data-target"));
    }

    #[test]
    fn image_embeds_point_at_assets() {
        let notes = vec![note("a.md", "![[pic.png]]")];
        let files = build_site(&notes, &options());
        let page = find(&files, "a.html");
        assert!(
            page.contents.contains("src=\"assets/pic.png\""),
            "{}",
            page.contents
        );
        assert!(referenced_attachments(&notes).contains("pic.png"));
    }

    #[test]
    fn index_lists_notes_sorted() {
        let notes = vec![note("Zebra.md", "z"), note("apple.md", "a")];
        let files = build_site(&notes, &options());
        let index = find(&files, "index.html");
        let z = index.contents.find("Zebra").unwrap();
        let a = index.contents.find("apple").unwrap();
        assert!(a < z, "case-insensitive sort: apple before Zebra");
    }

    #[test]
    fn raw_html_in_notes_is_escaped_by_renderer() {
        let notes = vec![note("a.md", "<script>alert(1)</script>")];
        let page = find(&build_site(&notes, &options()), "a.html")
            .contents
            .clone();
        assert!(!page.contains("<script>alert(1)"));
    }
}
