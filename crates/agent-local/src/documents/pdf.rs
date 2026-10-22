use super::{ExtractedDocument, TEXT_LIMIT};
use pdf_extract::{Document, LoadOptions, Object, ObjectId};
use std::{
    cell::RefCell,
    io::{self, Read, Write},
};

const STREAM_LIMIT: usize = 16 * 1024 * 1024;
const EXPANSION_LIMIT: usize = 64 * 1024 * 1024;
#[derive(Default)]
struct Budget {
    objects: usize,
    expanded: usize,
    error: Option<String>,
}
thread_local! { static BUDGET: RefCell<Budget> = RefCell::new(Budget::default()); }

/// lopdf invokes the filter before expanding object streams. Decompress the
/// common Flate streams with a bounded reader, then remove their compression
/// filter so later font/content extraction cannot inflate them again.
fn filter(id: ObjectId, object: &mut Object) -> Option<(ObjectId, Object)> {
    BUDGET.with_borrow_mut(|budget| {
        budget.objects += 1;
        if budget.objects > 100_000 {
            budget.error = Some("PDF exceeds 100000 objects".into());
        }
        if budget.error.is_some() {
            return None;
        }
        if let Object::Stream(stream) = object {
            let image = stream
                .dict
                .get(b"Subtype")
                .and_then(Object::as_name)
                .is_ok_and(|name| name == b"Image");
            if image {
                // Images are not needed for text extraction, including scans.
                stream.content.clear();
                stream.dict.remove(b"Filter");
                stream.dict.remove(b"DecodeParms");
            } else {
                let filters = stream.filters().unwrap_or_default();
                if !filters.is_empty() {
                    if filters.len() != 1 || filters[0] != b"FlateDecode" {
                        budget.error = Some(
                            "PDF uses an unsupported stream encoding; re-export as a standard PDF"
                                .into(),
                        );
                        return None;
                    }
                    let mut decoded = Vec::new();
                    let decoder = flate2::read::ZlibDecoder::new(stream.content.as_slice());
                    if decoder
                        .take((STREAM_LIMIT + 1) as u64)
                        .read_to_end(&mut decoded)
                        .is_err()
                        || decoded.len() > STREAM_LIMIT
                    {
                        budget.error =
                            Some("PDF stream is malformed or exceeds 16 MiB decoded limit".into());
                        return None;
                    }
                    // Default/no-op predictors are safe to remove after decoding.
                    if stream.dict.get(b"DecodeParms").is_ok_and(|params| {
                        !matches!(params, Object::Null)
                            && params.as_dict().map_or(true, |dict| {
                                dict.get(b"Predictor")
                                    .is_ok_and(|value| value.as_i64().unwrap_or(2) != 1)
                            })
                    }) {
                        budget.error = Some(
                            "PDF text stream uses unsupported prediction; re-export the PDF".into(),
                        );
                        return None;
                    }
                    stream.content = decoded;
                    stream.dict.remove(b"Filter");
                    stream.dict.remove(b"DecodeParms");
                    stream.dict.set("Length", stream.content.len() as i64);
                }
                budget.expanded = budget.expanded.saturating_add(stream.content.len());
                if budget.expanded > EXPANSION_LIMIT {
                    budget.error = Some("PDF decoded streams exceed 64 MiB".into());
                    return None;
                }
            }
        }
        Some((id, object.clone()))
    })
}
struct TextWriter {
    text: Vec<u8>,
    remaining: usize,
}
impl Write for TextWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other("extracted PDF text exceeds 8 MiB"));
        }
        self.remaining -= bytes.len();
        self.text.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn extract(bytes: &[u8]) -> io::Result<ExtractedDocument> {
    if !bytes.starts_with(b"%PDF-") {
        return Err(io::Error::other("invalid PDF header"));
    }
    BUDGET.with_borrow_mut(|budget| *budget = Budget::default());
    let loaded = Document::load_mem_with_options(
        bytes,
        LoadOptions {
            filter: Some(filter),
            ..Default::default()
        },
    );
    if let Some(error) = BUDGET.with_borrow_mut(|budget| budget.error.take()) {
        return Err(io::Error::other(error));
    }
    let document = loaded.map_err(|error| {
        io::Error::other(format!(
            "cannot read PDF (malformed or encrypted); provide an unencrypted, valid PDF: {error}"
        ))
    })?;
    if document.is_encrypted() {
        return Err(io::Error::other(
            "PDF is encrypted; provide an unencrypted copy",
        ));
    }
    let pages = document.get_pages();
    if pages.len() > 2000 {
        return Err(io::Error::other(
            "PDF exceeds 2000 pages; split the document",
        ));
    }
    validate_resources(&document, pages.values().copied())?;
    let mut output = ExtractedDocument::new("pdf");
    let mut remaining = TEXT_LIMIT;
    let mut empty = 0;
    for number in pages.keys() {
        let mut writer = TextWriter {
            text: vec![],
            remaining,
        };
        let mut device = pdf_extract::PlainTextOutput::new(&mut writer as &mut dyn Write);
        pdf_extract::output_doc_page(&document, &mut device, *number).map_err(|error| {
            io::Error::other(format!(
                "cannot extract PDF page {number} within limits: {error}"
            ))
        })?;
        remaining = writer.remaining;
        let text = String::from_utf8(writer.text).map_err(io::Error::other)?;
        if text.trim().is_empty() {
            empty += 1;
        }
        output.push(format!("page {number}"), text)?;
    }
    if empty > 0 {
        output.warnings.push(format!("{empty} page(s) have no extractable text and may be scanned images. OCR is required to read those pages."));
    }
    output.warnings.push(
        "PDF text order may differ from visual layout; images and diagrams are not extracted."
            .into(),
    );
    Ok(output)
}

