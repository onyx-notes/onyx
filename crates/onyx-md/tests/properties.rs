//! Property tests: extraction must be total and internally consistent for
//! ANY input — notes are arbitrary user text.

use onyx_md::extract;
use proptest::prelude::*;

proptest! {
    /// Extraction never panics and every reported span is a valid,
    /// in-bounds, char-boundary-aligned slice of the source.
    #[test]
    fn extraction_is_total_and_spans_are_valid(source in "\\PC*") {
        let note = extract(&source);

        for span in note
            .links
            .iter()
            .map(|l| l.span.clone())
            .chain(note.tags.iter().map(|t| t.span.clone()))
            .chain(note.headings.iter().map(|h| h.span.clone()))
        {
            prop_assert!(span.start <= span.end);
            prop_assert!(span.end <= source.len());
            prop_assert!(source.is_char_boundary(span.start));
            prop_assert!(source.is_char_boundary(span.end));
        }

        prop_assert!(note.body_range.end == source.len());
        prop_assert!(note.body_range.start <= source.len());
        prop_assert!(source.is_char_boundary(note.body_range.start));
    }

    /// Markdown-looking documents (with newlines, brackets, hashes) also
    /// never panic — denser grammar-shaped fuzzing than \\PC*.
    #[test]
    fn markdown_shaped_inputs_are_total(
        source in r"(?s)([-#\[\]()|^`%!>\\ \nа-яa-z0-9]{0,200})"
    ) {
        let _ = extract(&source);
    }

    /// Wikilinks we generate are always found with the right target.
    #[test]
    fn generated_wikilinks_roundtrip(name in "[a-zA-Z][a-zA-Z0-9 _-]{0,30}") {
        let source = format!("prefix [[{name}]] suffix");
        let note = extract(&source);
        prop_assert_eq!(note.links.len(), 1);
        prop_assert_eq!(note.links[0].target.as_str(), name.trim());
    }
}
