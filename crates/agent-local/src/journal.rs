use crate::{CommandRecord, FileChange, paths};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SavedFile {
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_permissions: Option<SavedPermissions>,
    pub after: Option<String>,
    pub owned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SavedPermissions {
    pub readonly: bool,
    pub unix_mode: Option<u32>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Journal {
    root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root_identity: Option<DirectoryIdentity>,
    pub files: BTreeMap<String, SavedFile>,
    pub commands: Vec<CommandRecord>,
    /// An unfinished command never replays. Its pre-execution snapshot permits
    /// inspecting effects even if the application exited before reconciliation.
    pub command_baseline: Option<BTreeMap<String, String>>,
}

impl Journal {
    pub fn load(directory: &Path, root: &Path, _metadata: &fs::Metadata) -> io::Result<Self> {
        let path = directory.join("journal.json");
        let mut journal: Self = if path.exists() {
            let bytes = fs::read(path)?;
            if bytes.len() > 40 * 1024 * 1024 {
                return Err(io::Error::other("workspace journal exceeds size limit"));
            }
            serde_json::from_slice(&bytes).map_err(io::Error::other)?
        } else {
            Self {
                root: root.display().to_string(),
                ..Self::default()
            }
        };
        if journal.root != root.display().to_string() {
            return Err(io::Error::other("journal belongs to another workspace"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let identity = DirectoryIdentity {
                device: _metadata.dev(),
                inode: _metadata.ino(),
            };
            if journal
                .root_identity
                .as_ref()
                .is_some_and(|saved| saved != &identity)
            {
                return Err(paths::denied(
                    "workspace directory was replaced since this session was saved; select the project in a new session",
                ));
            }
            // Legacy journals had only a path. Bind them to the observed
            // directory when first opened by this version.
            journal.root_identity = Some(identity);
        }
        if let Some(baseline) = journal.command_baseline.as_ref()
            && let Ok(after) = snapshot(root)
        {
            let baseline = baseline.clone();
            journal.reconcile_command(baseline, after);
            journal.command_baseline = None;
        }
        Ok(journal)
    }

    pub fn save(&self, directory: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > 40 * 1024 * 1024 {
            return Err(io::Error::other("workspace journal exceeds size limit"));
        }
        let path = directory.join("journal.tmp");
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(path, directory.join("journal.json"))?;
        #[cfg(unix)]
        fs::File::open(directory)?.sync_all()?;
        Ok(())
    }

    pub fn changes(&self, root: &Path) -> io::Result<Vec<FileChange>> {
        let mut result = Vec::new();
        for (path, saved) in &self.files {
            match paths::resolve(root, path).and_then(|target| paths::text_file(&target)) {
                Ok(current) if current == saved.before => (),
                Ok(current) => result.push(FileChange {
                    path: path.clone(),
                    status: if current != saved.after {
                        "externally_modified"
                    } else if saved.before.is_none() {
                        "added"
                    } else if current.is_none() {
                        "deleted"
                    } else {
                        "modified"
                    }
                    .into(),
                    before: saved.before.clone(),
                    revertible: saved.owned && current == saved.after,
                    after: current,
                }),
                Err(_) => result.push(FileChange {
                    path: path.clone(),
                    status: "unavailable".into(),
                    before: saved.before.clone(),
                    after: None,
                    revertible: false,
                }),
            }
        }
        Ok(result)
    }

    pub fn reconcile_command(
        &mut self,
        before: BTreeMap<String, String>,
        after: BTreeMap<String, String>,
    ) {
        for path in before.keys().chain(after.keys()) {
            let previous = before.get(path).cloned();
            let current = after.get(path).cloned();
            if previous == current {
                continue;
            }
            self.files
                .entry(path.clone())
                .and_modify(|saved| {
                    saved.after = current.clone();
                    saved.owned = false;
                })
                .or_insert(SavedFile {
                    before_permissions: None,
                    before: previous,
                    after: current,
                    owned: false,
                });
        }
    }
}

pub(crate) fn snapshot(root: &Path) -> io::Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    let mut total = 0;
    let mut visited = 0;
    walk(root, root, &mut files, &mut total, &mut visited, 0)?;
    Ok(files)
}

fn walk(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, String>,
    total: &mut usize,
    visited: &mut usize,
    depth: usize,
) -> io::Result<()> {
    if depth > 64 {
        return Err(io::Error::other(
            "workspace exceeds maximum directory depth of 64",
        ));
    }
    for entry in fs::read_dir(directory)? {
        *visited += 1;
        if *visited > 16384 {
            return Err(io::Error::other(
                "workspace scan exceeds 16384 entries; select a smaller project directory",
            ));
        }
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if paths::skipped_directory(name) {
            continue;
        }
        let metadata = entry.file_type()?;
        if metadata.is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            walk(root, &entry.path(), files, total, visited, depth + 1)?;
        } else if metadata.is_file()
            && let Ok(Some(content)) = paths::text_file(&entry.path())
        {
            *total += content.len();
            if files.len() >= 4096 || *total > 16 * 1024 * 1024 {
                return Err(io::Error::other(
                    "workspace snapshot exceeds 4096 files or 16 MiB; select a smaller project directory",
                ));
            }
            files.insert(
                entry
                    .path()
                    .strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_string_lossy()
                    .into_owned(),
                content,
            );
        }
    }
    Ok(())
}
