//! End-to-end extraction tests covering the Obsidian markdown dialect.

use onyx_md::{LinkKind, extract};

fn link_targets(source: &str) -> Vec<String> {
    extract(source)
        .links
        .into_iter()
        .map(|l| l.target)
        .collect()
}

fn tag_names(source: &str) -> Vec<String> {
    extract(source).tags.into_iter().map(|t| t.tag).collect()
}

#[test]
fn plain_wikilink() {
    let note = extract("See [[Other Note]] for details.");
    assert_eq!(note.links.len(), 1);
    let link = &note.links[0];
    assert_eq!(link.kind, LinkKind::Wiki);
    assert_eq!(link.target, "Other Note");
    assert_eq!(link.heading, None);
    assert_eq!(link.block, None);
    assert_eq!(link.alias, None);
    assert_eq!(
        &"See [[Other Note]] for details."[link.span.clone()],
        "[[Other Note]]"
    );
}

#[test]
fn wikilink_with_heading_block_and_alias() {
    let note = extract("[[Note#Section|shown]] [[Note#^abc123]] [[#Local Heading]]");
    assert_eq!(note.links.len(), 3);

    assert_eq!(note.links[0].target, "Note");
    assert_eq!(note.links[0].heading.as_deref(), Some("Section"));
    assert_eq!(note.links[0].alias.as_deref(), Some("shown"));

    assert_eq!(note.links[1].target, "Note");
    assert_eq!(note.links[1].block.as_deref(), Some("abc123"));
    assert_eq!(note.links[1].heading, None);

    assert_eq!(note.links[2].target, "");
    assert_eq!(note.links[2].heading.as_deref(), Some("Local Heading"));
}

#[test]
fn nested_heading_path_is_kept_verbatim() {
    let note = extract("[[Note#Outer#Inner]]");
    assert_eq!(note.links[0].heading.as_deref(), Some("Outer#Inner"));
}

#[test]
fn wiki_embed() {
    let note = extract("![[image.png]] and ![[Other#Part]]");
    assert_eq!(note.links[0].kind, LinkKind::WikiEmbed);
    assert_eq!(note.links[0].target, "image.png");
    assert_eq!(note.links[1].kind, LinkKind::WikiEmbed);
    assert_eq!(note.links[1].heading.as_deref(), Some("Part"));
}

#[test]
fn wikilink_with_path_and_alias_pipe_first_hash_second() {
    let note = extract("[[folder/Note Name|My Alias]]");
    assert_eq!(note.links[0].target, "folder/Note Name");
    assert_eq!(note.links[0].alias.as_deref(), Some("My Alias"));
}

#[test]
fn markdown_links_internal_and_external() {
    let source = "[text](Other%20Note.md) ![img](assets/pic.png) [web](https://example.com/x?q=1)";
    let note = extract(source);
    assert_eq!(note.links.len(), 3);

    assert_eq!(note.links[0].kind, LinkKind::Markdown);
    assert_eq!(note.links[0].target, "Other Note.md");
    assert_eq!(note.links[0].alias.as_deref(), Some("text"));

    assert_eq!(note.links[1].kind, LinkKind::MarkdownEmbed);
    assert_eq!(note.links[1].target, "assets/pic.png");
    assert_eq!(
        &source[note.links[1].span.clone()],
        "![img](assets/pic.png)"
    );

    assert_eq!(note.links[2].kind, LinkKind::External);
    assert_eq!(note.links[2].target, "https://example.com/x?q=1");
}

#[test]
fn markdown_link_with_fragment_and_title() {
    let note =
        extract("[t](note.md#Heading) [u](note.md#^blk) [v](<file with space.md> \"title\")");
    assert_eq!(note.links[0].heading.as_deref(), Some("Heading"));
    assert_eq!(note.links[1].block.as_deref(), Some("blk"));
    assert_eq!(note.links[2].target, "file with space.md");
}

#[test]
fn markdown_link_nested_brackets_and_parens() {
    let note = extract("[see [x] here](target(1).md)");
    assert_eq!(note.links.len(), 1);
    assert_eq!(note.links[0].target, "target(1).md");
    assert_eq!(note.links[0].alias.as_deref(), Some("see [x] here"));
}

