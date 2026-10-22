use super::{ExtractedDocument, TEXT_LIMIT, UNIT_LIMIT};
use calamine::{Reader, Xlsx};
use quick_xml::{Reader as XmlReader, events::Event};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Cursor, Read},
};
use zip::ZipArchive;

const EXPANDED_LIMIT: u64 = 64 * 1024 * 1024;
const XML_LIMIT: u64 = 8 * 1024 * 1024;
type Archive<'a> = ZipArchive<Cursor<&'a [u8]>>;
fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("invalid Office document: {error}"))
}

/// Validate declared AND actual decompressed sizes before handing the archive
/// to another parser. Nothing is extracted onto disk or resolved over a network.
fn checked_archive(bytes: &[u8]) -> io::Result<Archive<'_>> {
    if bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0]) {
        return Err(io::Error::other(
            "encrypted or legacy Office document; save an unencrypted DOCX/XLSX/PPTX copy",
        ));
    }
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(invalid)?;
    if archive.len() > 4096 {
        return Err(io::Error::other("Office archive exceeds 4096 entries"));
    }
    let mut total = 0u64;
    let mut names = BTreeSet::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(invalid)?;
        let name = entry.name().map_err(invalid)?.into_owned();
        if !names.insert(name.clone()) || entry.enclosed_name().is_none() {
            return Err(invalid("duplicate or unsafe archive entry"));
        }
        let limit = if name.ends_with(".xml") || name.ends_with(".rels") {
            XML_LIMIT
        } else {
            super::MAX_DOCUMENT_BYTES as u64
        };
        if entry.size() > limit || total.saturating_add(entry.size()) > EXPANDED_LIMIT {
            return Err(io::Error::other(
                "Office archive expansion limit exceeded (8 MiB XML / 64 MiB total)",
            ));
        }
        let size =
            io::copy(&mut entry.by_ref().take(limit + 1), &mut io::sink()).map_err(invalid)?;
        total = total.saturating_add(size);
        if size > limit || total > EXPANDED_LIMIT {
            return Err(io::Error::other("Office archive expansion limit exceeded"));
        }
    }
    Ok(archive)
}
fn xml(archive: &mut Archive<'_>, name: &str) -> io::Result<String> {
    let mut entry = archive.by_name(name).map_err(invalid)?;
    let mut text = String::new();
    entry
        .by_ref()
        .take(XML_LIMIT + 1)
        .read_to_string(&mut text)
        .map_err(invalid)?;
    if text.len() > XML_LIMIT as usize {
        return Err(invalid("XML expansion limit exceeded"));
    }
    Ok(text)
}

