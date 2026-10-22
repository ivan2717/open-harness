//! Built-in document text extraction. No macros, formulas, embedded scripts or
//! external references are executed. Callers must enforce access before passing bytes.
mod office;
mod pdf;

use crate::{files::parse, paths};
use agent_runtime::{ToolCall, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{io, path::Path};

pub const MAX_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;
pub(super) const TEXT_LIMIT: usize = 8 * 1024 * 1024;
pub(super) const UNIT_LIMIT: usize = 20_000;
const CHUNK_CHARS: usize = 6000;

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

pub(crate) fn definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition { name:"read_document".into(), description:"Read PDF, DOCX, XLSX or PPTX in read-only mode without running commands. Returns bounded text with page/paragraph/slide/sheet locations. Start with unit=1, offset=0; pass next_cursor fields to continue. Unit numbers are 1-based; offsets count characters. Does not perform OCR or execute macros/formulas.".into(), input_schema:json!({"type":"object","properties":{"path":{"type":"string"},"unit":{"type":"integer","minimum":1},"offset":{"type":"integer","minimum":0}},"required":["path"],"additionalProperties":false}).to_string() },
        ToolDefinition { name:"search_document".into(), description:"Search literal text inside a PDF, DOCX, XLSX or PPTX. Returns bounded excerpts and page/paragraph/slide/sheet locations. offset paginates matching lines. Search is case-sensitive; text extraction cannot search image-only scans.".into(), input_schema:json!({"type":"object","properties":{"path":{"type":"string"},"query":{"type":"string"},"offset":{"type":"integer","minimum":0}},"required":["path","query"],"additionalProperties":false}).to_string() },
    ]
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
        let document = load(root, &input.path)?;
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
            budget -= count;
            chunks
                .push(json!({"unit":index+1,"location":unit.location,"offset":offset,"text":text}));
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
        Ok(json!({"path":input.path,"format":document.format,"unit_count":document.units.len(),"chunks":chunks,"next_cursor":next,"warnings":document.warnings}).to_string())
    } else {
        let input: SearchInput = parse(&call.arguments)?;
        if input.query.is_empty() || input.query.len() > 1024 {
            return Err(io::Error::other("query must contain 1 to 1024 bytes"));
        }
        let document = load(root, &input.path)?;
        let query = &input.query;
        let matches: Vec<_> = document
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
            .skip(input.offset)
            .take(51)
            .collect();
        let next = (matches.len() > 50).then(|| input.offset + 50);
        let matches:Vec<_> = matches.into_iter().take(50).map(|(index,unit,line,text)|json!({"unit":index+1,"location":unit.location,"line":line+1,"text":paths::bounded(text,512)})).collect();
        Ok(json!({"path":input.path,"format":document.format,"matches":matches,"next_offset":next,"warnings":document.warnings}).to_string())
    }
}
