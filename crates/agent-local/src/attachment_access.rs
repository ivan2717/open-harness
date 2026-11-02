//! Exact, host-authorized original files. Never grant their containing store.
use crate::{LocalTools, lock_error};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

impl LocalTools {
    /// The host must first resolve a persisted attachment in this task's scope.
    /// This is not a model-facing filesystem grant API.
    pub fn allow_attachment_read(&self, path: &Path) -> io::Result<()> {
        let metadata = checked_file(path)?;
        self.inner
            .attachment_reads
            .lock()
            .map_err(lock_error)?
            .insert(path.to_owned(), metadata);
        Ok(())
    }

    pub(crate) fn attachment_read_paths(&self) -> io::Result<Vec<PathBuf>> {
        let files = self.inner.attachment_reads.lock().map_err(lock_error)?;
        for (path, previous) in files.iter() {
            let metadata = checked_file(path)?;
            if metadata.len() != previous.len() || metadata.modified()? != previous.modified()? {
                return Err(io::Error::other(
                    "attachment changed after it was authorized",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.dev() != previous.dev() || metadata.ino() != previous.ino() {
                    return Err(io::Error::other("attachment identity changed"));
                }
            }
        }
        Ok(files.keys().cloned().collect())
    }
}

fn checked_file(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if !path.is_absolute()
        || !metadata.is_file()
        || metadata.file_type().is_symlink()
        || fs::canonicalize(path)? != path
    {
        return Err(io::Error::other(
            "attachment must be an exact, regular file",
        ));
    }
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants_only_one_file_and_rejects_replacement() {
        let temp = std::env::temp_dir().join(format!("attachment-access-{}", std::process::id()));
        fs::create_dir_all(temp.join("project")).unwrap();
        let temp = fs::canonicalize(temp).unwrap();
        let tools = LocalTools::open(
            temp.join("project"),
            temp.join("state"),
            crate::PermissionMode::ReadOnly,
        )
        .unwrap();
        let file = temp.join("original");
        fs::write(&file, b"original").unwrap();
        tools.allow_attachment_read(&file).unwrap();
        assert_eq!(tools.attachment_read_paths().unwrap(), vec![file.clone()]);
        assert!(tools.allow_attachment_read(&temp).is_err());
        fs::write(&file, b"different content").unwrap();
        assert!(tools.attachment_read_paths().is_err());
        fs::remove_dir_all(temp).unwrap();
    }
}
