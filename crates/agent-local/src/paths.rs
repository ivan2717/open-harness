use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

pub(crate) const FILE_LIMIT: usize = 256 * 1024;
pub(crate) const OUTPUT_LIMIT: usize = 64 * 1024;
const PROTECTED_NAMES: &[&str] = &[
    ".git",
    ".ssh",
    ".aws",
    ".gnupg",
    ".codex",
    ".agents",
    ".npmrc",
    ".netrc",
    "credentials",
    "credentials.json",
    ".env",
];
const PROTECTED_PREFIXES: &[&str] = &[".env.", "id_rsa", "id_ed25519"];
const PROTECTED_SUFFIXES: &[&str] = &[".pem", ".key"];

pub(crate) fn denied_component(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    PROTECTED_NAMES.contains(&name.as_str())
        || PROTECTED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || PROTECTED_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

/// Use the same component policy for Rust tools and Seatbelt. Explicit ASCII
/// case classes also match case-sensitive volumes without relying on flags
/// unsupported by older Seatbelt regex implementations.
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
            "node_modules"
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
                if denied_component(name) {
                    return Err(denied("protected path"));
                }
                candidate.push(name);
                match fs::symlink_metadata(&candidate) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(denied("symbolic links are not accessible"));
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

pub(crate) fn text_file(path: &Path) -> io::Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(denied("expected a regular file"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() > 1 {
                    return Err(denied("hard-linked files are not accessible"));
                }
            }
            if metadata.len() > FILE_LIMIT as u64 {
                return Err(io::Error::other("file exceeds 256 KiB limit"));
            }
            let mut options = fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW);
            }
            use std::io::Read;
            let mut content = Vec::new();
            options
                .open(path)?
                .take((FILE_LIMIT + 1) as u64)
                .read_to_end(&mut content)?;
            if content.len() > FILE_LIMIT || content.contains(&0) {
                return Err(io::Error::other(
                    "binary or oversized file is not supported",
                ));
            }
            String::from_utf8(content)
                .map(Some)
                .map_err(io::Error::other)
        }
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
