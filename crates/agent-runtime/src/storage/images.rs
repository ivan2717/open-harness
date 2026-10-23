//! Content-addressed sidecars keep screenshots out of append-only event logs.
//! Only hexadecimal content IDs are read; logs cannot nominate arbitrary paths.
use super::EventStoreError;
use agent_core::{
    MAX_IMAGE_BYTES, ModelImage,
    agent::{RunEvent, RunEventEnvelope},
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

const PREFIX: &str = "sha256:";
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

fn write_image(directory: &Path, hash: &str, bytes: &[u8]) -> Result<(), EventStoreError> {
    let path = directory.join(hash);
    if fs::symlink_metadata(&path).is_ok() {
        read_image(directory, hash)?;
        return Ok(());
    }
    // Publish complete files atomically. Different runs may capture identical
    // pixels concurrently; no reader should see the other's half-written file.
    let temporary = directory.join(format!(
        ".image-{}-{}",
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
    let result = (|| {
        let mut file = options
            .open(&temporary)
            .map_err(|e| EventStoreError::io("create image", temporary.clone(), e))?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| EventStoreError::io("write image", temporary.clone(), e))?;
        fs::rename(&temporary, &path).map_err(|e| EventStoreError::io("publish image", path, e))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn invalid(message: &'static str) -> EventStoreError {
    EventStoreError::backend(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message,
    ))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn image_path(directory: &Path, hash: &str) -> Result<std::path::PathBuf, EventStoreError> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(invalid("invalid image content ID"));
    }
    let path = directory.join(hash);
    let meta = fs::symlink_metadata(&path)
        .map_err(|e| EventStoreError::io("inspect image", path.clone(), e))?;
    if !meta.file_type().is_file() || meta.len() > MAX_IMAGE_BYTES as u64 {
        return Err(invalid("invalid image sidecar"));
    }
    Ok(path)
}

fn read_image(directory: &Path, hash: &str) -> Result<Vec<u8>, EventStoreError> {
    let path = image_path(directory, hash)?;
    let file = File::open(&path).map_err(|e| EventStoreError::io("open image", path.clone(), e))?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| EventStoreError::io("read image", path, e))?;
    if bytes.len() > MAX_IMAGE_BYTES || digest(&bytes) != hash {
        return Err(invalid("image sidecar checksum mismatch"));
    }
    Ok(bytes)
}

fn image_directory(log: &Path) -> Result<std::path::PathBuf, EventStoreError> {
    let directory = log
        .parent()
        .ok_or_else(|| invalid("event log has no parent"))?
        .join("images");
    if let Ok(meta) = fs::symlink_metadata(&directory)
        && !meta.file_type().is_dir()
    {
        return Err(invalid("image directory must not be a symlink"));
    }
    Ok(directory)
}

pub(super) fn externalize(
    log: &Path,
    event: &RunEventEnvelope,
) -> Result<RunEventEnvelope, EventStoreError> {
    let mut stored = event.clone();
    if let RunEvent::ToolCallSucceeded { images, .. } = &mut stored.event {
        if images.is_empty() {
            return Ok(stored);
        }
        if images.len() > agent_core::MAX_TOOL_IMAGES {
            return Err(invalid("too many tool images"));
        }
        let directory = image_directory(log)?;
        fs::create_dir_all(&directory)
            .map_err(|e| EventStoreError::io("create image directory", directory.clone(), e))?;
        for image in images {
            if let Some(hash) = image.data_base64.strip_prefix(PREFIX) {
                // Referenced historical images still undergo content and size
                // validation; unloading must not mask damaged source evidence.
                let bytes = read_image(&directory, hash)?;
                ModelImage::from_bytes(image.mime_type.clone(), &bytes)
                    .map_err(EventStoreError::backend)?;
                continue;
            }
            let bytes = image.decoded_bytes().map_err(EventStoreError::backend)?;
            let hash = digest(&bytes);
            write_image(&directory, &hash, &bytes)?;
            image.data_base64 = format!("{PREFIX}{hash}");
        }
        #[cfg(unix)]
        File::open(&directory)
            .and_then(|file| file.sync_all())
            .map_err(|e| EventStoreError::io("sync image directory", directory, e))?;
    }
    Ok(stored)
}

/// Runtime restoration keeps historical references without re-reading every
/// raster on every model round. Full archive reads verify checksums on demand.
pub(super) fn retain_reference(
    log: &Path,
    event: RunEventEnvelope,
) -> Result<RunEventEnvelope, EventStoreError> {
    if let RunEvent::ToolCallSucceeded { images, .. } = &event.event {
        if images.len() > agent_core::MAX_TOOL_IMAGES {
            return Err(invalid("too many tool images"));
        }
        for image in images {
            if let Some(hash) = image.data_base64.strip_prefix(PREFIX) {
                image_path(&image_directory(log)?, hash)?;
            } else {
                // Older inline journals migrate lazily; normal sidecar logs
                // never allocate old raster payloads on runtime restoration.
                return externalize(log, &event);
            }
        }
    }
    Ok(event)
}

pub(super) fn hydrate(
    log: &Path,
    mut event: RunEventEnvelope,
) -> Result<RunEventEnvelope, EventStoreError> {
    if let RunEvent::ToolCallSucceeded { images, .. } = &mut event.event {
        if images.len() > agent_core::MAX_TOOL_IMAGES {
            return Err(invalid("too many tool images"));
        }
        for image in images {
            if let Some(hash) = image.data_base64.strip_prefix(PREFIX) {
                let bytes = read_image(&image_directory(log)?, hash)?;
                *image = ModelImage::from_bytes(image.mime_type.clone(), &bytes)
                    .map_err(EventStoreError::backend)?;
            } else {
                image.validate().map_err(EventStoreError::backend)?;
            }
        }
    }
    Ok(event)
}
