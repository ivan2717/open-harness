use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Unsafe Chrome native host path",
    )
}

fn has_expansion(path: &Path) -> io::Result<bool> {
    let path = path.to_str().ok_or_else(invalid)?;
    Ok(path.contains(['%', '!']))
}

fn regular_executable(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || !path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        || !fs::symlink_metadata(path)?.file_type().is_file()
    {
        return Err(invalid());
    }
    Ok(())
}

fn safe_path(
    path: &Path,
    short_path: impl FnOnce(&Path) -> io::Result<PathBuf>,
) -> io::Result<PathBuf> {
    regular_executable(path)?;
    if !has_expansion(path)? {
        return Ok(path.to_owned());
    }
    // Some Chrome versions pass the manifest path through cmd.exe. Quoting
    // protects spaces and &, but does not prevent %VAR% or !VAR! expansion.
    let short = short_path(path)?;
    regular_executable(&short)?;
    if has_expansion(&short)? || fs::canonicalize(path)? != fs::canonicalize(&short)? {
        return Err(invalid());
    }
    Ok(short)
}

#[cfg(windows)]
fn short_path(path: &Path) -> io::Result<PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;

    let mut input: Vec<u16> = path.as_os_str().encode_wide().collect();
    if input.contains(&0) || input.len() >= 32_768 {
        return Err(invalid());
    }
    input.push(0);
    // SAFETY: input is a null-terminated UTF-16 path; a null output asks only
    // for the required capacity, including its terminator.
    let capacity = unsafe { GetShortPathNameW(input.as_ptr(), std::ptr::null_mut(), 0) };
    if capacity == 0 {
        return Err(io::Error::last_os_error());
    }
    if capacity > 32_768 {
        return Err(invalid());
    }
    let mut output = vec![0; capacity as usize];
    // SAFETY: output has capacity UTF-16 units and input remains alive. The
    // return value excludes the terminator on success.
    let length = unsafe { GetShortPathNameW(input.as_ptr(), output.as_mut_ptr(), capacity) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length >= capacity {
        return Err(invalid());
    }
    output.truncate(length as usize);
    Ok(PathBuf::from(OsString::from_wide(&output)))
}

#[cfg(not(windows))]
fn short_path(_path: &Path) -> io::Result<PathBuf> {
    Err(invalid())
}

/// Read-only setup query. Native Messaging and MCP modes never emit this JSON.
pub fn query(args: &[OsString]) -> io::Result<u8> {
    if args.len() != 1 {
        return Err(invalid());
    }
    let path = safe_path(Path::new(&args[0]), short_path)?;
    let mut output = serde_json::to_vec(path.to_str().ok_or_else(invalid)?)?;
    output.push(b'\n');
    let mut stdout = io::stdout().lock();
    stdout.write_all(&output)?;
    stdout.flush()?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let directory = std::env::temp_dir().join(format!(
                "oh-native-path-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&directory).unwrap();
            Self(directory)
        }

        fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, b"fixture").unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn ordinary_existing_executable_path_is_returned_unchanged() {
        let fixture = Fixture::new();
        let path = fixture.file("native 中文 & host.EXE");
        assert_eq!(
            safe_path(&path, |_| panic!("ordinary paths need no short name")).unwrap(),
            path,
        );
    }

    #[test]
    fn relative_missing_non_executable_and_directory_paths_are_rejected() {
        let fixture = Fixture::new();
        for path in [
            PathBuf::from("relative.exe"),
            fixture.0.join("missing.exe"),
            fixture.file("script.cmd"),
            fixture.0.clone(),
        ] {
            assert!(safe_path(&path, |_| panic!("invalid paths need no lookup")).is_err());
        }
        assert!(query(&[]).is_err());
        assert!(query(&["one.exe".into(), "two.exe".into()]).is_err());
    }

    #[test]
    fn unsafe_missing_or_unrelated_short_names_are_rejected() {
        let fixture = Fixture::new();
        for name in ["host %PATH%.exe", "host !PATH!.exe"] {
            let path = fixture.file(name);
            assert!(safe_path(&path, |_| Err(invalid())).is_err());
            assert!(safe_path(&path, |_| Ok(path.clone())).is_err());
            assert!(safe_path(&path, |_| Ok(fixture.0.join("missing.exe"))).is_err());
            assert!(safe_path(&path, |_| Ok(fixture.file("unrelated.exe"))).is_err());
        }
    }

    #[test]
    #[cfg(unix)]
    fn safe_alias_must_resolve_to_the_original_regular_file() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("unsafe %PATH% !PATH!");
        fs::create_dir(&directory).unwrap();
        let original = directory.join("host.exe");
        fs::write(&original, b"fixture").unwrap();
        let alias = fixture.0.join("safe");
        // A directory alias simulates Windows long/short names without allowing
        // a final executable symlink to pass the regular-file check.
        std::os::unix::fs::symlink(&directory, &alias).unwrap();
        let safe = alias.join("host.exe");
        assert_eq!(safe_path(&original, |_| Ok(safe.clone())).unwrap(), safe);

        let link = fixture.0.join("linked.exe");
        std::os::unix::fs::symlink(&original, &link).unwrap();
        assert!(safe_path(&link, |_| panic!("symlinks must fail")).is_err());
    }
}
