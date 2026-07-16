//! Single-pass line-oriented scanner for links, tags, and headings.
//!
//! The scanner walks the body line by line, tracking block state (fenced
//! code, `%%` comments, HTML comments) and masking inline code spans, so
//! that link/tag syntax inside code or comments is never extracted —
//! matching Obsidian's behavior.

use crate::{Heading, LinkKind, LinkRef, TagRef};

#[derive(Debug, Default)]
pub(crate) struct ScanResult {
    pub links: Vec<LinkRef>,
    pub tags: Vec<TagRef>,
    pub headings: Vec<Heading>,
}

/// Carry-over state between lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    /// Inside a fenced code block opened with `marker` repeated `len` times.
    Fence {
        marker: u8,
        len: usize,
    },
    /// Inside a `%% … %%` Obsidian comment.
    PercentComment,
    /// Inside an HTML `<!-- … -->` comment.
    HtmlComment,
}

pub(crate) fn scan(body: &str, base: usize) -> ScanResult {
    let mut result = ScanResult::default();
    let mut state = State::Normal;
    let mut line_start = 0;

    for line in body.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let offset = base + line_start;

        match state {
            State::Fence { marker, len } => {
                if is_fence_close(content, marker, len) {
                    state = State::Normal;
                }
            }
            State::Normal => {
                if let Some((marker, len)) = fence_open(content) {
                    state = State::Fence { marker, len };
                } else {
                    if let Some(heading) = parse_heading(content, offset) {
                        result.headings.push(heading);
                    }
                    state = scan_inline(content, offset, State::Normal, &mut result);
                }
            }
            // Inside comments only the closing marker matters; fences and
            // headings inside a comment are not real.
            State::PercentComment | State::HtmlComment => {
                state = scan_inline(content, offset, state, &mut result);
            }
        }

        line_start += line.len();
    }

    result
}

/// Detect a fence opening after optional indentation and blockquote markers
/// (so code fences inside callouts still suppress extraction).
fn fence_open(line: &str) -> Option<(u8, usize)> {
    let stripped = strip_quote_prefix(line);
    let bytes = stripped.as_bytes();
    let marker = *bytes.first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let len = bytes.iter().take_while(|&&byte| byte == marker).count();
    if len < 3 {
        return None;
    }
    // CommonMark: a backtick fence's info string may not contain backticks.
    if marker == b'`' && stripped[len..].contains('`') {
        return None;
    }
    Some((marker, len))
}

fn is_fence_close(line: &str, marker: u8, open_len: usize) -> bool {
    let stripped = strip_quote_prefix(line);
    let bytes = stripped.as_bytes();
    let len = bytes.iter().take_while(|&&byte| byte == marker).count();
    len >= open_len && stripped[len..].trim().is_empty()
}

/// Strip leading whitespace and blockquote (`>`) markers.
fn strip_quote_prefix(line: &str) -> &str {
    let mut rest = line.trim_start();
    while let Some(after) = rest.strip_prefix('>') {
        rest = after.trim_start();
    }
    rest
}

/// Parse an ATX heading: up to 3 leading spaces, 1–6 `#`, then space or EOL.
fn parse_heading(line: &str, offset: usize) -> Option<Heading> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let level = rest.bytes().take_while(|&byte| byte == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = &rest[level..];
    if !after.is_empty() && !after.starts_with([' ', '\t']) {
        return None;
    }

    // Strip the optional closing sequence: trailing `#`s preceded by a space.
    let mut text = after.trim();
    let closing = text.len() - text.trim_end_matches('#').len();
    if closing > 0 {
        let before = &text[..text.len() - closing];
        if before.is_empty() || before.ends_with([' ', '\t']) {
            text = before.trim_end();
        }
    }

    Some(Heading {
        level: level as u8,
        text: text.to_owned(),
        span: offset..offset + line.trim_end().len(),
    })
}

