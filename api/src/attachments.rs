//! Immutable originals addressed by content, with conversation-scoped access.
//! A user-visible path is only a label; all reads originate from saved input events.
use super::{AppState, ServiceError};
use agent_core::{AttachmentReference, FileReference};
use agent_runtime::EventStore;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) const MAX_ATTACHMENT_BYTES: usize = 100 * 1024 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub(crate) fn valid_attachment_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn invalid(message: &str) -> ServiceError {
    ServiceError::BadRequest(message.into())
}

fn directory(root: &Path) -> Result<PathBuf, ServiceError> {
    let directory = root.join("attachments");
    match fs::create_dir(&directory) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.file_type().is_dir() || fs::canonicalize(&directory)? != directory {
        return Err(invalid("附件存储目录无效，不能使用符号链接"));
    }
    Ok(directory)
}

fn stored_path(root: &Path, reference: &AttachmentReference) -> Result<PathBuf, ServiceError> {
    if !valid_attachment_id(&reference.id) || reference.size > MAX_ATTACHMENT_BYTES as u64 {
        return Err(invalid("原文件附件标识或大小无效"));
    }
    let path = directory(root)?.join(&reference.id);
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| invalid("附件原文件不存在，请重新添加"))?;
    if !metadata.file_type().is_file() || metadata.len() != reference.size {
        return Err(invalid("附件原文件不完整或已被替换，请重新添加"));
    }
    if fs::canonicalize(&path)? != path {
        return Err(invalid("附件原文件不能是符号链接"));
    }
    Ok(path)
}

fn read_stored(
    root: &Path,
    reference: &AttachmentReference,
) -> Result<(PathBuf, Vec<u8>), ServiceError> {
    let path = stored_path(root, reference)?;
    let file = File::open(&path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() != reference.size {
        return Err(invalid("附件原文件不完整，请重新添加"));
    }
    let mut bytes = Vec::new();
    file.take(reference.size + 1).read_to_end(&mut bytes)?;
    // Validate the opened content, not just the earlier pathname metadata. This
    // also detects external edits and substitutions before bytes reach a model.
    if bytes.len() as u64 != reference.size || digest(&bytes) != reference.id {
        return Err(invalid("附件原文件校验失败，请重新添加"));
    }
    stored_path(root, reference)?;
    Ok((path, bytes))
}

fn write_stored(root: &Path, name: String, bytes: &[u8]) -> Result<FileReference, ServiceError> {
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(invalid("单个原文件最多 100 MiB"));
    }
    let reference = FileReference {
        name,
        path: None,
        text: String::new(),
        image: None,
        attachment: Some(AttachmentReference {
            id: digest(bytes),
            size: bytes.len() as u64,
        }),
    };
    crate::input::validate("", std::slice::from_ref(&reference))?;
    let attachment = reference.attachment.as_ref().expect("stored attachment");
    let directory = directory(root)?;
    let path = directory.join(&attachment.id);
    if fs::symlink_metadata(&path).is_ok() {
        read_stored(root, attachment)?;
        return Ok(reference);
    }
    let temporary = directory.join(format!(
        ".upload-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<(), ServiceError> {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        // Hard-link publication cannot replace an existing original. A parallel
        // identical upload publishes the same hash and is verified below.
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                read_stored(root, attachment)?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result?;
    Ok(reference)
}

impl AppState {
    pub async fn store_attachment(
        &self,
        name: String,
        bytes: Vec<u8>,
    ) -> Result<FileReference, ServiceError> {
        let root = self.inner.directory.clone();
        tokio::task::spawn_blocking(move || write_stored(&root, name, &bytes))
            .await
            .map_err(ServiceError::internal)?
    }

    pub(super) async fn validate_stored_attachments(
        &self,
        files: &[FileReference],
    ) -> Result<(), ServiceError> {
        let references: Vec<_> = files
            .iter()
            .filter_map(|file| file.attachment.clone())
            .collect();
        if references.is_empty() {
            return Ok(());
        }
        let root = self.inner.directory.clone();
        tokio::task::spawn_blocking(move || {
            for reference in references {
                read_stored(&root, &reference)?;
            }
            Ok(())
        })
        .await
        .map_err(ServiceError::internal)?
    }

    async fn attachment_reference_for_run(
        &self,
        run_id: &str,
        id: &str,
    ) -> Result<FileReference, ServiceError> {
        if !valid_attachment_id(id) {
            return Err(invalid("附件标识无效"));
        }
        let root_id = self.root_id(run_id)?;
        // Each child shares only the task family's explicit user attachments.
        // Clone handles before awaiting, never retain the global runs lock.
        let entries: Vec<_> = self
            .inner
            .runs
            .read()
            .expect("runs lock poisoned")
            .values()
            .filter(|entry| {
                let summary = entry.snapshot.get().summary;
                summary.id == root_id
                    || summary
                        .delegation
                        .as_ref()
                        .is_some_and(|d| d.root_run_id == root_id)
            })
            .cloned()
            .collect();
        for entry in entries {
            let detail = entry.snapshot.get();
            for message in detail
                .messages
                .iter()
                .filter(|message| message.role == "user" && !message.files.is_empty())
            {
                let Some(sequence) = message
                    .id
                    .strip_prefix("event-")
                    .and_then(|value| value.parse::<u64>().ok())
                else {
                    continue;
                };
                let Some(end) = sequence.checked_add(1) else {
                    continue;
                };
                let Some(stored) = entry
                    .runtime
                    .event_store()
                    .inner
                    .load_range(&detail.summary.id, sequence..end)
                    .await
                    .map_err(ServiceError::internal)?
                else {
                    continue;
                };
                for envelope in stored.events {
                    if envelope.sequence != sequence {
                        continue;
                    }
                    if let Some(file) = envelope.event.files().iter().find(|file| {
                        file.attachment
                            .as_ref()
                            .is_some_and(|reference| reference.id == id)
                    }) {
                        return Ok(file.clone());
                    }
                }
            }
        }
        Err(invalid("此附件未添加到当前任务，不能读取其他会话的文件"))
    }

    pub(crate) async fn attachment_path_for_run(
        &self,
        run_id: &str,
        id: &str,
    ) -> Result<(FileReference, PathBuf), ServiceError> {
        let file = self.attachment_reference_for_run(run_id, id).await?;
        let reference = file
            .attachment
            .clone()
            .expect("authorized stored attachment");
        let root = self.inner.directory.clone();
        let path = tokio::task::spawn_blocking(move || {
            read_stored(&root, &reference).map(|(path, _)| path)
        })
        .await
        .map_err(ServiceError::internal)??;
        Ok((file, path))
    }

    pub(crate) async fn attachment_bytes_for_run(
        &self,
        run_id: &str,
        id: &str,
    ) -> Result<(FileReference, Vec<u8>), ServiceError> {
        let file = self.attachment_reference_for_run(run_id, id).await?;
        let bytes = self.read_attachment_original(&file).await?;
        Ok((file, bytes))
    }

    pub(crate) async fn read_attachment_original(
        &self,
        file: &FileReference,
    ) -> Result<Vec<u8>, ServiceError> {
        let reference = file
            .attachment
            .clone()
            .ok_or_else(|| invalid("此附件没有保存原文件"))?;
        let root = self.inner.directory.clone();
        tokio::task::spawn_blocking(move || read_stored(&root, &reference).map(|(_, bytes)| bytes))
            .await
            .map_err(ServiceError::internal)?
    }
}

#[cfg(test)]
#[path = "attachment_tests.rs"]
mod tests;
