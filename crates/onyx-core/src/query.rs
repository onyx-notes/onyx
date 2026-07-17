//! `onyx-query` blocks: a small, Dataview-inspired query language over the
//! index. Covers the ~60% of Dataview people actually use — list/table by
//! tag, folder, and frontmatter, with where/sort/limit — without a
//! JavaScript runtime, evaluated in Rust against the index.
//!
//! Grammar (case-insensitive keywords, one clause per line or space-
//! separated):
//!
//! ```text
//! LIST | TABLE col1, col2, ...
//! FROM #tag | "folder/" | #a and #b | #a or #b
//! WHERE field OP value        (OP: = != > < contains)
//! SORT field [ASC|DESC]
//! LIMIT n
//! ```
//!
//! `field` is `title`, `path`, `tags`, or any frontmatter key. Columns in
//! TABLE are the same field names.

use crate::index::QueryRow;

#[derive(Debug, PartialEq)]
pub enum Mode {
    List,
    Table(Vec<String>),
}

#[derive(Debug, PartialEq)]
enum Source {
    Tag(String),
    Folder(String),
    All,
}

#[derive(Debug, PartialEq)]
enum SourceExpr {
    Single(Source),
    And(Vec<Source>),
    Or(Vec<Source>),
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Op {
    Eq,
    Ne,
    Gt,
    Lt,
    Contains,
}

#[derive(Debug, PartialEq)]
struct Condition {
    field: String,
    op: Op,
    value: String,
}

#[derive(Debug, PartialEq)]
pub struct Query {
    pub mode: Mode,
    source: SourceExpr,
    conditions: Vec<Condition>,
    sort: Option<(String, bool)>, // (field, ascending)
    limit: Option<usize>,
}

/// The rendered result: column headers + rows of cell strings. The first
/// column of every result is the note (path) so the UI can link it.
#[derive(Debug, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

pub fn parse(source: &str) -> Result<Query, String> {
    let mut mode: Option<Mode> = None;
    let mut src = SourceExpr::Single(Source::All);
    let mut conditions = Vec::new();
    let mut sort = None;
    let mut limit = None;

    // Clauses may be newline- OR space-separated; split on keyword tokens
    // wherever they appear so `list from #x where y` works on one line.
    for (keyword, rest) in split_clauses(source)? {
        match keyword.as_str() {
            "list" => mode = Some(Mode::List),
            "table" => {
                let cols = rest
                    .split(',')
                    .map(|c| c.trim().to_owned())
                    .filter(|c| !c.is_empty())
                    .collect();
                mode = Some(Mode::Table(cols));
            }
            "from" => src = parse_source(&rest)?,
            "where" => conditions.push(parse_condition(&rest)?),
            "sort" => {
                let mut parts = rest.split_whitespace();
                let field = parts.next().ok_or("sort needs a field")?.to_owned();
                let ascending = !matches!(
                    parts.next().map(|d| d.to_lowercase()),
                    Some(ref d) if d == "desc"
                );
                sort = Some((field, ascending));
            }
            "limit" => {
                limit = Some(
                    rest.trim()
                        .parse()
                        .map_err(|_| format!("invalid limit: {rest}"))?,
                );
            }
            other => return Err(format!("unknown clause: {other}")),
        }
    }

    Ok(Query {
        mode: mode.ok_or("query must start with LIST or TABLE")?,
        source: src,
        conditions,
        sort,
        limit,
    })
}

const KEYWORDS: &[&str] = &["list", "table", "from", "where", "sort", "limit"];

/// Split a query into `(keyword, rest)` clauses. A keyword token (outside
/// quotes) starts a new clause; content before the first keyword errors.
fn split_clauses(source: &str) -> Result<Vec<(String, String)>, String> {
    let normalized = source.replace(['\n', '\r'], " ");

    // Tokenize, keeping quoted spans (folder "a b/") intact.
    let mut tokens: Vec<(String, bool)> = Vec::new(); // (token, was_quoted)
    let mut word = String::new();
    let mut in_quote = false;
    let mut quoted = false;
    for c in normalized.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                quoted = true;
                word.push(c);
            }
            c if c.is_whitespace() && !in_quote => {
                if !word.is_empty() {
                    tokens.push((std::mem::take(&mut word), quoted));
                    quoted = false;
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        tokens.push((word, quoted));
    }

    let mut clauses: Vec<(String, String)> = Vec::new();
    for (token, was_quoted) in tokens {
        let lowered = token.to_lowercase();
        if !was_quoted && KEYWORDS.contains(&lowered.as_str()) {
            clauses.push((lowered, String::new()));
        } else {
            let clause = clauses
                .last_mut()
                .ok_or_else(|| format!("query must start with a keyword, got: {token}"))?;
            if !clause.1.is_empty() {
                clause.1.push(' ');
            }
            clause.1.push_str(&token);
        }
    }
    for clause in &mut clauses {
        clause.1 = clause.1.trim().to_owned();
    }
    Ok(clauses)
}

fn parse_source(rest: &str) -> Result<SourceExpr, String> {
    let lower = rest.to_lowercase();
    let joiner = if lower.contains(" or ") {
        Some(false)
    } else if lower.contains(" and ") {
        Some(true)
    } else {
        None
    };
    let sep = if joiner == Some(false) {
        " or "
    } else {
        " and "
    };
    let sources: Result<Vec<Source>, String> = split_ci(rest, sep)
        .into_iter()
        .map(|token| parse_one_source(token.trim()))
        .collect();
    let sources = sources?;
    Ok(match joiner {
        Some(true) => SourceExpr::And(sources),
        Some(false) => SourceExpr::Or(sources),
        None => SourceExpr::Single(sources.into_iter().next().unwrap_or(Source::All)),
    })
}

fn parse_one_source(token: &str) -> Result<Source, String> {
    if let Some(tag) = token.strip_prefix('#') {
        Ok(Source::Tag(tag.to_lowercase()))
    } else if token.starts_with('"') && token.ends_with('"') && token.len() >= 2 {
        Ok(Source::Folder(
            token[1..token.len() - 1].trim_matches('/').to_lowercase(),
        ))
    } else if token.eq_ignore_ascii_case("all") || token.is_empty() {
        Ok(Source::All)
    } else {
        Err(format!("bad source: {token} (use #tag or \"folder/\")"))
    }
}

fn parse_condition(rest: &str) -> Result<Condition, String> {
    // Longest operators first so "!=" isn't read as "=".
    for (symbol, op) in [
        ("!=", Op::Ne),
        (" contains ", Op::Contains),
        (">", Op::Gt),
        ("<", Op::Lt),
        ("=", Op::Eq),
    ] {
        if let Some((field, value)) = rest.split_once(symbol) {
            return Ok(Condition {
                field: field.trim().to_lowercase(),
                op,
                value: value.trim().trim_matches('"').to_owned(),
            });
        }
    }
    Err(format!("bad where clause: {rest}"))
}

/// Split on `sep` case-insensitively (for and/or joiners).
fn split_ci(text: &str, sep: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut search = 0;
    while let Some(at) = lower[search..].find(sep) {
        let index = search + at;
        parts.push(text[start..index].to_owned());
        start = index + sep.len();
        search = start;
    }
    parts.push(text[start..].to_owned());
    parts
}

pub fn execute(query: &Query, rows: &[QueryRow]) -> QueryResult {
    let mut matched: Vec<&QueryRow> = rows
        .iter()
        .filter(|row| matches_source(&query.source, row))
        .filter(|row| {
            query
                .conditions
                .iter()
                .all(|cond| matches_condition(cond, row))
        })
        .collect();

    if let Some((field, ascending)) = &query.sort {
        matched.sort_by(|a, b| {
            let ord = field_value(a, field).cmp(&field_value(b, field));
            if *ascending { ord } else { ord.reverse() }
        });
    }
    if let Some(limit) = query.limit {
        matched.truncate(limit);
    }

    let columns = match &query.mode {
        Mode::List => vec!["note".to_owned()],
        Mode::Table(cols) => {
            let mut all = vec!["note".to_owned()];
            all.extend(cols.iter().cloned());
            all
        }
    };
    let rows = matched
        .iter()
        .map(|row| match &query.mode {
            Mode::List => vec![row.path.clone()],
            Mode::Table(cols) => {
                let mut cells = vec![row.path.clone()];
                cells.extend(cols.iter().map(|col| field_value(row, col)));
                cells
            }
        })
        .collect();

    QueryResult { columns, rows }
}

/// One-shot parse + execute.
pub fn run_query(source: &str, rows: &[QueryRow]) -> Result<QueryResult, String> {
    Ok(execute(&parse(source)?, rows))
}

fn matches_source(source: &SourceExpr, row: &QueryRow) -> bool {
    match source {
        SourceExpr::Single(s) => matches_one(s, row),
        SourceExpr::And(sources) => sources.iter().all(|s| matches_one(s, row)),
        SourceExpr::Or(sources) => sources.iter().any(|s| matches_one(s, row)),
    }
}

fn matches_one(source: &Source, row: &QueryRow) -> bool {
    match source {
        Source::All => true,
        Source::Tag(tag) => row.tags.iter().any(|t| {
            let t = t.to_lowercase();
            // A tag matches itself and any of its descendants (project
            // matches project/onyx), like Obsidian.
            t == *tag || t.starts_with(&format!("{tag}/"))
        }),
        Source::Folder(folder) => {
            let path = row.path.to_lowercase();
            folder.is_empty() || path.starts_with(&format!("{folder}/"))
        }
    }
}

fn matches_condition(cond: &Condition, row: &QueryRow) -> bool {
    let actual = field_value(row, &cond.field);
    let value = &cond.value;
    match cond.op {
        Op::Eq => actual.eq_ignore_ascii_case(value),
        Op::Ne => !actual.eq_ignore_ascii_case(value),
        Op::Contains => actual.to_lowercase().contains(&value.to_lowercase()),
        Op::Gt | Op::Lt => match (actual.parse::<f64>(), value.parse::<f64>()) {
            (Ok(a), Ok(b)) => {
                if cond.op == Op::Gt {
                    a > b
                } else {
                    a < b
                }
            }
            // Fall back to lexicographic for non-numeric values.
            _ => {
                if cond.op == Op::Gt {
                    actual.as_str() > value.as_str()
                } else {
                    actual.as_str() < value.as_str()
                }
            }
        },
    }
}

fn field_value(row: &QueryRow, field: &str) -> String {
    match field.to_lowercase().as_str() {
        "note" | "path" => row.path.clone(),
        "title" => row.title.clone(),
        "tags" => row.tags.join(", "),
        other => row
            .frontmatter
            .get(other)
            .map(|raw| raw.trim_matches('"').to_owned())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn row(path: &str, title: &str, tags: &[&str], fm: &[(&str, &str)]) -> QueryRow {
        QueryRow {
            path: path.into(),
            title: title.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            frontmatter: fm
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
        }
    }

    fn corpus() -> Vec<QueryRow> {
        vec![
            row(
                "projects/onyx.md",
                "Onyx",
                &["project/onyx", "status/wip"],
                &[("priority", "1")],
            ),
            row(
                "projects/side.md",
                "Side",
                &["project"],
                &[("priority", "3")],
            ),
            row("journal/day.md", "Day", &["journal"], &[("priority", "2")]),
            row("inbox/idea.md", "Idea", &[], &[]),
        ]
    }

    #[test]
    fn list_from_tag_matches_descendants() {
        let result = run_query("list from #project", &corpus()).unwrap();
        assert_eq!(result.columns, vec!["note"]);
        // project matches both project and project/onyx.
        let paths: Vec<&String> = result.rows.iter().map(|r| &r[0]).collect();
        assert!(paths.contains(&&"projects/onyx.md".to_string()));
        assert!(paths.contains(&&"projects/side.md".to_string()));
        assert_eq!(result.rows.len(), 2);
    }

    #[test]
    fn table_with_columns_and_frontmatter() {
        let result = run_query("table title, priority from #project", &corpus()).unwrap();
        assert_eq!(result.columns, vec!["note", "title", "priority"]);
        let onyx = result
            .rows
            .iter()
            .find(|r| r[0] == "projects/onyx.md")
            .unwrap();
        assert_eq!(onyx[1], "Onyx");
        assert_eq!(onyx[2], "1");
    }

    #[test]
    fn folder_source() {
        let result = run_query("list from \"journal/\"", &corpus()).unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][0], "journal/day.md");
    }