/// Scan one line for inline elements. Returns the state to carry into the
/// next line (comments may span lines; inline code may not).
fn scan_inline(line: &str, offset: usize, entry: State, out: &mut ScanResult) -> State {
    let mut state = entry;
    let mut i = 0;

    while i < line.len() {
        let rest = &line[i..];
        match state {
            State::PercentComment => match rest.find("%%") {
                Some(at) => {
                    i += at + 2;
                    state = State::Normal;
                }
                None => return state,
            },
            State::HtmlComment => match rest.find("-->") {
                Some(at) => {
                    i += at + 3;
                    state = State::Normal;
                }
                None => return state,
            },
            State::Fence { .. } => unreachable!("fence lines are skipped before inline scan"),
            State::Normal => {
                let c = rest.chars().next().expect("i is on a char boundary");
                if c == '\\' {
                    i += 1 + rest[1..].chars().next().map_or(0, char::len_utf8);
                } else if c == '`' {
                    i += skip_code_span(rest);
                } else if rest.starts_with("%%") {
                    i += 2;
                    state = State::PercentComment;
                } else if rest.starts_with("<!--") {
                    i += 4;
                    state = State::HtmlComment;
                } else if let Some(consumed) = try_wikilink(line, i, offset, out) {
                    i = consumed;
                } else if c == '[' {
                    match try_markdown_link(line, i, offset, out) {
                        Some(end) => i = end,
                        None => i += 1,
                    }
                } else if c == '#' {
                    match try_tag(line, i, offset, out) {
                        Some(end) => i = end,
                        None => i += 1,
                    }
                } else {
                    i += c.len_utf8();
                }
            }
        }
    }

    state
}

/// Consume an inline code span (`` `code` ``). Returns bytes consumed; if no
/// matching closing run exists on this line the backtick run is literal.
fn skip_code_span(rest: &str) -> usize {
    let open = rest.bytes().take_while(|&byte| byte == b'`').count();
    let mut search = open;
    while let Some(at) = rest[search..].find('`') {
        let run_start = search + at;
        let run = rest[run_start..]
            .bytes()
            .take_while(|&byte| byte == b'`')
            .count();
        if run == open {
            return run_start + run;
        }
        search = run_start + run;
    }
    open
}

/// Try to consume `[[…]]` or `![[…]]` at byte `i`. Returns the new scan
/// position on success.
fn try_wikilink(line: &str, i: usize, offset: usize, out: &mut ScanResult) -> Option<usize> {
    let (start, embed) = if line[i..].starts_with("![[") {
        (i, true)
    } else if line[i..].starts_with("[[") {
        (i, false)
    } else {
        return None;
    };
    let inner_start = start + if embed { 3 } else { 2 };
    let close = line[inner_start..].find("]]")?;
    let inner = &line[inner_start..inner_start + close];

    // `[[a [[b]]` — the first `[[` is literal; the scanner will retry at the
    // nested opener on the next iterations.
    if inner.contains("[[") || inner.is_empty() {
        return None;
    }

    let end = inner_start + close + 2;
    let (target_part, alias) = match inner.split_once('|') {
        Some((target, alias)) => (target, non_empty(alias.trim())),
        None => (inner, None),
    };
    let (target, heading, block) = split_reference(target_part);
    if target.is_empty() && heading.is_none() && block.is_none() {
        return None;
    }

    out.links.push(LinkRef {
        kind: if embed {
            LinkKind::WikiEmbed
        } else {
            LinkKind::Wiki
        },
        target,
        heading,
        block,
        alias,
        span: offset + start..offset + end,
    });
    Some(end)
}

/// Split `path#heading` / `path#^block` into components.
fn split_reference(reference: &str) -> (String, Option<String>, Option<String>) {
    match reference.split_once('#') {
        Some((path, fragment)) => {
            let path = path.trim().to_owned();
            match fragment.strip_prefix('^') {
                Some(block) => (path, None, non_empty(block.trim())),
                None => (path, non_empty(fragment.trim()), None),
            }
        }
        None => (reference.trim().to_owned(), None, None),
    }
}

