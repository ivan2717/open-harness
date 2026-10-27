//! Shared workspace coordination for delegated runs. A lease reserves exact
//! files, never a directory subtree; it does not grant additional permissions.
use crate::{
    CommandAccess, LocalTools, PermissionMode, command_policy::CommandPermissions, lock_error,
    paths,
};
use agent_runtime::ToolDefinition;
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::{OwnedRwLockWriteGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use unicode_normalization::UnicodeNormalization;

#[derive(Clone)]
pub struct WorkspaceAccess {
    inner: Arc<Shared>,
}

struct Shared {
    root: PathBuf,
    operations: Arc<RwLock<()>>,
    leases: Mutex<BTreeMap<String, Vec<PathBuf>>>,
    command_permissions: Mutex<BTreeMap<String, Arc<CommandPermissions>>>,
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
                operations: Arc::new(RwLock::new(())),
                leases: Mutex::new(BTreeMap::new()),
                command_permissions: Mutex::new(BTreeMap::new()),
            }),
        })
    }

    /// Atomically reserve files after any workspace-writing command or file
    /// mutation ends. Read-only commands do not hold this lock.
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
                    format!(
                        "run {owner} already reserves exact files {}; stop the run and wait for cleanup before replacing its write_paths",
                        describe_paths(existing),
                    ),
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
                    format!(
                        "requested files {} overlap exact files {} reserved by run {other}; coordinate with that owner, or stop it and wait for cleanup before retrying",
                        describe_paths(&paths),
                        describe_paths(reserved),
                    ),
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
                return Err(paths::denied(&format!(
                    "write_paths must name individual files, not directories: {path:?}; for example use .openharness-tmp/task.js, not .openharness-tmp. Omit write_paths to inherit workspace access",
                )));
            }
            let relative = target.strip_prefix(&self.inner.root).unwrap().to_path_buf();
            if normalized.contains(&relative) {
                continue;
            }
            if normalized
                .iter()
                .any(|existing| overlaps(existing, &relative))
            {
                return Err(paths::denied(&format!(
                    "overlapping individual file paths in write_paths at {path:?}; directory subtrees are not supported",
                )));
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

    /// A read-only command is enforced by the OS sandbox and can mutate only
    /// its own scratch directory. It needs no workspace mutation lock. Reads
    /// observe the live workspace; this does not promise a frozen test snapshot.
    pub(crate) fn command_operation(
        &self,
        access: CommandAccess,
    ) -> io::Result<Option<OwnedRwLockWriteGuard<()>>> {
        if access == CommandAccess::ReadOnly {
            return Ok(None);
        }
        self.inner
            .operations
            .clone()
            .try_write_owned()
            .map(Some)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "workspace is busy; wait for the current operation before running a command",
                )
            })
    }

    fn command_permissions(&self, root_run_id: &str) -> io::Result<Arc<CommandPermissions>> {
        validate_owner(root_run_id)?;
        Ok(self
            .inner
            .command_permissions
            .lock()
            .map_err(lock_error)?
            .entry(root_run_id.to_owned())
            .or_default()
            .clone())
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
    pub(crate) allowed: Option<Vec<PathBuf>>,
    pub definitions: Vec<ToolDefinition>,
}

impl LocalTools {
    /// `None` retains the configured workspace permission for either a parent
    /// or an inheriting delegate. `Some` narrows writes to exact leased files;
    /// an empty list makes the binding read-only. It never grants permissions.
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
            definitions.retain(|tool| tool.name != "apply_patch" || !paths.is_empty());
        }
        self.command_permissions = access.command_permissions(&owner)?;
        self.collaboration = Some(Arc::new(Collaboration {
            access,
            owner,
            allowed,
            definitions,
        }));
        Ok(self)
    }

    /// Bind descendants to their root conversation's user-approved command
    /// policy. The host supplies this ID from delegation metadata, never a
    /// model-controlled tool argument. Separate root conversations do not share
    /// grants even when they use the same project directory.
    pub fn with_command_approval_scope(mut self, root_run_id: &str) -> io::Result<Self> {
        let binding = self.collaboration.as_ref().ok_or_else(|| {
            io::Error::other("command approval scope requires a collaboration binding")
        })?;
        self.command_permissions = binding.access.command_permissions(root_run_id)?;
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
                return Err(paths::denied(&format!(
                    "file {path:?} is outside run {}'s active write lease; allowed exact files: {}. write_paths does not authorize directory children. Ask the coordinator to stop this task, then resume it with corrected write_paths (null to inherit the parent's workspace permission)",
                    binding.owner,
                    describe_paths(allowed),
                )));
            }
            if let Some((owner, reserved)) = leases.iter().find(|(owner, paths)| {
                *owner != &binding.owner && paths.iter().any(|path| overlaps(path, relative))
            }) {
                return Err(paths::denied(&format!(
                    "file {path:?} is reserved by another delegated run {owner}; its exact files are {}. Send that owner the task, or stop it and wait for cleanup before taking over",
                    describe_paths(reserved),
                )));
            }
        }
        Ok(target)
    }

    pub(crate) fn authorize_command(&self, access: CommandAccess) -> io::Result<()> {
        if access == CommandAccess::ReadOnly {
            return Ok(());
        }
        if self.inner.mode != PermissionMode::WorkspaceWrite {
            return Err(paths::denied(
                "this workspace permits only read-only sandboxed commands",
            ));
        }
        if let Some(binding) = &self.collaboration {
            if binding.allowed.is_some() {
                return Err(paths::denied(&format!(
                    "file-scoped run {} can only execute read-only sandboxed commands; write through file tools within {}. If the task needs a workspace-writing command, ask the coordinator to stop it and resume with write_paths:null to inherit its permission",
                    binding.owner,
                    describe_paths(binding.allowed.as_ref().unwrap()),
                )));
            }
            let leases = binding.access.inner.leases.lock().map_err(lock_error)?;
            if let Some((owner, reserved)) = leases.iter().next() {
                return Err(paths::denied(&format!(
                    "wait for delegated file writes to finish before running a workspace-writing command; run {owner} reserves {}. Collect its result or stop it and wait for cleanup",
                    describe_paths(reserved),
                )));
            }
        }
        Ok(())
    }
}