    #[test]
    fn where_sort_limit() {
        let result = run_query(
            "table priority\nfrom all\nwhere priority > 1\nsort priority desc\nlimit 1",
            &corpus(),
        )
        .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0][1], "3"); // highest priority, limited to 1
    }

    #[test]
    fn and_or_sources() {
        let and = run_query("list from #project and #status/wip", &corpus()).unwrap();
        assert_eq!(and.rows.len(), 1);
        assert_eq!(and.rows[0][0], "projects/onyx.md");

        let or = run_query("list from #journal or #status/wip", &corpus()).unwrap();
        assert_eq!(or.rows.len(), 2);
    }

    #[test]
    fn where_contains_and_ne() {
        let contains = run_query("list from all where title contains day", &corpus()).unwrap();
        assert_eq!(contains.rows.len(), 1);
        let ne = run_query("list from #project where title != Onyx", &corpus()).unwrap();
        assert_eq!(ne.rows.len(), 1);
        assert_eq!(ne.rows[0][0], "projects/side.md");
    }

    #[test]
    fn parse_errors_are_helpful() {
        assert!(run_query("from #x", &corpus()).is_err()); // no LIST/TABLE
        assert!(run_query("list from bad-source", &corpus()).is_err());
        assert!(run_query("list from all where", &corpus()).is_err());
        assert!(run_query("frobnicate", &corpus()).is_err());
    }

    #[test]
    fn empty_result_is_valid() {
        let result = run_query("list from #nonexistent", &corpus()).unwrap();
        assert!(result.rows.is_empty());
        assert_eq!(result.columns, vec!["note"]);
    }
}
