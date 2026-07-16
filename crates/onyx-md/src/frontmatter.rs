//! YAML frontmatter parsing.
//!
//! A frontmatter block is a leading `---` line, YAML content, and a closing
//! `---` (or `...`) line. Invalid YAML means the whole block is treated as
//! body text — extraction must never fail on user input.

use serde_json::Value;

/// Parsed frontmatter as structured JSON-compatible data.
#[derive(Debug, Clone, PartialEq)]
pub struct Frontmatter {
    value: Value,
    /// Byte length of the frontmatter block including the closing delimiter
    /// line and its trailing newline.
    block_len: usize,
}

impl Frontmatter {
    /// The parsed YAML mapping as a JSON value.
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Look up a top-level key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.value.get(key)
    }

    /// `tags:` — accepts a YAML list, a comma-separated string, or a single
    /// scalar. Leading `#` is stripped; empty entries dropped.
    pub fn tags(&self) -> Vec<String> {
        self.string_list("tags")
            .into_iter()
            .map(|tag| tag.trim_start_matches('#').to_owned())
            .filter(|tag| !tag.is_empty())
            .collect()
    }

    /// `aliases:` — accepts a YAML list, a comma-separated string, or a
    /// single scalar.
    pub fn aliases(&self) -> Vec<String> {
        self.string_list("aliases")
    }

    /// Read a key that users write either as a list or a delimited string.
    fn string_list(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(value_to_string)
                .map(|item| item.trim().to_owned())
                .filter(|item| !item.is_empty())
                .collect(),
            Some(scalar) => value_to_string(scalar)
                .map(|joined| {
                    joined
                        .split(',')
                        .map(str::trim)
                        .filter(|part| !part.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Split a frontmatter block off `source`.
///
/// Returns the parsed frontmatter (if present and valid) and the byte offset
/// where the body starts.
pub(crate) fn parse(source: &str) -> (Option<Frontmatter>, usize) {
    let Some(yaml_start) = opening_delimiter_end(source) else {
        return (None, 0);
    };

    // Find the closing delimiter line: `---` or `...` alone on a line.
    let mut offset = yaml_start;
    for line in source[yaml_start..].split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" || trimmed == "..." {
            let yaml = &source[yaml_start..offset];
            let body_start = offset + line.len();
            return match serde_norway::from_str::<Value>(yaml) {
                // Frontmatter must be a mapping; `--- hello ---` is body text.
                Ok(value @ Value::Object(_)) => {
                    let frontmatter = Frontmatter {
                        value,
                        block_len: body_start,
                    };
                    (Some(frontmatter), body_start)
                }
                _ => (None, 0),
            };
        }
        offset += line.len();
    }

    (None, 0)
}

/// If the source opens a frontmatter block, return the offset just past the
/// opening `---` line.
fn opening_delimiter_end(source: &str) -> Option<usize> {
    let first_line = source.split_inclusive('\n').next()?;
    let trimmed = first_line.trim_end_matches(['\n', '\r']);
    // Must be exactly `---` and must be followed by more content.
    (trimmed == "---" && first_line.len() < source.len()).then_some(first_line.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_frontmatter() {
        let source = "---\ntitle: Hello\ntags: [a, b]\n---\nBody";
        let (frontmatter, body_start) = parse(source);
        let frontmatter = frontmatter.expect("frontmatter should parse");
        assert_eq!(frontmatter.get("title"), Some(&Value::from("Hello")));
        assert_eq!(&source[body_start..], "Body");
        assert_eq!(frontmatter.block_len, body_start);
    }

    #[test]
    fn tags_accept_list_and_string_forms() {
        let list = parse("---\ntags: [a, b/c]\n---\n").0.unwrap();
        assert_eq!(list.tags(), vec!["a", "b/c"]);

        // Unquoted ` #` starts a YAML comment, so `#c` must be quoted to survive.
        let comma = parse("---\ntags: \"a, b , #c\"\n---\n").0.unwrap();
        assert_eq!(comma.tags(), vec!["a", "b", "c"]);

        let scalar = parse("---\ntags: solo\n---\n").0.unwrap();
        assert_eq!(scalar.tags(), vec!["solo"]);

        let numeric = parse("---\ntags: [2024, plan]\n---\n").0.unwrap();
        assert_eq!(numeric.tags(), vec!["2024", "plan"]);
    }

    #[test]
    fn aliases_accept_list_and_scalar() {
        let frontmatter = parse("---\naliases: [One, \"Two Words\"]\n---\n")
            .0
            .unwrap();
        assert_eq!(frontmatter.aliases(), vec!["One", "Two Words"]);
    }

    #[test]
    fn invalid_yaml_is_body_text() {
        let source = "---\n: : bad [ yaml\n---\nBody";
        let (frontmatter, body_start) = parse(source);
        assert!(frontmatter.is_none());
        assert_eq!(body_start, 0);
    }

    #[test]
    fn non_mapping_yaml_is_body_text() {
        let (frontmatter, body_start) = parse("---\njust a string\n---\nBody");
        assert!(frontmatter.is_none());
        assert_eq!(body_start, 0);
    }

    #[test]
    fn unclosed_frontmatter_is_body_text() {
        let (frontmatter, body_start) = parse("---\ntitle: x\nno closing");
        assert!(frontmatter.is_none());
        assert_eq!(body_start, 0);
    }

    #[test]
    fn bare_dashes_document_is_not_frontmatter() {
        assert_eq!(parse("---"), (None, 0));
        assert_eq!(parse("--- not a delimiter\nx"), (None, 0));
    }

    #[test]
    fn crlf_line_endings_are_handled() {
        let source = "---\r\ntitle: Hello\r\n---\r\nBody";
        let (frontmatter, body_start) = parse(source);
        assert!(frontmatter.is_some());
        assert_eq!(&source[body_start..], "Body");
    }

    #[test]
    fn dot_closing_delimiter() {
        let (frontmatter, _) = parse("---\ntitle: x\n...\nBody");
        assert!(frontmatter.is_some());
    }
}
