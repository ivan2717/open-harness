//! Bounded text search with explicit coverage, independent from edit snapshots.
use crate::{documents, paths};
use serde_json::{Value, json};
use std::{fs, io, path::Path};

const ENTRY_LIMIT: usize = 16_384;
const BYTE_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Default)]
struct Scan {
    matches: Vec<Value>,
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
        if self.skipped.len() < 64 {
            self.skipped.push(json!({"path":path,"reason":reason}));
        }
    }
    fn walk(&mut self, root: &Path, path: &Path, query: &str, depth: usize) -> io::Result<()> {
        if self.limit_reached {
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
            let mut entries = fs::read_dir(path)?
                .take(ENTRY_LIMIT + 1)
                .collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                if self.limit_reached {
                    break;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if paths::skipped_directory(&name) {
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
        } else if !metadata.is_file() || metadata.file_type().is_symlink() {
            self.skip(&relative, "not a regular file or symbolic link");
        } else if documents::supported(path) {
            self.skip(&relative, "document: use search_document");
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
                    self.matches.push(
                        json!({"path":relative,"line":index+1,"text":paths::bounded(line,256)}),
                    );
                    if self.matches.len() == 100 {
                        self.limit_reached = true;
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn search(root: &Path, path: &str, query: &str) -> io::Result<Value> {
    if query.is_empty() || query.len() > 1024 {
        return Err(io::Error::other("query must contain 1 to 1024 bytes"));
    }
    let target = paths::resolve(root, path)?;
    if !target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "search path does not exist",
        ));
    }
    let mut scan = Scan::default();
    scan.walk(root, &target, query, 0)?;
    Ok(
        json!({"matches":scan.matches,"limit_reached":scan.limit_reached,"coverage":{"complete":!scan.limit_reached && scan.skipped_count == 0,"scanned_files":scan.scanned,"scanned_bytes":scan.bytes,"skipped_count":scan.skipped_count,"skipped":scan.skipped},"hint":"Text search excludes binary documents. Use search_document for PDF, DOCX, XLSX and PPTX."}),
    )
}
