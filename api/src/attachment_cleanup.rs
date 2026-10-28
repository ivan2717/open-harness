//! Startup-only collection of abandoned uploads. Durable logs, not UI snapshots,
//! determine reachability; an incomplete inventory prevents every deletion.
use super::attachments::{MAX_ATTACHMENT_BYTES, valid_attachment_id};
use agent_runtime::JsonlEventStore;
use std::{
    collections::BTreeSet,
    fs::{self, File, Metadata},
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const EVENT_PAGE: u64 = 8;

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    length: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
impl Stamp {
    fn from_metadata(metadata: &Metadata) -> io::Result<Self> {
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            },
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Log {
    path: PathBuf,
    run_id: String,
    stamp: Stamp,
}

fn regular_directory(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && fs::canonicalize(path)? == path => {
            Ok(true)
        }
        Ok(_) => Err(io::Error::other(
            "attachment cleanup found a linked or invalid storage directory",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn run_id(name: &str) -> io::Result<String> {
    let invalid = || io::Error::other("attachment cleanup found an unrecognized event log");
    let hex = name
        .strip_prefix("run-")
        .and_then(|name| name.strip_suffix(".jsonl"))
        .ok_or_else(invalid)?;
    if hex.is_empty()
        || hex.len() % 2 != 0
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid());
    }
    let bytes = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII hexadecimal"), 16)
                .map_err(|_| invalid())
        })
        .collect::<io::Result<Vec<_>>>()?;
    String::from_utf8(bytes).map_err(|_| invalid())
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    path: PathBuf,
    run_id: String,
    stamp: Stamp,
}

#[derive(Debug, PartialEq, Eq)]
struct Inventory {
    logs: Vec<Log>,
    snapshots: Vec<Snapshot>,
}

fn inventory(root: &Path) -> io::Result<Inventory> {
    // Snapshots are not the source of reachability, but a snapshot with no
    // durable log signals lost evidence. Never interpret that loss as an orphan.
    #[derive(serde::Deserialize)]
    struct Identity {
        summary: Summary,
    }
    #[derive(serde::Deserialize)]
    struct Summary {
        id: String,
    }

    let directory = root.join("runs");
    if !regular_directory(&directory)? {
        return Err(io::Error::other(
            "attachment cleanup cannot inventory run snapshots",
        ));
    }
    let mut snapshots = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || fs::canonicalize(&path)? != path {
            return Err(io::Error::other(
                "attachment cleanup cannot follow linked run snapshots",
            ));
        }
        // Deserialize only the identity; serde skips large message/image fields
        // without allocating their contents or hydrating sidecars.
        let identity: Identity = serde_json::from_reader(BufReader::new(File::open(&path)?))
            .map_err(io::Error::other)?;
        let id = identity.summary.id;
        if !super::valid_id(&id)
            || path.file_stem().and_then(|name| name.to_str()) != Some(id.as_str())
        {
            return Err(io::Error::other(
                "attachment cleanup found an invalid run snapshot identity",
            ));
        }
        snapshots.push(Snapshot {
            path,
            run_id: id,
            stamp: Stamp::from_metadata(&metadata)?,
        });
    }
    snapshots.sort_by(|left, right| left.path.cmp(&right.path));
    let logs = inventory_logs(root)?;
    let logged: BTreeSet<_> = logs
        .iter()
        .filter(|log| log.stamp.length > 0)
        .map(|log| log.run_id.as_str())
        .collect();
    if snapshots
        .iter()
        .any(|snapshot| !logged.contains(snapshot.run_id.as_str()))
    {
        return Err(io::Error::other(
            "attachment cleanup found a run snapshot with missing event history",
        ));
    }
    Ok(Inventory { logs, snapshots })
}

