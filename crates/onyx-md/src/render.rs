//! Markdown → HTML rendering: the semantic authority for reading view,
//! embeds/transclusions, and (later) export and publish.
//!
//! Wikilinks aren't CommonMark, so they are lifted out before parsing
//! (using the same code-aware extraction as indexing) and re-injected as
//! HTML afterwards via private-use-area placeholder tokens that survive
//! pulldown-cmark untouched.
//!
//! Security: raw HTML in notes is escaped, never passed through — rendered
//! output is injected into app surfaces, and a note must not be able to
//! smuggle markup (defense in depth alongside the CSP).

use pulldown_cmark::{Event, Options, Parser, html};

use crate::{LinkKind, extract};

/// Placeholder delimiters: Unicode private use area, vanishingly unlikely
/// in real notes and passed through markdown parsing as plain text.
const TOKEN_OPEN: char = '\u{E000}';
const TOKEN_CLOSE: char = '\u{E001}';

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "avif", "bmp"];

fn is_image_target(target: &str) -> bool {
    target
        .rsplit_once('.')
        .is_some_and(|(_, ext)| IMAGE_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Render a note body to HTML.
///
/// Wikilinks become `<a class="onyx-wikilink" data-target="…">`; image
/// embeds become `<img class="onyx-embed-image" data-vault-target="…">`;
/// note embeds become `<a class="onyx-wikilink onyx-embed-link" …>`. The
/// consumer wires navigation clicks and rewrites image sources to its
/// asset protocol — this crate stays UI-agnostic.
pub fn to_html(source: &str) -> String {
    let extracted = extract(source);
    let body_start = extracted.body_range.start;

    // Lift wikilinks (and only wikilinks — markdown links are CommonMark's
    // job) out of the source, replacing them with placeholder tokens.
    let wiki_links: Vec<_> = extracted
        .links
        .iter()
        .filter(|link| matches!(link.kind, LinkKind::Wiki | LinkKind::WikiEmbed))
        .collect();

    let mut rewritten = String::with_capacity(source.len());
    let mut cursor = body_start;
    for (token, link) in wiki_links.iter().enumerate() {
        rewritten.push_str(&source[cursor..link.span.start]);
        rewritten.push(TOKEN_OPEN);
        rewritten.push_str(&token.to_string());
        rewritten.push(TOKEN_CLOSE);
        cursor = link.span.end;
    }
    rewritten.push_str(&source[cursor..]);

    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let parser = Parser::new_ext(&rewritten, options).map(|event| match event {
        // Escape raw HTML: notes must not inject markup into app surfaces.
        Event::Html(raw) => Event::Text(raw),
        Event::InlineHtml(raw) => Event::Text(raw),
        other => other,
    });

    let mut rendered = String::with_capacity(rewritten.len() * 2);
    html::push_html(&mut rendered, parser);

    // Re-inject the lifted wikilinks. Placeholders may have been HTML-
    // escaped-adjacent but the PUA chars themselves pass through verbatim.
    let mut output = String::with_capacity(rendered.len());
    let mut rest = rendered.as_str();
    while let Some(open) = rest.find(TOKEN_OPEN) {
        output.push_str(&rest[..open]);
        let after = &rest[open + TOKEN_OPEN.len_utf8()..];
        let Some(close) = after.find(TOKEN_CLOSE) else {
            output.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let index: Option<usize> = after[..close].parse().ok();
        if let Some(link) = index.and_then(|i| wiki_links.get(i)) {
            let target = escape_html(&link.target);
            let display = escape_html(link.alias.as_deref().unwrap_or(&link.target));
            if link.kind == LinkKind::WikiEmbed && is_image_target(&link.target) {
                output.push_str(&format!(
                    r#"<img class="onyx-embed-image" data-vault-target="{target}" alt="{display}">"#
                ));
            } else if link.kind == LinkKind::WikiEmbed {
                output.push_str(&format!(
                    r#"<a class="onyx-wikilink onyx-embed-link" data-target="{target}">{display}</a>"#
                ));
            } else {
                let mut full = link.target.clone();
                if let Some(heading) = &link.heading {
                    full.push('#');
                    full.push_str(heading);
                }
                output.push_str(&format!(
                    r#"<a class="onyx-wikilink" data-target="{}">{display}</a>"#,
                    escape_html(&full)
                ));
            }
        }
        rest = &after[close + TOKEN_CLOSE.len_utf8()..];
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_basic_markdown() {
        let html = to_html("# Title\n\nSome **bold** text.");
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
    }

    #[test]
    fn frontmatter_is_not_rendered() {
        let html = to_html("---\ntitle: secret\n---\nbody");
        assert!(!html.contains("secret"));
        assert!(html.contains("body"));
    }

    #[test]
    fn wikilinks_become_data_target_anchors() {
        let html = to_html("See [[Other Note|the note]] and [[Plain]].");
        assert!(html.contains(r#"<a class="onyx-wikilink" data-target="Other Note">the note</a>"#));
        assert!(html.contains(r#"data-target="Plain">Plain</a>"#));
    }

    #[test]
    fn heading_refs_keep_fragment_in_target() {
        let html = to_html("[[Note#Section]]");
        assert!(html.contains(r#"data-target="Note#Section""#));
    }

    #[test]
    fn image_embeds_become_img_tags() {
        let html = to_html("![[pic.png]] and ![[photo.JPEG]]");
        assert_eq!(html.matches("<img class=\"onyx-embed-image\"").count(), 2);
        assert!(html.contains(r#"data-vault-target="pic.png""#));
    }

    #[test]
    fn note_embeds_become_embed_links() {
        let html = to_html("![[Some Note]]");
        assert!(html.contains("onyx-embed-link"));
    }

    #[test]
    fn wikilinks_inside_code_stay_literal() {
        let html = to_html("`[[not a link]]`\n\n```\n[[also not]]\n```");
        assert!(!html.contains("onyx-wikilink"));
        assert!(html.contains("[[not a link]]"));
    }

    #[test]
    fn raw_html_is_escaped() {
        let html = to_html("hello <script>alert(1)</script> <img src=x onerror=y>");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<img src=x"));
    }

    #[test]
    fn wikilink_targets_are_attribute_escaped() {
        let html = to_html(r#"[[a"onmouseover="x|click]]"#);
        assert!(!html.contains(r#""onmouseover"#));
        assert!(html.contains("&quot;"));
    }

    #[test]
    fn gfm_features_render() {
        let html = to_html("- [x] done\n- [ ] todo\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n~~gone~~");
        assert!(html.contains("checkbox"));
        assert!(html.contains("<table>"));
        assert!(html.contains("<del>gone</del>"));
    }

    #[test]
    fn wikilinks_render_inside_formatting() {
        let html = to_html("**bold [[Link]] text**");
        assert!(html.contains("<strong>"));
        assert!(html.contains("onyx-wikilink"));
    }

    #[test]
    fn stray_placeholder_chars_in_input_are_harmless() {
        let source = format!("weird {TOKEN_OPEN}99{TOKEN_CLOSE} input [[Real]]");
        let html = to_html(&source);
        // The stray token pair parses as index 99 → out of range → dropped;
        // the real link still renders and nothing panics.
        assert!(html.contains(r#"data-target="Real""#));
    }
}
