use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{WikiState, invalid};

pub(crate) const MAX_STATE_BYTES: u64 = 32 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub(crate) fn directory(path: &Path) -> io::Result<PathBuf> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(invalid("memory directory must be a real directory"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir_all(path)?,
        Err(error) => return Err(error),
    }
    fs::canonicalize(path)
}

pub(crate) fn child(root: &Path, name: &str, create: bool) -> io::Result<Option<PathBuf>> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid("memory parent must be a real directory"));
    }
    let candidate = root.join(name);
    match fs::symlink_metadata(&candidate) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(invalid("memory path must be a real directory"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound && !create => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match fs::create_dir(&candidate) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            if fs::symlink_metadata(&candidate)?.file_type().is_symlink() {
                return Err(invalid("memory path must be a real directory"));
            }
        }
        Err(error) => return Err(error),
    }
    let canonical = fs::canonicalize(candidate)?;
    if canonical.parent() != Some(root) {
        return Err(invalid("memory path escaped its store"));
    }
    Ok(Some(canonical))
}

pub(crate) fn read(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            return Err(invalid("memory state must be a regular file"));
        }
        Ok(meta) if meta.len() > MAX_STATE_BYTES => {
            return Err(invalid("memory state is too large"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut bytes = Vec::new();
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("memory state must be a regular file"));
    }
    file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(invalid("memory state is too large"));
    }
    Ok(Some(bytes))
}

pub(crate) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(invalid("memory state is too large"));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(invalid("memory output must be a regular file"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("missing memory directory"))?;
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".wiki-{}-{sequence}.tmp", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    // Cleanup must only remove a temporary file this call actually created.
    let mut file = options.open(&temp)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub(crate) fn lock(directory: &Path) -> io::Result<File> {
    let path = directory.join(".lock");
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err(invalid("memory lock must be a regular file"));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("memory lock must be a regular file"));
    }
    file.lock()?;
    Ok(file)
}

/// Markdown is a regenerable view. The authoritative JSON has already been
/// committed when this runs; callers must not roll it back on export failure.
pub(crate) fn export(directory: &Path, state: &WikiState) -> io::Result<()> {
    let pages = child(directory, "pages", true)?.expect("created directory");
    let mut index = format!(
        "# Session memory\n\nDerived notes through event {} (revision {}). Newer user instructions and current evidence take precedence.\n\n{}\n\n## Pages\n",
        state.through_sequence, state.revision, state.overview,
    );
    if let Some(cursor) = state.source_cursor {
        index.push_str(&format!("\nNext source position: event {}, character {}. Later content may still be pending.\n\n", cursor.sequence, cursor.offset));
    } else {
        index.push_str("\nLegacy partial excerpts; source backfill is pending.\n\n");
    }
    index.push_str("- [Task state and evidence](tasks.md)\n");
    for page in &state.pages {
        index.push_str(&format!(
            "- [{}](pages/{}.md)\n",
            page.title.replace(['[', ']'], ""),
            page.id
        ));
        let sources = page
            .source_sequences
            .iter()
            .map(|sequence| format!("event:{sequence}"))
            .collect::<Vec<_>>()
            .join(", ");
        let content = format!(
            "# {}\n\n{}\n\n---\nSources in this session: {sources}\nUpdated through event {}.\n",
            page.title, page.content, page.updated_through
        );
        write(&pages.join(format!("{}.md", page.id)), content.as_bytes())?;
    }
    let mut log = String::from(
        "# Memory revisions\n\nPrevious page content is retained in wiki.json; canonical events remain in the session event journal. This view lists the most recent 64 revisions.\n",
    );
    for revision in &state.revisions {
        log.push_str(&format!(
            "\n## Revision {} · through event {}\n\nUpdated: {}\n",
            revision.revision,
            revision.through_sequence,
            revision.updated_pages.join(", ")
        ));
        if !revision.updated_tasks.is_empty() {
            log.push_str(&format!(
                "Task updates: {}\n",
                revision.updated_tasks.join(", ")
            ));
        }
    }
    let mut tasks = String::from(
        "# Task state\n\nDerived records with exact source quotes. Source eligibility does not prove every paraphrase correct; inspect current state when necessary.\n",
    );
    for item in &state.tasks {
        tasks.push_str(&format!(
            "\n## {} · {:?} · {:?}\n\n{}\n",
            item.id, item.kind, item.status, item.text
        ));
        for evidence in &item.evidence {
            tasks.push_str(&format!(
                "\nSource event {}:\n> {}\n",
                evidence.sequence,
                evidence.quote.replace('\n', "\n> ")
            ));
        }
    }
    write(&directory.join("tasks.md"), tasks.as_bytes())?;
    write(&directory.join("index.md"), index.as_bytes())?;
    write(&directory.join("log.md"), log.as_bytes())
}
