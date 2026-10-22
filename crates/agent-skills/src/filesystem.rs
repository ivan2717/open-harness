use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

#[cfg(unix)]
use std::sync::Arc;

/// Keep an open directory so a rename or symlink substitution cannot redirect
/// resource reads outside the skill that was discovered. Unix children are
/// opened relative to this descriptor, never by following the displayed path.
#[derive(Clone, Debug)]
pub(crate) struct SkillDirectory {
    pub path: PathBuf,
    #[cfg(unix)]
    handle: Arc<File>,
}

impl SkillDirectory {
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(denied("skill roots must be absolute paths"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            let handle = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open("/")?;
            let mut directory = Self {
                path: PathBuf::from("/"),
                handle: Arc::new(handle),
            };
            for component in path.components() {
                match component {
                    Component::RootDir | Component::CurDir => (),
                    Component::Normal(name) => directory = directory.child(name)?,
                    _ => return Err(denied("skill roots must not contain parent traversal")),
                }
            }
            Ok(directory)
        }
        #[cfg(not(unix))]
        {
            for ancestor in path.ancestors() {
                let metadata = fs::symlink_metadata(ancestor)?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(denied("skill roots must contain only real directories"));
                }
            }
            if path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
            {
                return Err(denied("skill roots must not contain parent traversal"));
            }
            Ok(Self {
                path: fs::canonicalize(path)?,
            })
        }
    }

    pub fn child(&self, name: &OsStr) -> io::Result<Self> {
        let path = self.path.join(name);
        #[cfg(unix)]
        {
            Ok(Self {
                path,
                handle: Arc::new(self.open_child(name, true)?),
            })
        }
        #[cfg(not(unix))]
        {
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(denied("symbolic links are not accessible"));
            }
            let path = fs::canonicalize(&path)?;
            if !path.starts_with(&self.path) {
                return Err(denied("path escapes the skill directory"));
            }
            Ok(Self { path })
        }
    }

    pub fn entries(&self, remaining: usize) -> io::Result<Vec<OsString>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.path)? {
            if names.len() == remaining {
                return Err(io::Error::other("skill scan entry limit reached"));
            }
            names.push(entry?.file_name());
        }
        names.sort();
        Ok(names)
    }

    pub fn read_text(&self, relative: &str, limit: usize) -> io::Result<String> {
        let components = resource_components(relative)?;
        let (filename, parents) = components
            .split_last()
            .ok_or_else(|| denied("a skill-relative file path is required"))?;
        let mut directory = self.clone();
        for parent in parents {
            directory = directory.child(parent)?;
        }
        #[cfg(unix)]
        let file = directory.open_child(filename, false)?;
        #[cfg(not(unix))]
        let file = {
            let path = directory.path.join(filename);
            if fs::symlink_metadata(&path)?.file_type().is_symlink() {
                return Err(denied("symbolic links are not accessible"));
            }
            let canonical = fs::canonicalize(&path)?;
            if !canonical.starts_with(&self.path) {
                return Err(denied("path escapes the skill directory"));
            }
            File::open(canonical)?
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(denied("expected a regular skill resource file"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.nlink() > 1 {
                return Err(denied("hard-linked skill files are not accessible"));
            }
        }
        if metadata.len() > limit as u64 {
            return Err(io::Error::other(format!(
                "skill file exceeds {limit} byte limit"
            )));
        }
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(io::Error::other(format!(
                "skill file exceeds {limit} byte limit"
            )));
        }
        let content = String::from_utf8(bytes)
            .map_err(|_| io::Error::other("skill resources must be UTF-8 text"))?;
        if content.contains('\0') {
            return Err(io::Error::other("binary skill resources are not supported"));
        }
        Ok(content)
    }

    #[cfg(unix)]
    fn open_child(&self, name: &OsStr, directory: bool) -> io::Result<File> {
        use std::{
            ffi::CString,
            os::{
                fd::{AsRawFd, FromRawFd},
                unix::ffi::OsStrExt,
            },
        };

        if name.as_bytes().contains(&b'/') || name == "." || name == ".." {
            return Err(denied("expected a single path component"));
        }
        let name = CString::new(name.as_bytes()).map_err(|_| denied("path contains a NUL byte"))?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK
            | if directory { libc::O_DIRECTORY } else { 0 };
        // SAFETY: the directory descriptor and CString remain valid during
        // openat. A successful descriptor is uniquely owned by the returned File.
        let descriptor = unsafe { libc::openat(self.handle.as_raw_fd(), name.as_ptr(), flags) };
        if descriptor < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat succeeded and ownership has not been transferred yet.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn resource_components(relative: &str) -> io::Result<Vec<&OsStr>> {
    if relative.is_empty() || relative.contains('\\') || relative.chars().any(char::is_control) {
        return Err(denied(
            "use a skill-relative file path without control characters",
        ));
    }
    Path::new(relative)
        .components()
        .map(|component| match component {
            Component::Normal(name) if !protected_resource(name) => Ok(name),
            _ => Err(denied(
                "resource path is protected or escapes the skill directory",
            )),
        })
        .collect()
}

fn protected_resource(name: &OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_lowercase();
    name.starts_with('.')
        || matches!(name.as_str(), "credentials" | "credentials.json")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.ends_with(".key")
        || name.ends_with(".pem")
}

fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