#[test]
fn mailto_and_obsidian_schemes_are_external() {
    let note = extract("[m](mailto:a@b.c) [o](obsidian://open?vault=x)");
    assert!(note.links.iter().all(|l| l.kind == LinkKind::External));
}

#[test]
fn code_masks_links_and_tags() {
    let source = "\
`[[not a link]]` and #real-tag

```rust
[[also not a link]] #nottag
```

[[real link]]
";
    assert_eq!(link_targets(source), vec!["real link"]);
    assert_eq!(tag_names(source), vec!["real-tag"]);
}

#[test]
fn tilde_fence_and_longer_close() {
    let source = "~~~\n[[hidden]]\n~~~~\n[[shown]]";
    assert_eq!(link_targets(source), vec!["shown"]);
}

#[test]
fn fence_inside_callout_masks() {
    let source = "> ```\n> [[hidden]]\n> ```\n[[shown]]";
    assert_eq!(link_targets(source), vec!["shown"]);
}

#[test]
fn unclosed_inline_code_is_literal() {
    // A single backtick with no closing run: the wikilink after it is real.
    assert_eq!(link_targets("a ` b [[link]]"), vec!["link"]);
}

#[test]
fn double_backtick_code_span() {
    assert_eq!(link_targets("`` `[[x]]` `` [[y]]"), vec!["y"]);
}

#[test]
fn percent_comments_mask_across_lines() {
    let source = "%%\n[[hidden]] #hidden\n%%\n[[shown]] #shown";
    assert_eq!(link_targets(source), vec!["shown"]);
    assert_eq!(tag_names(source), vec!["shown"]);
}

#[test]
fn inline_percent_comment() {
    assert_eq!(link_targets("a %%[[x]]%% [[y]]"), vec!["y"]);
}

#[test]
fn html_comments_mask() {
    let source = "<!-- [[hidden]] -->\n[[shown]]\n<!--\n[[multi hidden]]\n-->";
    assert_eq!(link_targets(source), vec!["shown"]);
}

#[test]
fn escaped_syntax_is_literal() {
    assert_eq!(link_targets(r"\[[not a link]]"), Vec::<String>::new());
    assert_eq!(tag_names(r"\#nottag"), Vec::<String>::new());
}

#[test]
fn tags_basic_rules() {
    assert_eq!(
        tag_names("#tag #tag/nested #with-dash #with_underscore"),
        vec!["tag", "tag/nested", "with-dash", "with_underscore"]
    );
    // Not preceded by whitespace → not a tag.
    assert_eq!(tag_names("foo#bar"), Vec::<String>::new());
    // All digits → not a tag; mixed is fine.
    assert_eq!(tag_names("#123 #1a"), vec!["1a"]);
    // Heading marker is not a tag.
    assert_eq!(tag_names("# Heading"), Vec::<String>::new());
    // Trailing slash trimmed.
    assert_eq!(tag_names("#a/b/"), vec!["a/b"]);
    // Unicode tags work.
    assert_eq!(tag_names("#ünïcode #日本語"), vec!["ünïcode", "日本語"]);
}

#[test]
fn tag_at_line_start_and_in_heading() {
    assert_eq!(tag_names("#start of line"), vec!["start"]);
    let note = extract("## Heading with #tag");
    assert_eq!(note.tags.len(), 1);
    assert_eq!(note.headings.len(), 1);
}

#[test]
fn headings_basic() {
    let note = extract("# One\n\ntext\n\n### Three ###\n#### Four #closing not stripped\n");
    assert_eq!(note.headings.len(), 3);
    assert_eq!(note.headings[0].level, 1);
    assert_eq!(note.headings[0].text, "One");
    // Closing sequence stripped.
    assert_eq!(note.headings[1].level, 3);
    assert_eq!(note.headings[1].text, "Three");
    // `#` not preceded by space is content, not a closing sequence.
    assert_eq!(note.headings[2].text, "Four #closing not stripped");
}

