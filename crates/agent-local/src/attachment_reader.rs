//! Bounded reads of immutable attachment bytes. The host authenticates the
//! attachment reference; this module never resolves names as filesystem paths.
use crate::{documents, paths};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{io, path::Path};

const MAX_ATTACHMENT_BYTES: usize = 100 * 1024 * 1024;
const TEXT_CHUNK_CHARS: usize = 8192;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    unit: usize,
    offset: usize,
}

/// Read one bounded text page. Document cursors identify source units such as
/// PDF pages, paragraphs or sheets; plain text always uses unit 1.
pub fn read_attachment(name: &str, bytes: &[u8], cursor: Option<&Value>) -> io::Result<Value> {
    validate_size(bytes)?;
    let cursor = cursor
        .map(|value| serde_json::from_value::<Cursor>(value.clone()))
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
        .unwrap_or(Cursor { unit: 1, offset: 0 });
    if documents::supported(Path::new(name)) {
        let document = documents::extract_document(name, bytes)?;
        return documents::read_page(&document, name, cursor.unit, cursor.offset);
    }
    if cursor.unit != 1 {
        return Err(io::Error::other("text files use cursor unit 1"));
    }
    let content = text_content(bytes)?;
    let mut chars = content.chars();
    let mut line = 1;
    for _ in 0..cursor.offset {
        match chars.next() {
            Some('\n') => line += 1,
            Some(_) => {}
            None => {
                return Err(io::Error::other(
                    "cursor is outside this file; use next_cursor",
                ));
            }
        }
    }
    let mut chars = chars.peekable();
    let chunk: String = chars.by_ref().take(TEXT_CHUNK_CHARS).collect();
    let next = chars.peek().map(|_| cursor.offset + chunk.chars().count());
    Ok(json!({
        "path":name,"format":"text","content":chunk,"line":line,
        "offset":cursor.offset,"next_offset":next,
        "next_cursor":next.map(|offset|json!({"unit":1,"offset":offset}))
    }))
}

/// Search one attachment without adding its full contents to the model input.
/// Results contain at most 50 matching lines and a continuation offset.
pub fn search_attachment(
    name: &str,
    bytes: &[u8],
    query: &str,
    offset: usize,
) -> io::Result<Value> {
    validate_size(bytes)?;
    documents::validate_query(query)?;
    if documents::supported(Path::new(name)) {
        let document = documents::extract_document(name, bytes)?;
        return documents::search_page(&document, name, query, offset);
    }
    let content = text_content(bytes)?;
    let mut candidates = content
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(query))
        .skip(offset)
        .peekable();
    let matches: Vec<_> = candidates
        .by_ref()
        .take(50)
        .map(|(line, text)| json!({"path":name,"line":line+1,"text":paths::bounded(text,512)}))
        .collect();
    let next = candidates.peek().map(|_| offset + matches.len());
    Ok(json!({
        "path":name,"format":"text","matches":matches,"next_offset":next,
        "coverage":{"complete":next.is_none(),"scanned_files":1,"scanned_bytes":bytes.len(),"skipped_count":0,"skipped":[]}
    }))
}

fn validate_size(bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(io::Error::other(
            "attachment exceeds the 100 MiB read limit",
        ));
    }
    Ok(())
}

fn text_content(bytes: &[u8]) -> io::Result<&str> {
    let text = std::str::from_utf8(bytes).map_err(|_| unsupported())?;
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(unsupported());
    }
    if bytes.starts_with(b"%PDF-") {
        return Err(io::Error::other(
            "this attachment contains PDF bytes but has no .pdf extension; text is not safely readable",
        ));
    }
    Ok(text)
}

fn unsupported() -> io::Error {
    io::Error::other(
        "attachment is binary or not UTF-8; text extraction supports UTF-8, PDF, DOCX, XLSX and PPTX. The original file is retained; use a suitable parser or conversion tool for this format and do not infer its contents from the filename",
    )
}