fn inventory_logs(root: &Path) -> io::Result<Vec<Log>> {
    let directory = root.join("events");
    if !regular_directory(&directory)? {
        // Missing logs alongside run snapshots may indicate lost history, not
        // proof that every stored original is disposable.
        if fs::read_dir(root.join("runs"))?.next().is_some() {
            return Err(io::Error::other(
                "attachment cleanup cannot inventory missing event history",
            ));
        }
        return Ok(Vec::new());
    }
    let mut logs = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if name == "images" {
            regular_directory(&path)?;
            continue;
        }
        let name = name
            .to_str()
            .ok_or_else(|| io::Error::other("event log filename is not UTF-8"))?;
        let run_id = run_id(name)?;
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || fs::canonicalize(&path)? != path {
            return Err(io::Error::other(
                "attachment cleanup cannot follow linked event logs",
            ));
        }
        // Cleanup must not repair unknown/orphaned logs while deciding whether
        // their attachments are reachable. Recovery belongs to the event store.
        if metadata.len() > 0 {
            let mut file = File::open(&path)?;
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last != [b'\n'] {
                return Err(io::Error::other(
                    "attachment cleanup found an incomplete event log",
                ));
            }
        }
        logs.push(Log {
            path,
            run_id,
            stamp: Stamp::from_metadata(&metadata)?,
        });
    }
    logs.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(logs)
}

/// Invoke only before the host accepts uploads or messages. Pending UI drafts
/// have no active leases at startup, and recent abandoned files retain a grace period.
pub(super) async fn collect(root: PathBuf, now: SystemTime) -> io::Result<usize> {
    let root = fs::canonicalize(root)?;
    let attachments = root.join("attachments");
    if !regular_directory(&attachments)? {
        return Ok(0);
    }
    let candidates = {
        let attachments = attachments.clone();
        tokio::task::spawn_blocking(move || {
            let mut candidates = Vec::new();
            for entry in fs::read_dir(&attachments)? {
                let entry = entry?;
                let name = entry.file_name();
                let Some(id) = name.to_str().filter(|id| valid_attachment_id(id)) else {
                    continue;
                };
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path)?;
                if !metadata.file_type().is_file() || fs::canonicalize(&path)? != path {
                    continue;
                }
                if now.duration_since(metadata.modified()?).unwrap_or_default() < RETENTION {
                    continue;
                }
                candidates.push((id.to_owned(), path, Stamp::from_metadata(&metadata)?));
            }
            Ok::<_, io::Error>(candidates)
        })
        .await
        .map_err(io::Error::other)??
    };
    if candidates.is_empty() {
        return Ok(0);
    }
    let history = {
        let root = root.clone();
        tokio::task::spawn_blocking(move || inventory(&root))
            .await
            .map_err(io::Error::other)??
    };
    let store = JsonlEventStore::new(root.join("events"));
    let mut retained = BTreeSet::new();
    for log in &history.logs {
        let mut offset = 0_u64;
        loop {
            let end = offset
                .checked_add(EVENT_PAGE)
                .ok_or_else(|| io::Error::other("event sequence overflow"))?;
            let stored = store
                .load_range_metadata(&log.run_id, offset..end)
                .await
                .map_err(io::Error::other)?;
            let Some(stored) = stored else {
                if log.stamp.length != 0 {
                    return Err(io::Error::other(
                        "event log disappeared during attachment cleanup",
                    ));
                }
                break;
            };
            for event in &stored.events {
                for file in event.event.files() {
                    if let Some(reference) = &file.attachment {
                        if !valid_attachment_id(&reference.id)
                            || reference.size > MAX_ATTACHMENT_BYTES as u64
                        {
                            return Err(io::Error::other("invalid persisted attachment reference"));
                        }
                        retained.insert(reference.id.clone());
                    }
                }
            }
            if stored.events.len() < EVENT_PAGE as usize {
                break;
            }
            offset = end;
        }
    }
    tokio::task::spawn_blocking(move || {
        // Nothing has been removed yet. Recheck the complete inventory before
        // deleting, so an external writer/new log aborts this cleanup cycle.
        if inventory(&root)? != history {
            return Err(io::Error::other(
                "event history changed during attachment cleanup",
            ));
        }
        let mut removed = 0;
        for (id, path, stamp) in candidates {
            if retained.contains(&id) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() || Stamp::from_metadata(&metadata)? != stamp {
                continue;
            }
            fs::remove_file(path)?;
            removed += 1;
        }
        Ok(removed)
    })
    .await
    .map_err(io::Error::other)?
}

#[cfg(test)]
#[path = "attachment_cleanup_tests.rs"]
mod tests;
