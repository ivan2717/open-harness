//! File operations anchored to verified directories, rather than a checked
//! pathname that can later be redirected through a replaced ancestor.
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use unix::ParentDirectory;
#[cfg(windows)]
pub(crate) use windows::ParentDirectory;

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::{Read, Write},
        sync::atomic::{AtomicU64, Ordering},
    };

    #[test]
    fn held_parent_cannot_redirect_reads_or_replacements_outside_its_directory() {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "openharness-parent-handle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(base.join("original")).unwrap();
        fs::create_dir_all(base.join("outside")).unwrap();
        let base = fs::canonicalize(base).unwrap();
        fs::write(base.join("original/value.txt"), "original").unwrap();
        fs::write(base.join("outside/value.txt"), "outside").unwrap();
        let parent = ParentDirectory::open(&base.join("original/value.txt"), false).unwrap();
        #[cfg(unix)]
        {
            fs::rename(base.join("original"), base.join("moved")).unwrap();
            std::os::unix::fs::symlink(base.join("outside"), base.join("original")).unwrap();
            assert!(ParentDirectory::open(&base.join("original/value.txt"), false).is_err());
            assert!(ParentDirectory::open(&base.join("original/new/target.txt"), true).is_err());
            assert!(!base.join("outside/new").exists());
        }
        #[cfg(windows)]
        assert!(fs::rename(base.join("original"), base.join("moved")).is_err());
        let mut text = String::new();
        parent.read().unwrap().read_to_string(&mut text).unwrap();
        assert_eq!(text, "original");
        let temporary = std::ffi::OsStr::new("replacement");
        let mut file = parent.create(temporary).unwrap();
        file.write_all(b"new original").unwrap();
        drop(file);
        parent.replace(temporary).unwrap();
        assert_eq!(
            fs::read_to_string(base.join("outside/value.txt")).unwrap(),
            "outside"
        );
        parent.remove(None).unwrap();
        assert_eq!(
            fs::read_to_string(base.join("outside/value.txt")).unwrap(),
            "outside"
        );
        drop(parent);
        fs::remove_dir_all(base).unwrap();
    }
}