/// Try to consume `[text](dest)` / `![alt](dest)` at byte `i` (pointing at
/// the `[`).
fn try_markdown_link(line: &str, i: usize, offset: usize, out: &mut ScanResult) -> Option<usize> {
    let embed = line[..i].ends_with('!');
    let start = if embed { i - 1 } else { i };

    let text_end = find_bracket_close(&line[i..])?;
    let text = &line[i + 1..i + text_end];
    let after_bracket = i + text_end + 1;
    if !line[after_bracket..].starts_with('(') {
        return None;
    }

    let dest_close = find_paren_close(&line[after_bracket..])?;
    let raw_dest = line[after_bracket + 1..after_bracket + dest_close].trim();
    let end = after_bracket + dest_close + 1;

    let dest = if let Some(bracketed) = raw_dest.strip_prefix('<') {
        // `<dest>` may be followed by a `"title"`; cut at the closing `>`.
        match bracketed.split_once('>') {
            Some((inside, _)) => inside.to_owned(),
            None => bracketed.to_owned(),
        }
    } else {
        // Drop an optional `"title"` / `'title'` suffix.
        match raw_dest.find(|c: char| c.is_whitespace()) {
            Some(space) if raw_dest[space..].trim_start().starts_with(['"', '\'']) => {
                raw_dest[..space].to_owned()
            }
            _ => raw_dest.to_owned(),
        }
    };
    if dest.is_empty() {
        return None;
    }

    let span = offset + start..offset + end;
    let alias = non_empty(text.trim());

    if has_uri_scheme(&dest) {
        out.links.push(LinkRef {
            kind: LinkKind::External,
            target: dest,
            heading: None,
            block: None,
            alias,
            span,
        });
    } else {
        let decoded = percent_decode(&dest);
        let (target, heading, block) = split_reference(&decoded);
        if target.is_empty() && heading.is_none() && block.is_none() {
            return None;
        }
        out.links.push(LinkRef {
            kind: if embed {
                LinkKind::MarkdownEmbed
            } else {
                LinkKind::Markdown
            },
            target,
            heading,
            block,
            alias,
            span,
        });
    }
    Some(end)
}

/// Find the `]` closing the bracket at position 0, honoring nesting and
/// backslash escapes. Returns its index.
fn find_bracket_close(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Find the `)` closing the paren at position 0, honoring nesting and
/// backslash escapes. Returns its index.
fn find_paren_close(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn has_uri_scheme(dest: &str) -> bool {
    let Some(colon) = dest.find(':') else {
        return false;
    };
    let scheme = &dest[..colon];
    let mut chars = scheme.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && scheme.len() > 1 // single letters are Windows drive prefixes, not schemes
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

/// Try to consume `#tag` at byte `i`. Tags must be preceded by whitespace or
/// start of line, contain at least one non-digit character, and consist of
/// alphanumerics, `_`, `-`, `/`.
fn try_tag(line: &str, i: usize, offset: usize, out: &mut ScanResult) -> Option<usize> {
    let preceded_ok = line[..i]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace);
    if !preceded_ok {
        return None;
    }

    let body = &line[i + 1..];
    let len: usize = body
        .chars()
        .take_while(|&c| c.is_alphanumeric() || matches!(c, '_' | '-' | '/'))
        .map(char::len_utf8)
        .sum();
    let tag = body[..len].trim_end_matches('/');
    if tag.is_empty() || tag.starts_with('/') || !tag.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }

    out.tags.push(TagRef {
        tag: tag.to_owned(),
        span: offset + i..offset + i + 1 + tag.len(),
    });
    Some(i + 1 + len)
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

/// Decode `%XX` escapes; invalid sequences pass through unchanged.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 {
            let hex = bytes.get(i + 1..i + 3);
            if let Some(byte) =
                hex.and_then(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
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
