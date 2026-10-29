use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

#[path = "file_access/mod.rs"]
mod file_access;
pub(crate) use file_access::ParentDirectory;

pub(crate) const FILE_LIMIT: usize = 256 * 1024;
pub(crate) const READ_LIMIT: usize = 32 * 1024 * 1024;
pub(crate) const OUTPUT_LIMIT: usize = 64 * 1024;
pub(crate) use crate::protected_paths::denied_component;
#[cfg(not(windows))]
use crate::protected_paths::{PROTECTED_NAMES, PROTECTED_PREFIXES, PROTECTED_SUFFIXES};

/// Use the same component policy for Rust tools and Seatbelt. Explicit ASCII
/// case classes also match case-sensitive volumes without relying on flags
/// unsupported by older Seatbelt regex implementations.
#[cfg(not(windows))]
pub(crate) fn secret_patterns() -> Vec<String> {
    let names = PROTECTED_NAMES
        .iter()
        .filter(|name| **name != ".git")
        .map(|name| regex_literal(name))
        .collect::<Vec<_>>()
        .join("|");
    let prefixes = PROTECTED_PREFIXES
        .iter()
        .map(|name| regex_literal(name))
        .collect::<Vec<_>>()
        .join("|");
    let suffixes = PROTECTED_SUFFIXES
        .iter()
        .map(|name| regex_literal(name))
        .collect::<Vec<_>>()
        .join("|");
    vec![
        format!("(^|/)({names})(/|$)"),
        format!("(^|/)({prefixes})[^/]*(/|$)"),
        format!("({suffixes})(/|$)"),
    ]
}

#[cfg(not(windows))]
pub(crate) fn regex_literal(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '.' => "[.]".into(),
            ch if ch.is_ascii_alphabetic() => {
                format!("[{}{}]", ch.to_ascii_lowercase(), ch.to_ascii_uppercase())
            }
            ch => ch.to_string(),
        })
        .collect()
}

pub(crate) fn skipped_directory(name: &str) -> bool {
    denied_component(name)
        || matches!(
            name,
            ".DS_Store"
                | "node_modules"
                | "target"
                | "dist"
                | "out"
                | ".next"
                | ".venv"
                | "venv"
                | "__pycache__"
                | ".openharness-tmp"
        )
}

/// Only the host's effective task policy can construct unrestricted scope.
#[derive(Clone, Copy)]
pub(crate) struct Scope<'a> {
    pub root: &'a Path,
    pub unrestricted: bool,
}
impl Scope<'_> {
    pub fn resolve(self, path: &str) -> io::Result<PathBuf> {
        if !self.unrestricted {
            return resolve(self.root, path);
        }
        let input = Path::new(path);
        let candidate = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.root.join(input)
        };
        // Canonicalize the existing prefix so symlink aliases and '..' have
        // their actual OS meaning. Missing descendants are allowed for writes.
        fn existing_prefix(path: &Path) -> io::Result<PathBuf> {
            match fs::canonicalize(path) {
                Ok(path) => Ok(path),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let parent = path.parent().ok_or(error)?;
                    let name = path
                        .file_name()
                        .ok_or_else(|| denied("invalid host path"))?;
                    Ok(existing_prefix(parent)?.join(name))
                }
                Err(error) => Err(error),
            }
        }
        existing_prefix(&candidate)
    }
}

/// Refuse every symlink component, including links whose current target is
/// inside the root. This avoids surprising writes through shared aliases.
pub(crate) fn resolve(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(denied("use a workspace-relative path"));
    }
    let mut candidate = root.to_path_buf();
    for component in path.components() {
        match component {
            Component::CurDir => continue,
            Component::Normal(name) => {
                let name = name.to_str().ok_or_else(|| denied("path must be UTF-8"))?;
                #[cfg(windows)]
                if name.contains(':') || name.ends_with(['.', ' ']) {
                    return Err(denied(
                        "alternate data streams and ambiguous Windows names are not accessible",
                    ));
                }
                if denied_component(name) {
                    return Err(denied("protected path"));
                }
                candidate.push(name);
                match fs::symlink_metadata(&candidate) {
                    Ok(metadata) if redirected(&metadata) => {
                        return Err(denied(
                            "symbolic links and reparse points are not accessible",
                        ));
                    }
                    Ok(_) => (),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error),
                }
            }
            _ => return Err(denied("path escapes the workspace")),
        }
    }
    Ok(candidate)
}

fn redirected(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Opens a bounded regular file without following its final symlink. All callers
/// must first resolve the workspace path to apply the component policy.
pub(crate) fn read_bytes(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let parent = ParentDirectory::open(path, false)?;
    let mut file = parent.read()?;
    let metadata = file.metadata()?;
    if metadata.len() > limit as u64 {
        return Err(io::Error::other(format!(
            "file exceeds {} MiB read limit",
            limit as f64 / 1048576.0
        )));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    ParentDirectory::check_file(&file)?;
    if bytes.len() > limit {
        return Err(io::Error::other("file exceeds read limit"));
    }
    Ok(bytes)
}

pub(crate) fn create_parent_directory(target: &Path) -> io::Result<()> {
    ParentDirectory::open(target, true).map(|_| ())
}

pub(crate) fn read_text(path: &Path, limit: usize) -> io::Result<String> {
    let bytes = read_bytes(path, limit)?;
    if bytes.contains(&0) {
        return Err(io::Error::other(
            "unsupported binary file; read_file supports UTF-8, PDF, DOCX, XLSX and PPTX",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::other("not a supported UTF-8 text file or document"))
}

/// The smaller edit/review bound is independent from the paginated read limit.
pub(crate) fn text_file(path: &Path) -> io::Result<Option<String>> {
    match read_text(path, FILE_LIMIT) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn bounded(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit.saturating_sub(32);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[output truncated]", &text[..end])
}

pub(crate) fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
