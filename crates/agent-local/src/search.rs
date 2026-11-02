//! Bounded text search with explicit coverage, independent from edit snapshots.
use crate::{documents, paths};
use serde_json::{Value, json};
use std::{fs, io, path::Path};

const ENTRY_LIMIT: usize = 16_384;
const BYTE_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Default)]
struct Scan {
    unrestricted: bool,
    matches: Vec<Value>,
    match_bytes: usize,
    skipped_bytes: usize,
    offset: usize,
    seen: usize,
    has_more: bool,
    skipped: Vec<Value>,
    skipped_count: usize,
    scanned: usize,
    visited: usize,
    bytes: u64,
    limit_reached: bool,
}
impl Scan {
    fn skip(&mut self, path: &str, reason: &str) {
        self.skipped_count += 1;
        let entry = json!({"path":path,"reason":reason});
        let bytes = entry.to_string().len() + 1;
        if self.skipped.len() < 64 && self.skipped_bytes + bytes <= 8192 {
            self.skipped_bytes += bytes;
            self.skipped.push(entry);
        }
    }
    fn walk(&mut self, root: &Path, path: &Path, query: &str, depth: usize) -> io::Result<()> {
        if self.limit_reached || self.has_more {
            return Ok(());
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        self.visited += 1;
        if self.visited > ENTRY_LIMIT || depth > 64 {
            self.limit_reached = true;
            self.skip(&relative, "scan depth or entry limit reached");
            return Ok(());
        }
        let metadata = match fs::symlink_metadata(path) {
            Ok(value) => value,
            Err(_) => {
                self.skip(&relative, "file unavailable");
                return Ok(());
            }
        };
        if metadata.is_dir() {
            let directory = match fs::read_dir(path) {
                Ok(directory) => directory,
                Err(_) => {
                    self.skip(&relative, "directory unavailable");
                    return Ok(());
                }
            };
            let mut entries = Vec::new();
            let mut enumerated = 0;
            for entry in directory.take(ENTRY_LIMIT + 1) {
                enumerated += 1;
                match entry {
                    Ok(entry) => entries.push(entry),
                    Err(_) => self.skip(&relative, "directory entry unavailable"),
                }
            }
            let truncated = enumerated > ENTRY_LIMIT;
            if truncated {
                entries.truncate(ENTRY_LIMIT);
                self.skip(&relative, "directory enumeration limit reached");
            }
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                if self.limit_reached || self.has_more {
                    break;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if paths::skipped_directory(&name)
                    && !(self.unrestricted && paths::denied_component(&name))
                {
                    self.skip(
                        &entry
                            .path()
                            .strip_prefix(root)
                            .unwrap_or(&entry.path())
                            .to_string_lossy(),
                        "excluded path",
                    );
                    continue;
                }
                self.walk(root, &entry.path(), query, depth + 1)?;
            }
            self.limit_reached |= truncated;
        } else if !metadata.is_file() || metadata.file_type().is_symlink() {
            self.skip(&relative, "not a regular file or symbolic link");
        } else if documents::supported(path) {
            self.skip(
                &relative,
                "document: search this file individually with search_files",
            );
        } else if metadata.len() > paths::READ_LIMIT as u64 {
            self.skip(&relative, "file exceeds 32 MiB text read limit");
        } else if self.bytes.saturating_add(metadata.len()) > BYTE_LIMIT {
            self.limit_reached = true;
            self.skip(&relative, "64 MiB scan budget reached");
        } else {
            // Count attempted bytes as well, so binary files cannot evade the budget.
            self.bytes += metadata.len();
            let content = match paths::read_text(path, paths::READ_LIMIT) {
                Ok(text) => text,
                Err(_) => {
                    self.skip(&relative, "binary, non-UTF-8 or inaccessible file");
                    return Ok(());
                }
            };
            self.scanned += 1;
            for (index, line) in content.lines().enumerate() {
                if line.contains(query) {
                    self.seen += 1;
                    if self.seen <= self.offset {
                        continue;
                    }
                    let entry =
                        json!({"path":relative,"line":index+1,"text":paths::bounded(line,256)});
                    let bytes = entry.to_string().len() + 1;
                    if self.matches.len() == 100 || self.match_bytes + bytes > 48 * 1024 {
                        self.has_more = true;
                        break;
                    }
                    self.match_bytes += bytes;
                    self.matches.push(entry);
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn search(
    scope: paths::Scope<'_>,
    path: &str,
    query: &str,
    offset: usize,
) -> io::Result<Value> {
    if query.is_empty() || query.len() > 1024 {
        return Err(io::Error::other("query must contain 1 to 1024 bytes"));
    }
    let target = scope.resolve(path)?;
    if !target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "search path does not exist",
        ));
    }
    let mut scan = Scan {
        unrestricted: scope.unrestricted,
        offset,
        ..Scan::default()
    };
    scan.walk(scope.root, &target, query, 0)?;
    Ok(
        json!({"matches":scan.matches,"next_offset":scan.has_more.then(|| offset + scan.matches.len()),"limit_reached":scan.limit_reached || scan.has_more,"coverage":{"complete":!scan.limit_reached && !scan.has_more && scan.skipped_count == 0,"scanned_files":scan.scanned,"scanned_bytes":scan.bytes,"skipped_count":scan.skipped_count,"skipped":scan.skipped},"hint":"Text search excludes binary documents. Search a PDF, DOCX, XLSX or PPTX file individually with search_files."}),
    )
}
