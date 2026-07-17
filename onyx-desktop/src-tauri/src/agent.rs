//! The Vault Assistant agent: a tool-using loop that can read and search
//! the vault and *propose* edits — but never mutates anything itself.
//!
//! Hard invariant (the plan's safety rule): tools are read-only plus
//! `propose_*`. Proposals accumulate into a changeset the user reviews as
//! diffs and applies atomically. The model can plan and gather all it
//! wants; the only path to disk is the user pressing Apply.

use serde::{Deserialize, Serialize};

/// One proposed change to a note.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Proposal {
    /// Replace a note's entire content (or create it).
    Write { path: String, content: String },
    /// Delete a note.
    Delete { path: String },
}

impl Proposal {
    pub fn path(&self) -> &str {
        match self {
            Proposal::Write { path, .. } | Proposal::Delete { path } => path,
        }
    }
}

/// A tool call the model may request.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "tool")]
pub enum ToolCall {
    /// Full-text search; returns matching paths.
    SearchVault { query: String },
    /// Read a note's content.
    ReadNote { path: String },
    /// List all note paths.
    ListNotes,
    /// Propose writing/creating a note (accumulates, does not apply).
    ProposeWrite { path: String, content: String },
    /// Propose deleting a note.
    ProposeDelete { path: String },
    /// Finish: no more tools, return this message to the user.
    Finish { message: String },
}

/// The tool-catalog description handed to the model in the system prompt.
pub const TOOL_SPEC: &str = r#"You are the Onyx Vault Assistant. You work in STRICT JSON tool calls.

Every reply MUST be a single JSON object, nothing else, one of:
  {"tool":"search_vault","query":"..."}
  {"tool":"read_note","path":"..."}
  {"tool":"list_notes"}
  {"tool":"propose_write","path":"...","content":"...full new content..."}
  {"tool":"propose_delete","path":"..."}
  {"tool":"finish","message":"summary for the user"}

You CANNOT modify files directly. propose_* calls are collected into a
changeset the user reviews and applies. Gather context with read/search
first. When done proposing, call finish. Keep proposals minimal and
correct; include the COMPLETE new note content in propose_write."#;

/// Result of executing a read-only tool, fed back to the model.
#[derive(Debug, Serialize)]
pub struct ToolResult {
    pub output: String,
}

/// Parse the model's JSON reply into a tool call. Tolerant of code fences
/// and surrounding prose (models stray); extracts the first JSON object.
pub fn parse_tool_call(reply: &str) -> Result<ToolCall, String> {
    let json = extract_json_object(reply).ok_or("no JSON object in model reply")?;
    serde_json::from_str(json).map_err(|error| format!("invalid tool call: {error}"))
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in text[start..].bytes().enumerate() {
        match byte {
            b'"' if !escaped => in_string = !in_string,
            b'\\' if in_string => {
                escaped = !escaped;
                continue;
            }
            b'{' if !in_string => depth += 1,
            b'}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + offset + 1]);
                }
            }
            _ => {}
        }
        escaped = false;
    }
    None
}

/// The evolving changeset the UI renders and applies.
#[derive(Debug, Default, Serialize)]
pub struct Changeset {
    pub proposals: Vec<Proposal>,
    pub log: Vec<String>,
    pub finished: Option<String>,
}

impl Changeset {
    pub fn add(&mut self, proposal: Proposal) {
        // A later proposal for the same path supersedes an earlier one.
        self.proposals
            .retain(|existing| existing.path() != proposal.path());
        self.proposals.push(proposal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_and_fenced_and_prosey_json() {
        assert!(matches!(
            parse_tool_call(r#"{"tool":"list_notes"}"#).unwrap(),
            ToolCall::ListNotes
        ));
        assert!(matches!(
            parse_tool_call("```json\n{\"tool\":\"read_note\",\"path\":\"a.md\"}\n```").unwrap(),
            ToolCall::ReadNote { path } if path == "a.md"
        ));
        assert!(matches!(
            parse_tool_call("Sure! {\"tool\":\"search_vault\",\"query\":\"x\"} done").unwrap(),
            ToolCall::SearchVault { query } if query == "x"
        ));
    }

    #[test]
    fn extracts_object_with_nested_braces_and_strings() {
        let reply =
            r##"{"tool":"propose_write","path":"a.md","content":"# H\n{not json} \"quote\""}"##;
        match parse_tool_call(reply).unwrap() {
            ToolCall::ProposeWrite { path, content } => {
                assert_eq!(path, "a.md");
                assert!(content.contains("{not json}"));
                assert!(content.contains('"'));
            }
            other => panic!("wrong tool: {other:?}"),
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_tool_call("no json here").is_err());
        assert!(parse_tool_call(r#"{"tool":"unknown"}"#).is_err());
    }

    #[test]
    fn changeset_supersedes_same_path() {
        let mut changeset = Changeset::default();
        changeset.add(Proposal::Write {
            path: "a.md".into(),
            content: "v1".into(),
        });
        changeset.add(Proposal::Write {
            path: "a.md".into(),
            content: "v2".into(),
        });
        changeset.add(Proposal::Write {
            path: "b.md".into(),
            content: "x".into(),
        });
        assert_eq!(changeset.proposals.len(), 2);
        assert_eq!(
            changeset.proposals[0],
            Proposal::Write {
                path: "a.md".into(),
                content: "v2".into()
            }
        );
    }
}