fn describe_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "[] (read-only)".into();
    }
    let listed = paths
        .iter()
        .take(8)
        .map(|path| format!("{:?}", path.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > 8 {
        format!("[{listed}, … {} more]", paths.len() - 8)
    } else {
        format!("[{listed}]")
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

#[cfg(test)]
mod command_policy_tests {
    use super::*;
    use crate::NetworkAccess;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let root = std::env::temp_dir().join(format!(
                "openharness-command-scopes-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("project")).unwrap();
            Self(root)
        }
        fn tools(
            &self,
            access: &WorkspaceAccess,
            owner: &str,
            allowed: Option<Vec<String>>,
        ) -> LocalTools {
            LocalTools::open(
                self.0.join("project"),
                self.0.join(owner),
                PermissionMode::WorkspaceWrite,
            )
            .unwrap()
            .with_collaboration(access.clone(), owner, allowed)
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn descendants_share_only_their_root_conversations_authorization_ceiling() {
        let fixture = Fixture::new();
        let access = WorkspaceAccess::new(fixture.0.join("project")).unwrap();
        let parent = fixture.tools(&access, "parent", None);
        let child = fixture
            .tools(&access, "child", Some(Vec::new()))
            .with_command_approval_scope("parent")
            .unwrap();
        let other = fixture.tools(&access, "unrelated", None);
        parent
            .command_permissions
            .grant(CommandAccess::WorkspaceWrite, NetworkAccess::Allowed)
            .unwrap();
        assert!(
            child
                .command_permissions
                .allows(CommandAccess::ReadOnly, NetworkAccess::Allowed)
                .unwrap()
        );
        assert!(
            !other
                .command_permissions
                .allows(CommandAccess::ReadOnly, NetworkAccess::Denied)
                .unwrap()
        );
        // Inheriting a broad parent grant does not broaden delegated file access.
        assert!(child.authorize_command(CommandAccess::ReadOnly).is_ok());
        assert!(
            child
                .authorize_command(CommandAccess::WorkspaceWrite)
                .is_err()
        );
        parent.command_permissions.revoke().unwrap();
        assert!(
            !child
                .command_permissions
                .allows(CommandAccess::ReadOnly, NetworkAccess::Denied)
                .unwrap()
        );
        // A new host has no saved grant even for the same root/run identifiers.
        let reopened = WorkspaceAccess::new(fixture.0.join("project")).unwrap();
        assert!(
            reopened
                .command_permissions("parent")
                .unwrap()
                .grants()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn only_workspace_mutating_commands_take_the_global_mutation_lock() {
        let fixture = Fixture::new();
        let access = WorkspaceAccess::new(fixture.0.join("project")).unwrap();
        let writer = access
            .command_operation(CommandAccess::WorkspaceWrite)
            .unwrap()
            .unwrap();
        assert!(
            access
                .command_operation(CommandAccess::ReadOnly)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            access
                .command_operation(CommandAccess::WorkspaceWrite)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(access.try_acquire("child", &["a.txt".into()]).is_err());
        drop(writer);
        access.try_acquire("child", &["a.txt".into()]).unwrap();
        let child = fixture.tools(&access, "child", Some(vec!["a.txt".into()]));
        let parent = fixture.tools(&access, "parent", None);
        assert!(child.authorize_command(CommandAccess::ReadOnly).is_ok());
        assert!(parent.authorize_command(CommandAccess::ReadOnly).is_ok());
        assert!(
            parent
                .authorize_command(CommandAccess::WorkspaceWrite)
                .is_err()
        );
        access.release("child").await;
        assert!(
            parent
                .authorize_command(CommandAccess::WorkspaceWrite)
                .is_ok()
        );
    }
}
