use crate::paths::denied;
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io,
    os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFileInformationByHandle, GetFinalPathNameByHandleW,
};

pub(crate) struct ParentDirectory {
    // No DELETE sharing: an ancestor cannot be renamed/replaced while an
    // operation still uses an absolute Win32 path beneath these handles.
    _ancestors: Vec<File>,
    parent: PathBuf,
    target: PathBuf,
}

impl ParentDirectory {
    pub fn open(path: &Path, create: bool) -> io::Result<Self> {
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(denied("file access requires an absolute file path"));
        }
        let parent = path
            .parent()
            .ok_or_else(|| denied("cannot access the filesystem root"))?;
        let mut ancestors = Vec::new();
        for directory in parent
            .ancestors()
            .filter(|path| path.has_root())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let open = || {
                OpenOptions::new()
                    .access_mode(FILE_READ_ATTRIBUTES)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(directory)
            };
            let file = match open() {
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    match fs::create_dir(directory) {
                        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {
                            return Err(error);
                        }
                        _ => (),
                    }
                    open()?
                }
                result => result?,
            };
            check_path(&file, directory)?;
            if !file.metadata()?.is_dir() {
                return Err(denied("file ancestor is not a directory"));
            }
            ancestors.push(file);
        }
        Ok(Self {
            _ancestors: ancestors,
            parent: parent.into(),
            target: path.into(),
        })
    }

    pub fn read(&self) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&self.target)?;
        check_path(&file, &self.target)?;
        Self::check_file(&file)?;
        Ok(file)
    }

    pub fn check_file(file: &File) -> io::Result<()> {
        let info = info(file)?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || info.nNumberOfLinks != 1
            || !file.metadata()?.is_file()
        {
            return Err(denied(
                "expected a regular file without hard links or reparse points",
            ));
        }
        Ok(())
    }

    pub fn create(&self, temporary: &OsStr) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.parent.join(temporary))
    }
    pub fn replace(&self, temporary: &OsStr) -> io::Result<()> {
        fs::rename(self.parent.join(temporary), &self.target)
    }
    pub fn remove(&self, temporary: Option<&OsStr>) -> io::Result<()> {
        fs::remove_file(
            temporary.map_or_else(|| self.target.clone(), |name| self.parent.join(name)),
        )
    }
    pub fn sync(&self) -> io::Result<()> {
        Ok(())
    }
}

fn info(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

fn check_path(file: &File, expected: &Path) -> io::Result<()> {
    if info(file)?.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(denied("reparse points are not accessible"));
    }
    let mut buffer = vec![0u16; 32768];
    let len = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    if len as usize >= buffer.len() {
        return Err(denied("resolved file path is too long"));
    }
    let actual = PathBuf::from(OsString::from_wide(&buffer[..len as usize]));
    // LocalTools roots are canonical verbatim Windows paths. This comparison
    // uses the opened handle, not a second pathname resolution of the target.
    if !actual
        .as_os_str()
        .eq_ignore_ascii_case(expected.as_os_str())
    {
        return Err(denied("file path changed while opening"));
    }
    Ok(())
}
