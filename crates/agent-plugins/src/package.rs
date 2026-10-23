use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use sha2::{Digest, Sha256};

use crate::invalid;

pub(crate) const MAX_PACKAGE_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_DEPTH: usize = 16;

#[derive(Default)]
struct Budget {
    bytes: u64,
    entries: usize,
}

impl Budget {
    fn entry(&mut self, size: u64) -> io::Result<()> {
        self.entries += 1;
        self.bytes = self
            .bytes
            .checked_add(size)
            .ok_or_else(|| invalid("package size overflow"))?;
        if self.entries > MAX_ENTRIES || self.bytes > MAX_PACKAGE_BYTES || size > MAX_FILE_BYTES {
            return Err(invalid(
                "plugin package exceeds limits (4096 entries, 16 MiB per file, 64 MiB total)",
            ));
        }
        Ok(())
    }
}

pub(crate) fn relative_path(value: &str) -> io::Result<PathBuf> {
    if value.is_empty()
        || value.len() > 2048
        || value.contains(['\\', ':'])
        || value.chars().any(char::is_control)
    {
        return Err(invalid(
            "plugin paths must be relative, without control characters or Windows separators",
        ));
    }
    let path = Path::new(value);
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => (),
            Component::Normal(name) => relative.push(name),
            _ => return Err(invalid("plugin path escapes the package root")),
        }
    }
    if relative.as_os_str().is_empty() || relative.components().count() > MAX_DEPTH {
        return Err(invalid("plugin path is empty or exceeds 16 levels"));
    }
    Ok(relative)
}

pub(crate) fn validate_tree_path(root: &Path, relative: &Path) -> io::Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(invalid("invalid package path"));
        }
        current.push(component);
        if fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(invalid("symbolic links are not permitted in plugins"));
        }
    }
    Ok(())
}

pub(crate) fn read_regular(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid("symbolic links are not permitted in plugins"));
    }
    read_file(options.open(path)?, limit)
}

fn read_file(file: File, limit: u64) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid("expected a bounded regular plugin file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() > 1 {
            return Err(invalid("hard-linked files are not permitted in plugins"));
        }
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("plugin file exceeds size limit"));
    }
    Ok(bytes)
}

pub(crate) fn import(source: &Path, destination: &Path) -> io::Result<()> {
    if source.is_dir() {
        copy_directory(source, destination, &mut Budget::default())
    } else {
        import_zip(source, destination)?;
        // Common ZIP exports contain one enclosing package directory. Unwrap
        // exactly that shape, without searching arbitrary nested folders.
        if !destination.join("plugin.json").exists()
            && !destination.join(".codex-plugin/plugin.json").exists()
        {
            let children = fs::read_dir(destination)?.collect::<io::Result<Vec<_>>>()?;
            if children.len() == 1 && children[0].file_type()?.is_dir() {
                let wrapper = children[0].path();
                if wrapper.join("plugin.json").is_file()
                    || wrapper.join(".codex-plugin/plugin.json").is_file()
                {
                    for entry in fs::read_dir(&wrapper)? {
                        let entry = entry?;
                        fs::rename(entry.path(), destination.join(entry.file_name()))?;
                    }
                    fs::remove_dir(wrapper)?;
                }
            }
        }
        Ok(())
    }
}

fn import_zip(source: &Path, destination: &Path) -> io::Result<()> {
    if source
        .extension()
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("zip"))
    {
        return Err(invalid("import a plugin directory or .zip archive"));
    }
    let bytes = read_regular(source, MAX_PACKAGE_BYTES)?;
    let mut archive = zip::ZipArchive::new(io::Cursor::new(bytes))
        .map_err(|_| invalid("invalid plugin ZIP archive"))?;
    if archive.len() > MAX_ENTRIES {
        return Err(invalid("plugin ZIP contains too many entries"));
    }
    let mut budget = Budget::default();
    let mut seen = HashSet::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|_| invalid("cannot read plugin ZIP entry"))?;
        let entry_name = entry
            .name()
            .map_err(|_| invalid("invalid ZIP filename encoding"))?;
        let relative = relative_path(&entry_name)?;
        // Case-insensitive collisions must also be rejected for macOS/Windows.
        if !seen.insert(relative.to_string_lossy().to_lowercase()) {
            return Err(invalid("plugin ZIP contains duplicate paths"));
        }
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 && kind != 0o040000 {
                return Err(invalid("plugin ZIP contains a link or special file"));
            }
        }
        budget.entry(entry.size())?;
        let target = destination.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        fs::create_dir_all(
            target
                .parent()
                .ok_or_else(|| invalid("invalid archive path"))?,
        )?;
        let mut content = Vec::new();
        (&mut entry)
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut content)?;
        if content.len() as u64 != entry.size() || content.len() as u64 > MAX_FILE_BYTES {
            return Err(invalid("plugin ZIP entry size differs from its metadata"));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)?;
        file.write_all(&content)?;
        preserve_executable(&target, entry.unix_mode().unwrap_or(0))?;
    }
    Ok(())
}

#[cfg(unix)]
fn preserve_executable(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // Strip setuid/setgid and group/world write bits from imported content.
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if mode & 0o111 == 0 { 0o600 } else { 0o700 }),
    )
}

