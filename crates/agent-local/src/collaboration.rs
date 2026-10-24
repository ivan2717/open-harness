//! Shared workspace coordination for delegated runs. A lease reserves exact
//! files, never a directory subtree; it does not grant additional permissions.
use crate::{LocalTools, lock_error, paths};
use agent_runtime::ToolDefinition;
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone)]
pub struct WorkspaceAccess {
    inner: Arc<Shared>,
}

struct Shared {
    root: PathBuf,
    operations: RwLock<()>,
    leases: Mutex<BTreeMap<String, Vec<PathBuf>>>,
}

impl WorkspaceAccess {
    pub fn new(root: PathBuf) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(io::Error::other("workspace must be a directory"));
        }
        Ok(Self {
            inner: Arc::new(Shared {
                root,
                operations: RwLock::new(()),
                leases: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    /// Atomically reserve files after any active command or file mutation ends.
    /// Cancelling while waiting leaves no reservation. Repeating the same lease
    /// is idempotent; replacing one requires an explicit release first.
    pub async fn acquire(&self, owner: &str, paths: &[String]) -> io::Result<()> {
        validate_owner(owner)?;
        let _operation = self.inner.operations.write().await;
        self.acquire_locked(owner, paths)
    }

    /// Non-waiting admission for hosts that hold a short-lived scheduling lock.
    /// A busy workspace returns `WouldBlock` without retaining any reservation.
    pub fn try_acquire(&self, owner: &str, paths: &[String]) -> io::Result<()> {
        validate_owner(owner)?;
        let _operation = self.inner.operations.try_write().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "workspace is busy; wait for the current operation before delegating writes",
            )
        })?;
        self.acquire_locked(owner, paths)
    }

    fn acquire_locked(&self, owner: &str, paths: &[String]) -> io::Result<()> {
        let paths = self.normalize_paths(paths)?;
        let mut leases = self.inner.leases.lock().map_err(lock_error)?;
        if let Some(existing) = leases.get(owner) {
            return if existing == &paths {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "release the existing file lease before replacing it",
                ))
            };
        }
        for (other, reserved) in leases.iter() {
            if paths
                .iter()
                .any(|path| reserved.iter().any(|reserved| overlaps(path, reserved)))
            {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("a requested file overlaps a lease held by {other}"),
                ));
            }
        }
        if !paths.is_empty() {
            leases.insert(owner.to_owned(), paths);
        }
        Ok(())
    }

    /// Call after stopping the delegated run. The operation lock also waits for
    /// an in-flight write, so a released lease cannot race a parent command.
    pub async fn release(&self, owner: &str) {
        let _operation = self.inner.operations.write().await;
        self.inner
            .leases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(owner);
    }

    fn normalize_paths(&self, paths: &[String]) -> io::Result<Vec<PathBuf>> {
        let mut normalized = Vec::new();
        for path in paths {
            let target = paths::resolve(&self.inner.root, path)?;
            if target == self.inner.root
                || fs::symlink_metadata(&target).is_ok_and(|metadata| !metadata.is_file())
            {
                return Err(paths::denied("write leases must name individual files"));
            }
            let relative = target.strip_prefix(&self.inner.root).unwrap().to_path_buf();
            if normalized.contains(&relative) {
                continue;
            }
            if normalized
                .iter()
                .any(|existing| overlaps(existing, &relative))
            {
                return Err(paths::denied("overlapping paths in write lease"));
            }
            normalized.push(relative);
        }
        normalized.sort();
        Ok(normalized)
    }

    pub(crate) async fn operation(&self, write: bool) -> WorkspaceOperation<'_> {
        if write {
            WorkspaceOperation::Write {
                _guard: self.inner.operations.write().await,
            }
        } else {
            WorkspaceOperation::Read {
                _guard: self.inner.operations.read().await,
            }
        }
    }

    pub(crate) fn try_write(&self) -> io::Result<RwLockWriteGuard<'_, ()>> {
        self.inner.operations.try_write().map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "wait for the running workspace operation before reverting",
            )
        })
    }

    pub(crate) fn try_read(&self) -> Option<RwLockReadGuard<'_, ()>> {
        self.inner.operations.try_read().ok()
    }
}