pub(super) fn extract(format: &str, bytes: &[u8]) -> io::Result<ExtractedDocument> {
    let mut archive = checked_archive(bytes)?;
    let mut output = ExtractedDocument::new(format);
    match format {
        "docx" => {
            for (index, text) in paragraphs(&xml(&mut archive, "word/document.xml")?)?
                .into_iter()
                .enumerate()
            {
                output.push(format!("paragraph {}", index + 1), text)?;
            }
            output.warnings.push("Body text and table cells only; page layout, headers, footers and embedded images are not extracted.".into());
        }
        "pptx" => {
            let presentation = xml(&mut archive, "ppt/presentation.xml")?;
            let rels = relationships(&xml(&mut archive, "ppt/_rels/presentation.xml.rels")?)?;
            let mut reader = XmlReader::from_str(&presentation);
            let mut slide = 0;
            loop {
                match reader.read_event().map_err(invalid)? {
                    Event::Empty(event) | Event::Start(event)
                        if event.local_name().as_ref() == "sldId" =>
                    {
                        let id = event
                            .attributes()
                            .flatten()
                            .find(|a| a.key.as_ref() == "r:id")
                            .ok_or_else(|| invalid("slide relationship missing"))?;
                        let id = id
                            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                            .map_err(invalid)?;
                        let target = rels
                            .get(id.as_ref())
                            .ok_or_else(|| invalid("slide relationship missing"))?;
                        let target = target.strip_prefix("/ppt/").unwrap_or(target);
                        if !target.starts_with("slides/")
                            || target.contains("..")
                            || target.contains('\\')
                        {
                            return Err(invalid("invalid slide target"));
                        }
                        slide += 1;
                        let text =
                            paragraphs(&xml(&mut archive, &format!("ppt/{target}"))?)?.join("\n");
                        output.push(format!("slide {slide}"), text)?;
                    }
                    Event::DocType(_) => return Err(invalid("XML document types are not allowed")),
                    Event::Eof => break,
                    _ => (),
                }
            }
            output.warnings.push(
                "Slide text only; images, speaker notes, animations and layout are not extracted."
                    .into(),
            );
        }
        "xlsx" => {
            let mut book = Xlsx::new(Cursor::new(bytes)).map_err(invalid)?;
            let names = book.sheet_names();
            if names.len() > 256 {
                return Err(invalid("workbook exceeds 256 sheets"));
            }
            let mut cells = 0;
            for name in names {
                let mut reader = book.worksheet_cells_reader(&name).map_err(invalid)?;
                let mut text = String::new();
                while let Some(cell) = reader.next_cell().map_err(invalid)? {
                    cells += 1;
                    if cells > 200_000 {
                        return Err(invalid("workbook exceeds 200000 cells"));
                    }
                    let value = calamine::Data::from(cell.get_value().clone()).to_string();
                    if value.is_empty() {
                        continue;
                    }
                    let (row, column) = cell.get_position();
                    let line =
                        format!("{}{}: {}\n", column_name(column), u64::from(row) + 1, value);
                    if text.len().saturating_add(line.len()) > TEXT_LIMIT {
                        return Err(invalid("extracted sheet exceeds 8 MiB"));
                    }
                    text.push_str(&line);
                }
                output.push(format!("sheet {name}"), text)?;
            }
            output.warnings.push("Uses stored cell values; formulas are not recalculated and may be stale. Charts, images and formatting are not extracted.".into());
        }
        _ => unreachable!(),
    }
    if output.units.is_empty() {
        output.warnings.push("No extractable text found.".into());
    }
    Ok(output)
}
fn column_name(mut column: u32) -> String {
    let mut name = Vec::new();
    loop {
        name.push((b'A' + (column % 26) as u8) as char);
        if column < 26 {
            break;
        }
        column = column / 26 - 1;
    }
    name.into_iter().rev().collect()
}
fn relationships(source: &str) -> io::Result<BTreeMap<String, String>> {
    let mut reader = XmlReader::from_str(source);
    let mut result = BTreeMap::new();
    loop {
        match reader.read_event().map_err(invalid)? {
            Event::Empty(event) | Event::Start(event)
                if event.local_name().as_ref() == "Relationship" =>
            {
                let mut id = None;
                let mut target = None;
                let mut external = false;
                for attr in event.attributes() {
                    let attr = attr.map_err(invalid)?;
                    let value = attr
                        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(invalid)?
                        .into_owned();
                    match attr.key.as_ref() {
                        "Id" => id = Some(value),
                        "Target" => target = Some(value),
                        "TargetMode" => external = value == "External",
                        _ => (),
                    }
                }
                if !external && let (Some(id), Some(target)) = (id, target) {
                    result.insert(id, target);
                }
            }
            Event::DocType(_) => return Err(invalid("XML document types are not allowed")),
            Event::Eof => break,
            _ => (),
        }
    }
    Ok(result)
}
fn paragraphs(source: &str) -> io::Result<Vec<String>> {
    let mut reader = XmlReader::from_str(source);
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_text = false;
    let mut depth = 0usize;
    let mut total = 0usize;
    loop {
        match reader.read_event().map_err(invalid)? {
            Event::Start(event) => {
                depth += 1;
                if depth > 128 {
                    return Err(invalid("XML nesting exceeds 128"));
                }
                if event.local_name().as_ref() == "t" {
                    in_text = true;
                }
            }
            Event::End(event) => {
                depth = depth.saturating_sub(1);
                match event.local_name().as_ref() {
                    "t" => in_text = false,
                    "p" if !current.is_empty() => {
                        total += current.len();
                        if result.len() >= UNIT_LIMIT || total > TEXT_LIMIT {
                            return Err(invalid("document text limit exceeded"));
                        }
                        result.push(std::mem::take(&mut current));
                    }
                    _ => (),
                }
            }
            Event::Empty(event) => match event.local_name().as_ref() {
                "tab" => current.push('\t'),
                "br" | "cr" => current.push('\n'),
                _ => (),
            },
            Event::Text(text) if in_text => current.push_str(&text.xml10_content()),
            Event::GeneralRef(reference) if in_text => {
                if let Some(ch) = reference.resolve_char_ref().map_err(invalid)? {
                    current.push(ch);
                } else {
                    let name = reference.xml10_content();
                    current.push_str(match name.as_ref() {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => return Err(invalid("unknown XML entity")),
                    });
                }
            }
            Event::DocType(_) => return Err(invalid("XML document types are not allowed")),
            Event::Eof => break,
            _ => (),
        }
    }
    if depth != 0 {
        return Err(invalid("unclosed XML element"));
    }
    if !current.is_empty() {
        result.push(current);
    }
    Ok(result)
}
