//! One model-facing entry point for bounded text and document operations.
use crate::{documents, files::parse, paths};
use serde::Deserialize;
use serde_json::json;
use std::io;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    unit: usize,
    offset: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    cursor: Option<Cursor>,
    // Compatibility with saved calls; only cursor is advertised.
    unit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    path: String,
    query: String,
    #[serde(default)]
    offset: usize,
}

pub(crate) fn read(scope: paths::Scope<'_>, arguments: &str) -> io::Result<String> {
    let input: ReadInput = parse(arguments)?;
    if input.cursor.is_some() && (input.unit.is_some() || input.offset.is_some()) {
        return Err(io::Error::other(
            "use cursor without legacy unit/offset fields",
        ));
    }
    let cursor = input.cursor.unwrap_or(Cursor {
        unit: input.unit.unwrap_or(1),
        offset: input.offset.unwrap_or(0),
    });
    let target = scope.resolve(&input.path)?;
    if documents::supported(&target) {
        return documents::read(scope, &input.path, cursor.unit, cursor.offset);
    }
    if cursor.unit != 1 {
        return Err(io::Error::other("text files use cursor unit 1"));
    }
    let content = paths::read_text(&target, paths::READ_LIMIT)?;
    if cursor.offset > content.chars().count() {
        return Err(io::Error::other(
            "cursor is outside this file; use next_cursor",
        ));
    }
    let mut chars = content.chars().skip(cursor.offset).peekable();
    let chunk: String = chars.by_ref().take(8192).collect();
    let next = chars.peek().map(|_| cursor.offset + chunk.chars().count());
    let line = content
        .chars()
        .take(cursor.offset)
        .filter(|ch| *ch == '\n')
        .count()
        + 1;
    Ok(
        json!({"path":input.path,"format":"text","content":chunk,"line":line,
        "offset":cursor.offset,"next_offset":next,
        "next_cursor":next.map(|offset|json!({"unit":1,"offset":offset}))})
        .to_string(),
    )
}

pub(crate) fn search(scope: paths::Scope<'_>, arguments: &str) -> io::Result<String> {
    let input: SearchInput = parse(arguments)?;
    let target = scope.resolve(&input.path)?;
    // A directory with a document-like suffix must still be a bounded text scan.
    if target.is_file() && documents::supported(&target) {
        return documents::search(scope, &input.path, &input.query, input.offset);
    }
    Ok(crate::search::search(scope, &input.path, &input.query, input.offset)?.to_string())
}