pub(crate) enum WorkspaceOperation<'a> {
    Read { _guard: RwLockReadGuard<'a, ()> },
    Write { _guard: RwLockWriteGuard<'a, ()> },
}

pub(crate) struct Collaboration {
    pub access: WorkspaceAccess,
    owner: String,
    allowed: Option<Vec<PathBuf>>,
    pub definitions: Vec<ToolDefinition>,
}

impl LocalTools {
    /// `None` is the parent role; `Some` restricts a delegate to exact files,
    /// and an empty list makes it read-only. A delegate must also hold a lease.
    /// Bind all tools for one workspace to the same `WorkspaceAccess` instance.
    pub fn with_collaboration(
        mut self,
        access: WorkspaceAccess,
        run_id: impl Into<String>,
        allowed_write_paths: Option<Vec<String>>,
    ) -> io::Result<Self> {
        self.verify_root()?;
        if self.root() != access.inner.root {
            return Err(paths::denied(
                "collaboration access belongs to a different workspace",
            ));
        }
        let owner = run_id.into();
        validate_owner(&owner)?;
        let allowed = allowed_write_paths
            .map(|paths| access.normalize_paths(&paths))
            .transpose()?;
        let mut definitions = self.inner.definitions.clone();
        if let Some(paths) = &allowed {
            definitions.retain(|tool| {
                tool.name != "run_command" && (tool.name != "apply_patch" || !paths.is_empty())
            });
        }
        self.collaboration = Some(Arc::new(Collaboration {
            access,
            owner,
            allowed,
            definitions,
        }));
        Ok(self)
    }

    pub(crate) fn authorize_write(&self, path: &str) -> io::Result<PathBuf> {
        let target = paths::resolve(self.root(), path)?;
        if let Some(binding) = &self.collaboration {
            let relative = target.strip_prefix(self.root()).unwrap();
            let leases = binding.access.inner.leases.lock().map_err(lock_error)?;
            if let Some(allowed) = &binding.allowed
                && (!allowed.iter().any(|path| path == relative)
                    || !leases
                        .get(&binding.owner)
                        .is_some_and(|paths| paths.iter().any(|path| path == relative)))
            {
                return Err(paths::denied(
                    "file is outside this delegated run's active write lease",
                ));
            }
            if leases.iter().any(|(owner, paths)| {
                owner != &binding.owner && paths.iter().any(|path| overlaps(path, relative))
            }) {
                return Err(paths::denied("file is reserved by another delegated run"));
            }
        }
        Ok(target)
    }

    pub(crate) fn authorize_command(&self) -> io::Result<()> {
        if let Some(binding) = &self.collaboration {
            if binding.allowed.is_some() {
                return Err(paths::denied(
                    "delegated runs cannot execute shell commands",
                ));
            }
            if !binding
                .access
                .inner
                .leases
                .lock()
                .map_err(lock_error)?
                .is_empty()
            {
                return Err(paths::denied(
                    "wait for delegated file writes to finish before running a command",
                ));
            }
        }
        Ok(())
    }
}

fn validate_owner(owner: &str) -> io::Result<()> {
    if owner.is_empty() || owner.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a valid run ID is required",
        ));
    }
    Ok(())
}

fn overlaps(left: &Path, right: &Path) -> bool {
    // macOS filesystems also equate composed/decomposed Unicode filenames.
    // Conservatively reserve these aliases on case-sensitive volumes too.
    let key = |path: &Path| {
        PathBuf::from(
            path.to_string_lossy()
                .to_lowercase()
                .nfc()
                .collect::<String>(),
        )
    };
    let left = key(left);
    let right = key(right);
    left.starts_with(&right) || right.starts_with(&left)
}
