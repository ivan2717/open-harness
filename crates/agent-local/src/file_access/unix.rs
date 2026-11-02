use crate::paths::denied;
use std::{
    ffi::{CString, OsStr},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

pub(crate) struct ParentDirectory {
    directory: File,
    name: CString,
}

impl ParentDirectory {
    pub fn open(path: &Path, create: bool) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(denied("file access requires an absolute path"));
        }
        let parent = path
            .parent()
            .ok_or_else(|| denied("cannot access the filesystem root"))?;
        let name = name_component(
            path.file_name()
                .ok_or_else(|| denied("expected a file name"))?,
        )?;
        let mut directory = opened(unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        })?;
        for part in parent.components() {
            let Component::Normal(part) = part else {
                if part == Component::RootDir {
                    continue;
                }
                return Err(denied("file path must be absolute and normalized"));
            };
            let part = name_component(part)?;
            let open = || {
                opened(unsafe {
                    libc::openat(
                        directory.as_raw_fd(),
                        part.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    )
                })
            };
            directory = match open() {
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    let result =
                        unsafe { libc::mkdirat(directory.as_raw_fd(), part.as_ptr(), 0o755) };
                    if result < 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(error);
                        }
                    }
                    open()?
                }
                result => result?,
            };
        }
        Ok(Self { directory, name })
    }

    pub fn read(&self) -> io::Result<File> {
        let file = opened(unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                self.name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        })?;
        Self::check_file(&file)?;
        Ok(file)
    }

    pub fn check_file(file: &File) -> io::Result<()> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(denied("expected a regular file without hard links"));
        }
        Ok(())
    }

    pub fn create(&self, temporary: &OsStr) -> io::Result<File> {
        let temporary = name_component(temporary)?;
        opened(unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o666,
            )
        })
    }

    pub fn replace(&self, temporary: &OsStr) -> io::Result<()> {
        let temporary = name_component(temporary)?;
        checked(unsafe {
            libc::renameat(
                self.directory.as_raw_fd(),
                temporary.as_ptr(),
                self.directory.as_raw_fd(),
                self.name.as_ptr(),
            )
        })
    }

    pub fn remove(&self, temporary: Option<&OsStr>) -> io::Result<()> {
        let temporary = temporary.map(name_component).transpose()?;
        checked(unsafe {
            libc::unlinkat(
                self.directory.as_raw_fd(),
                temporary.as_ref().unwrap_or(&self.name).as_ptr(),
                0,
            )
        })
    }

    pub fn sync(&self) -> io::Result<()> {
        self.directory.sync_all()
    }
}

fn name_component(value: &OsStr) -> io::Result<CString> {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.contains(&b'/') || bytes == b"." || bytes == b".." {
        return Err(denied("expected a single file name"));
    }
    CString::new(bytes).map_err(|_| denied("NUL in file name"))
}
fn opened(fd: libc::c_int) -> io::Result<File> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}
fn checked(result: libc::c_int) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