// pdf-extract recursively follows inherited resources and Form XObjects. Reject
// cyclic/deep graphs before entering that recursion, which cannot be caught as
// a normal parser error after a native stack overflow.
fn validate_resources(
    document: &Document,
    pages: impl Iterator<Item = ObjectId>,
) -> io::Result<()> {
    let mut visits = 0usize;
    for page in pages {
        let mut dictionary = document.get_dictionary(page).map_err(io::Error::other)?;
        let mut chain = std::collections::BTreeSet::new();
        let mut resources = None;
        loop {
            if resources.is_none() {
                resources = dictionary
                    .get(b"Resources")
                    .ok()
                    .and_then(|value| document.dereference(value).ok())
                    .and_then(|(_, value)| value.as_dict().ok());
            }
            let Ok(parent) = dictionary.get(b"Parent").and_then(Object::as_reference) else {
                break;
            };
            if !chain.insert(parent) || chain.len() > 64 {
                return Err(io::Error::other(
                    "PDF page inheritance is cyclic or too deep",
                ));
            }
            dictionary = document.get_dictionary(parent).map_err(io::Error::other)?;
        }
        if let Some(resources) = resources {
            let content = document.get_page_content(page).map_err(io::Error::other)?;
            validate_forms(document, resources, &content, &mut vec![], &mut visits)?;
        }
    }
    Ok(())
}
fn validate_forms(
    document: &Document,
    resources: &pdf_extract::Dictionary,
    content: &[u8],
    stack: &mut Vec<ObjectId>,
    visits: &mut usize,
) -> io::Result<()> {
    let operations = pdf_extract::content::Content::decode(content).map_err(io::Error::other)?;
    for operation in operations
        .operations
        .iter()
        .filter(|operation| operation.operator == "Do")
    {
        *visits += 1;
        if *visits > 20_000 || stack.len() > 32 {
            return Err(io::Error::other(
                "PDF form nesting or traversal limit exceeded",
            ));
        }
        let name = operation
            .operands
            .first()
            .and_then(|value| value.as_name().ok())
            .ok_or_else(|| io::Error::other("invalid PDF XObject invocation"))?;
        let objects = resources
            .get(b"XObject")
            .ok()
            .and_then(|value| document.dereference(value).ok())
            .and_then(|(_, value)| value.as_dict().ok())
            .ok_or_else(|| io::Error::other("missing PDF XObject resources"))?;
        let value = objects.get(name).map_err(io::Error::other)?;
        let (id, object) = document.dereference(value).map_err(io::Error::other)?;
        let stream = object.as_stream().map_err(io::Error::other)?;
        if stream
            .dict
            .get(b"Subtype")
            .and_then(Object::as_name)
            .is_ok_and(|name| name == b"Image")
        {
            continue;
        }
        if let Some(id) = id {
            if stack.contains(&id) {
                return Err(io::Error::other("PDF contains cyclic Form XObjects"));
            }
            stack.push(id);
        }
        let nested = stream
            .dict
            .get(b"Resources")
            .ok()
            .and_then(|value| document.dereference(value).ok())
            .and_then(|(_, value)| value.as_dict().ok())
            .unwrap_or(resources);
        validate_forms(document, nested, &stream.content, stack, visits)?;
        if id.is_some() {
            stack.pop();
        }
    }
    Ok(())
}