#[cfg(not(unix))]
fn preserve_executable(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn copy_directory(source: &Path, destination: &Path, budget: &mut Budget) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)?;
    copy_open_directory(&directory, destination, budget, 0)
}

#[cfg(unix)]
fn copy_open_directory(
    directory: &File,
    destination: &Path,
    budget: &mut Budget,
    depth: usize,
) -> io::Result<()> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::fs::PermissionsExt,
        },
    };
    for name in directory_entries(directory)? {
        if depth >= MAX_DEPTH {
            return Err(invalid("plugin package exceeds 16 directory levels"));
        }
        let safe = relative_path(&name)?;
        if safe.components().count() != 1 {
            return Err(invalid("invalid plugin filename"));
        }
        let name = CString::new(name).map_err(|_| invalid("invalid plugin filename"))?;
        // SAFETY: fd and CString live through openat; successful fd ownership is
        // transferred exactly once to File. NOFOLLOW prevents replacement links.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let source = unsafe { File::from_raw_fd(fd) };
        let metadata = source.metadata()?;
        let target = destination.join(&safe);
        if metadata.is_dir() {
            budget.entry(0)?;
            fs::create_dir(&target)?;
            copy_open_directory(&source, &target, budget, depth + 1)?;
        } else {
            let content = read_file(source, MAX_FILE_BYTES)?;
            budget.entry(content.len() as u64)?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            output.write_all(&content)?;
            preserve_executable(&target, metadata.permissions().mode())?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn directory_entries(directory: &File) -> io::Result<Vec<String>> {
    use std::{ffi::CStr, os::fd::AsRawFd};
    // fdopendir takes ownership; duplicate the descriptor so the caller retains
    // its stable directory handle for all subsequent openat calls.
    let duplicate = unsafe { libc::dup(directory.as_raw_fd()) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        unsafe {
            libc::close(duplicate);
        }
        return Err(error);
    }
    let result = (|| {
        let mut names = Vec::new();
        loop {
            // SAFETY: stream remains valid until closed below. Copy d_name before
            // the next readdir, which may reuse the returned entry storage.
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let name = std::str::from_utf8(bytes)
                .map_err(|_| invalid("plugin filenames must be UTF-8"))?;
            if names.len() >= MAX_ENTRIES {
                return Err(invalid("plugin directory contains too many entries"));
            }
            names.push(name.to_owned());
        }
        names.sort();
        Ok(names)
    })();
    unsafe {
        libc::closedir(stream);
    }
    result
}

#[cfg(not(unix))]
fn copy_directory(source: &Path, destination: &Path, budget: &mut Budget) -> io::Result<()> {
    fn visit(
        source: &Path,
        destination: &Path,
        budget: &mut Budget,
        depth: usize,
    ) -> io::Result<()> {
        if depth >= MAX_DEPTH {
            return Err(invalid("plugin package exceeds 16 directory levels"));
        }
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(invalid("symbolic links are not permitted in plugins"));
            }
            let name = entry.file_name();
            let name = relative_path(
                name.to_str()
                    .ok_or_else(|| invalid("plugin filenames must be UTF-8"))?,
            )?;
            let target = destination.join(name);
            if metadata.is_dir() {
                budget.entry(0)?;
                fs::create_dir(&target)?;
                visit(&entry.path(), &target, budget, depth + 1)?;
            } else {
                let content = read_regular(&entry.path(), MAX_FILE_BYTES)?;
                budget.entry(content.len() as u64)?;
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(target)?
                    .write_all(&content)?;
            }
        }
        Ok(())
    }
    visit(source, destination, budget, 0)
}

pub(crate) fn content_hash(root: &Path) -> io::Result<String> {
    fn hash_tree(
        root: &Path,
        relative: &Path,
        digest: &mut Sha256,
        budget: &mut Budget,
    ) -> io::Result<()> {
        if relative.components().count() > MAX_DEPTH {
            return Err(invalid("plugin package exceeds directory depth limit"));
        }
        let mut entries = Vec::new();
        for entry in fs::read_dir(root.join(relative))? {
            if entries.len() >= MAX_ENTRIES.saturating_sub(budget.entries) {
                return Err(invalid("plugin contains too many entries"));
            }
            entries.push(entry?);
        }
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = relative.join(entry.file_name());
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(invalid("symbolic links are not permitted in plugins"));
            }
            let name = path
                .to_str()
                .ok_or_else(|| invalid("invalid plugin filename"))?;
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name.as_bytes());
            if metadata.is_dir() {
                budget.entry(0)?;
                digest.update(b"directory");
                hash_tree(root, &path, digest, budget)?;
            } else {
                digest.update(b"file");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    digest.update([u8::from(metadata.permissions().mode() & 0o111 != 0)]);
                }
                let bytes = read_regular(&entry.path(), MAX_FILE_BYTES)?;
                budget.entry(bytes.len() as u64)?;
                digest.update((bytes.len() as u64).to_le_bytes());
                digest.update(bytes);
            }
        }
        Ok(())
    }
    let mut digest = Sha256::new();
    hash_tree(root, Path::new(""), &mut digest, &mut Budget::default())?;
    Ok(format!("{:x}", digest.finalize()))
}
