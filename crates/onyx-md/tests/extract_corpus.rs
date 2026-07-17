//! Obsidian-compatibility corpus: golden snapshots over real-world vault
//! patterns. The "opens your Obsidian vault in place" promise is
//! CI-enforced here — a regression in link/tag/heading extraction on any
//! of these fixtures fails the build.

use std::fmt::Write;

use onyx_md::extract;

/// Render an extraction to a stable, reviewable text form.
fn describe(source: &str) -> String {
    let note = extract(source);
    let mut out = String::new();
    writeln!(out, "frontmatter_tags: {:?}", note.frontmatter_tags()).unwrap();
    writeln!(out, "frontmatter_aliases: {:?}", note.frontmatter_aliases()).unwrap();
    writeln!(out, "word_count: {}", note.word_count).unwrap();
    writeln!(out, "headings:").unwrap();
    for heading in &note.headings {
        writeln!(out, "  H{} {:?}", heading.level, heading.text).unwrap();
    }
    writeln!(out, "tags:").unwrap();
    for tag in &note.tags {
        writeln!(out, "  #{}", tag.tag).unwrap();
    }
    writeln!(out, "links:").unwrap();
    for link in &note.links {
        writeln!(
            out,
            "  {:?} target={:?} heading={:?} block={:?} alias={:?}",
            link.kind, link.target, link.heading, link.block, link.alias
        )
        .unwrap();
    }
    out
}

macro_rules! corpus_test {
    ($name:ident, $file:literal) => {
        #[test]
        fn $name() {
            let source = include_str!(concat!("corpus/", $file));
            insta::assert_snapshot!($file, describe(source));
        }
    };
}

corpus_test!(callouts, "callouts.md");
corpus_test!(block_refs, "block-refs.md");
corpus_test!(nested_embeds, "nested-embeds.md");
corpus_test!(messy_frontmatter, "messy-frontmatter.md");
corpus_test!(mixed_links, "mixed-links.md");
corpus_test!(code_heavy, "code-heavy.md");
corpus_test!(dataview_ish, "dataview-ish.md");
