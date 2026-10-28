//! Built-in document text extraction. No macros, formulas, embedded scripts or
//! external references are executed. Callers must enforce access before passing bytes.
mod office;
mod pdf;

use crate::{files::parse, paths};
use agent_runtime::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{io, path::Path};

pub const MAX_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
pub(super) const TEXT_LIMIT: usize = 8 * 1024 * 1024;
pub(super) const UNIT_LIMIT: usize = 20_000;
const CHUNK_CHARS: usize = 6000;
// Leave room for the envelope, path and extraction warnings. JSON escaping and
// source locations count toward this bound, not only extracted characters.
const PAGE_BYTES: usize = 48 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct DocumentUnit {
    /// Human-readable source location (page, paragraph, slide or named sheet).
    pub location: String,
    pub text: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct ExtractedDocument {
    pub format: String,
    pub units: Vec<DocumentUnit>,
    pub warnings: Vec<String>,
    #[serde(skip)]
    text_bytes: usize,
}
impl ExtractedDocument {
    fn new(format: &str) -> Self {
        Self {
            format: format.into(),
            units: vec![],
            warnings: vec![],
            text_bytes: 0,
        }
    }
    fn push(&mut self, location: String, text: String) -> io::Result<()> {
        if self.units.len() >= UNIT_LIMIT || self.text_bytes.saturating_add(text.len()) > TEXT_LIMIT
        {
            return Err(io::Error::other(
                "document exceeds 8 MiB extracted text or 20000 source units; split the document",
            ));
        }
        self.text_bytes += text.len();
        self.units.push(DocumentUnit { location, text });
        Ok(())
    }
}

pub fn supported(path: &Path) -> bool {
    matches!(extension(path).as_str(), "pdf" | "docx" | "xlsx" | "pptx")
}
fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Extract bounded text from explicitly admitted bytes, e.g. a user attachment.
/// Does not grant filesystem access. Password-protected documents require an
/// unencrypted copy; image-only pages require OCR, which is not performed here.
pub fn extract_document(name: &str, bytes: &[u8]) -> io::Result<ExtractedDocument> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(io::Error::other("document exceeds 32 MiB input limit"));
    }
    let format = extension(Path::new(name));
    std::panic::catch_unwind(|| match format.as_str() {
        "pdf" => pdf::extract(bytes),
        "docx" | "xlsx" | "pptx" => office::extract(&format, bytes),
        _ => Err(io::Error::other("unsupported document format; supported: PDF, DOCX, XLSX, PPTX. Use read_file for UTF-8 text.")),
    }).map_err(|_| io::Error::other("document parser rejected malformed content; re-export this document"))?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    #[serde(default = "first_unit")]
    unit: usize,
    #[serde(default)]
    offset: usize,
}
fn first_unit() -> usize {
    1
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    path: String,
    query: String,
    #[serde(default)]
    offset: usize,
}
fn load(root: &Path, path: &str) -> io::Result<ExtractedDocument> {
    let target = paths::resolve(root, path)?;
    extract_document(path, &paths::read_bytes(&target, MAX_DOCUMENT_BYTES)?)
}
pub(crate) fn call(root: &Path, call: &ToolCall) -> io::Result<String> {
    if call.name == "read_document" {
        let input: ReadInput = parse(&call.arguments)?;
        read(root, &input.path, input.unit, input.offset)
    } else {
        let input: SearchInput = parse(&call.arguments)?;
        search(root, &input.path, &input.query, input.offset)
    }
}

pub(crate) fn read(root: &Path, path: &str, unit: usize, offset: usize) -> io::Result<String> {
    read_page(&load(root, path)?, path, unit, offset).map(|value| value.to_string())
}

/// Page already admitted document bytes without resolving a filesystem path.
pub(crate) fn read_page(
    document: &ExtractedDocument,
    path: &str,
    unit: usize,
    offset: usize,
) -> io::Result<Value> {
    let input = ReadInput {
        path: path.into(),
        unit,
        offset,
    };
    if input.unit == 0 || input.unit > document.units.len().max(1) {
        return Err(io::Error::other("unit is outside this document"));
    }
    let unit_length = document
        .units
        .get(input.unit - 1)
        .map_or(0, |unit| unit.text.chars().count());
    if input.offset > unit_length {
        return Err(io::Error::other(
            "offset is outside this document unit; use the returned next_cursor",
        ));
    }
    let mut chunks = vec![];
    let mut budget = CHUNK_CHARS;
    let mut page_bytes = 0;
    let mut next = None;
    for (index, unit) in document.units.iter().enumerate().skip(input.unit - 1) {
        let offset = if index == input.unit - 1 {
            input.offset
        } else {
            0
        };
        let mut chars = unit.text.chars().skip(offset).peekable();
        let text: String = chars.by_ref().take(budget).collect();
        let count = text.chars().count();
        let chunk = json!({"unit":index+1,"location":paths::bounded(&unit.location,1024),"offset":offset,"text":text});
        let bytes = chunk.to_string().len() + 1;
        if page_bytes + bytes > PAGE_BYTES {
            next = Some(json!({"unit":index+1,"offset":offset}));
            break;
        }
        page_bytes += bytes;
        budget -= count;
        chunks.push(chunk);
        if chars.peek().is_some() {
            next = Some(json!({"unit":index+1,"offset":offset+count}));
            break;
        }
        if budget == 0 || chunks.len() == 32 {
            if index + 1 < document.units.len() {
                next = Some(json!({"unit":index+2,"offset":0}));
            }
            break;
        }
    }
    Ok(
        json!({"path":input.path,"format":document.format,"unit_count":document.units.len(),"chunks":chunks,"next_cursor":next,"warnings":document.warnings}),
    )
}

pub(crate) fn search(root: &Path, path: &str, query: &str, offset: usize) -> io::Result<String> {
    validate_query(query)?;
    search_page(&load(root, path)?, path, query, offset).map(|value| value.to_string())
}

pub(crate) fn validate_query(query: &str) -> io::Result<()> {
    if query.is_empty() || query.len() > 1024 {
        return Err(io::Error::other("query must contain 1 to 1024 bytes"));
    }
    Ok(())
}

pub(crate) fn search_page(
    document: &ExtractedDocument,
    path: &str,
    query: &str,
    offset: usize,
) -> io::Result<Value> {
    validate_query(query)?;
    let input = SearchInput {
        path: path.into(),
        query: query.into(),
        offset,
    };
    let query = &input.query;
    let candidates = document
        .units
        .iter()
        .enumerate()
        .flat_map(|(index, unit)| {
            unit.text
                .lines()
                .enumerate()
                .filter_map(move |(line, text)| {
                    text.contains(query).then_some((index, unit, line, text))
                })
        })
        .skip(input.offset);
    let mut matches = Vec::new();
    let mut bytes = 0;
    let mut next = None;
    for (index, unit, line, text) in candidates {
        let entry = json!({"unit":index+1,"location":paths::bounded(&unit.location,1024),"line":line+1,"text":paths::bounded(text,512)});
        let size = entry.to_string().len() + 1;
        if matches.len() == 50 || bytes + size > PAGE_BYTES {
            next = Some(input.offset + matches.len());
            break;
        }
        bytes += size;
        matches.push(entry);
    }
    Ok(
        json!({"path":input.path,"format":document.format,"matches":matches,"next_offset":next,"warnings":document.warnings}),
    )
}