#[test]
fn heading_needs_space_after_hashes() {
    let note = extract("#not-a-heading\n####### seven hashes\n");
    assert!(note.headings.is_empty());
}

#[test]
fn heading_with_link_indexes_both() {
    let note = extract("## See [[Other]]");
    assert_eq!(note.headings.len(), 1);
    assert_eq!(note.links.len(), 1);
}

#[test]
fn headings_inside_code_fence_ignored() {
    let note = extract("```\n# not a heading\n```\n# real\n");
    assert_eq!(note.headings.len(), 1);
    assert_eq!(note.headings[0].text, "real");
}

#[test]
fn frontmatter_extracted_and_excluded_from_body() {
    let source = "---\ntags: [x]\naliases: [Alt]\n---\n# Body\nword word\n";
    let note = extract(source);
    assert_eq!(note.frontmatter_tags(), vec!["x"]);
    assert_eq!(note.frontmatter_aliases(), vec!["Alt"]);
    assert_eq!(&source[note.body_range.clone()], "# Body\nword word\n");
    // `tags: [x]` must not produce an inline tag or count as words.
    assert!(note.tags.is_empty());
    assert_eq!(note.word_count, 3);
}

#[test]
fn frontmatter_dashes_inside_body_are_not_frontmatter() {
    let source = "text\n---\ntitle: x\n---\n";
    let note = extract(source);
    assert!(note.frontmatter.is_none());
    assert_eq!(note.body_range.start, 0);
}

#[test]
fn nested_wikilink_openers_recover() {
    // The scanner must find the inner link even after a stray `[[`.
    assert_eq!(link_targets("[[a [[b]]"), vec!["b"]);
}

#[test]
fn empty_and_whitespace_links_are_not_links() {
    assert_eq!(link_targets("[[]] [[ | ]] [](x) [t]()"), vec!["x"]);
}

#[test]
fn links_in_lists_and_blockquotes() {
    let source = "- item [[In List]]\n> quoted [[In Quote]]\n> [!note] callout [[In Callout]]";
    assert_eq!(
        link_targets(source),
        vec!["In List", "In Quote", "In Callout"]
    );
}

#[test]
fn multiple_links_one_line_spans_correct() {
    let source = "[[a]] mid [[b|c]] end";
    let note = extract(source);
    assert_eq!(&source[note.links[0].span.clone()], "[[a]]");
    assert_eq!(&source[note.links[1].span.clone()], "[[b|c]]");
}

#[test]
fn windows_drive_paths_are_not_external() {
    // `C:` must not be mistaken for a URI scheme.
    let note = extract("[drive](C:/notes/x.md)");
    assert_eq!(note.links[0].kind, LinkKind::Markdown);
}

#[test]
fn crlf_source_works() {
    let source = "---\r\ntags: [a]\r\n---\r\n# H\r\n[[link]] #tag\r\n";
    let note = extract(source);
    assert_eq!(note.frontmatter_tags(), vec!["a"]);
    assert_eq!(note.headings.len(), 1);
    assert_eq!(note.headings[0].text, "H");
    assert_eq!(note.links.len(), 1);
    assert_eq!(note.tags.len(), 1);
}

#[test]
fn empty_source() {
    let note = extract("");
    assert!(note.links.is_empty() && note.tags.is_empty() && note.headings.is_empty());
    assert_eq!(note.word_count, 0);
    assert_eq!(note.body_range, 0..0);
}

#[test]
fn pathological_inputs_do_not_panic() {
    // Fuzz-ish worst cases: these only assert "no panic, sane output".
    for source in [
        "[[",
        "]]",
        "[[[[[[",
        "![[",
        "[](",
        "[a](b",
        "#",
        "#/",
        "`",
        "``",
        "%%",
        "<!--",
        "\\",
        "---",
        "---\n",
        "[[a|b|c#d#^e]]",
        "![x](y \"unterminated",
    ] {
        let _ = extract(source);
    }
}

#[test]
fn alias_with_pipe_keeps_first_split_only() {
    let note = extract("[[a|b|c]]");
    assert_eq!(note.links[0].target, "a");
    assert_eq!(note.links[0].alias.as_deref(), Some("b|c"));
}
