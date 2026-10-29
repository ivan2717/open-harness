use crate::{
    ApprovalMode, CommandAccess, CommandRecord, CommandStatus, Inner, LocalTools, NetworkAccess,
    PendingApproval, PermissionMode,
    command_environment::ReadOnlyScratch,
    command_policy::{Authorization, CommandPermissions},
    command_process::{CommandProcess, CommandProcessOptions},
    command_session::CommandSession,
    files::parse,
    journal, lock_error, paths,
};
use serde::Deserialize;
use std::{
    io,
    sync::{Arc, Weak, atomic::Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::oneshot,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandInput {
    command: String,
    #[serde(default = "default_yield")]
    yield_time_ms: u64,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    network_access: Option<NetworkAccess>,
    #[serde(default)]
    access: Option<CommandAccess>,
}
fn default_yield() -> u64 {
    10_000
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManageInput {
    command_id: String,
    #[serde(default = "default_action")]
    action: String,
    #[serde(default)]
    cursor: u64,
    #[serde(default)]
    wait_ms: u64,
}
fn default_action() -> String {
    "output".into()
}

pub(crate) fn sandbox_available() -> bool {
    crate::command_process::sandbox_available()
}
pub(crate) fn execution_mode() -> &'static str {
    if sandbox_available() {
        "sandboxed"
    } else {
        "unavailable"
    }
}
pub(crate) fn execution_available() -> bool {
    execution_mode() != "unavailable"
}

impl LocalTools {
    pub(crate) async fn command(&self, call: agent_runtime::ToolCall) -> io::Result<String> {
        let input: CommandInput = parse(&call.arguments)?;
        let access = input.access.unwrap_or_else(|| {
            if self.inner.mode == PermissionMode::ReadOnly
                || self
                    .collaboration
                    .as_ref()
                    .is_some_and(|binding| binding.allowed.is_some())
            {
                CommandAccess::ReadOnly
            } else {
                CommandAccess::WorkspaceWrite
            }
        });
        if self.inner.mode == PermissionMode::ReadOnly && access == CommandAccess::WorkspaceWrite {
            return Err(paths::denied(
                "workspace writes are disabled in read-only mode",
            ));
        }
        if input.command.trim().is_empty()
            || input.command.len() > 8192
            || input.command.contains('\0')
        {
            return Err(io::Error::other(
                "command must contain 1 to 8192 bytes without NUL",
            ));
        }
        if input.yield_time_ms > 30_000
            || input
                .timeout_seconds
                .is_some_and(|seconds| !(1..=86_400).contains(&seconds))
        {
            return Err(io::Error::other(
                "yield_time_ms must be between 0 and 30000; optional timeout_seconds must be between 1 and 86400",
            ));
        }
        let full_access = self.full_access();
        let network_access = input.network_access.unwrap_or(if full_access {
            NetworkAccess::Allowed
        } else {
            NetworkAccess::Denied
        });
        let unrestricted = full_access
            && access == CommandAccess::WorkspaceWrite
            && network_access == NetworkAccess::Allowed;
        if !unrestricted && !execution_available() {
            return Err(paths::denied(
                "command execution is not available on this platform",
            ));
        }
        let actual_execution_mode = if unrestricted {
            "unrestricted"
        } else {
            execution_mode()
        };
        self.verify_root()?;
        self.authorize_command(access)?;
        let id = format!(
            "command-{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            self.inner.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let session = Arc::new(CommandSession::new());
        self.inner.commands.insert(id.clone(), session.clone())?;
        let mut guard = CommandGuard {
            inner: Arc::downgrade(&self.inner),
            permissions: self.command_permissions.clone(),
            id: id.clone(),
            session: session.clone(),
            finished: false,
            owns_baseline: false,
        };
        {
            let mut journal = self.inner.journal.lock().map_err(lock_error)?;
            if journal.commands.len() >= 50
                && let Some(index) = journal.commands.iter().position(|record| {
                    !matches!(record.status.as_str(), "running" | "awaiting_approval")
                })
            {
                journal.commands.remove(index);
            }
            journal.commands.push(CommandRecord {
                id: id.clone(),
                execution_mode: actual_execution_mode.into(),
                task_title: None,
                command: input.command.clone(),
                status: "awaiting_approval".into(),
                output: String::new(),
                exit_code: None,
                network_access,
                access,
            });
            journal.save(&self.inner.state_dir)?;
        }
        let mut cancelled = session.cancellation();
        let authorization = self.command_permissions.request_for_task(
            &self.inner.state_dir,
            PendingApproval {
                task_title: None,
                id: id.clone(),
                review_mode: ApprovalMode::default(),
                epoch: 0,
                command: input.command.clone(),
                cwd: self.root().display().to_string(),
                execution_mode: actual_execution_mode.into(),
                network_access,
                access,
            },
            full_access,
        )?;
        let epoch = match authorization {
            Authorization::Ready(epoch) => Some(epoch),
            Authorization::Pending(receiver) => await_approval(receiver, &mut cancelled).await,
        };
        if epoch.is_none() || *cancelled.borrow() {
            guard.finish(
                if *cancelled.borrow() {
                    "cancelled"
                } else {
                    "denied"
                },
                None,
            )?;
            return Err(paths::denied(
                "command was denied, cancelled, or its permission was revoked",
            ));
        }
        // Approval is not a workspace lock. Recheck delegated authority after
        // the user decides; file leases may have changed while we were waiting.
        self.verify_root()?;
        self.authorize_command(access)?;
        let slot = self
            .command_permissions
            .admit_at_epoch(epoch.expect("authorization checked"))?;
        let shared_operation = self
            .collaboration
            .as_ref()
            .map(|binding| binding.access.command_operation(access))
            .transpose()?
            .flatten();
        let operation = if access == CommandAccess::WorkspaceWrite {
            Some(self.inner.operation.clone().try_lock_owned().map_err(|_| io::Error::new(
                io::ErrorKind::WouldBlock, "a workspace write operation is running; inspect or stop it before starting another write command"))?)
        } else {
            None
        };
        self.authorize_command(access)?;
        let scratch = if access == CommandAccess::ReadOnly {
            Some(ReadOnlyScratch::create(&id)?)
        } else {
            None
        };
        if access == CommandAccess::WorkspaceWrite {
            // Retain an incomplete previous baseline until reconciliation succeeds.
            {
                let mut journal = self.inner.journal.lock().map_err(lock_error)?;
                if let Some(previous) = journal.command_baseline.as_ref() {
                    let after = journal::snapshot(self.root()).map_err(|error| {
                        io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!("previous command changes still need inspection: {error}"),
                        )
                    })?;
                    let previous = previous.clone();
                    journal.reconcile_command(previous, after);
                    journal.command_baseline = None;
                    journal.save(&self.inner.state_dir)?;
                }
            }
            let baseline = journal::snapshot(self.root())?;
            {
                let mut journal = self.inner.journal.lock().map_err(lock_error)?;
                journal.command_baseline = Some(baseline);
                guard.owns_baseline = true;
                if let Some(record) = journal.commands.iter_mut().find(|record| record.id == id) {
                    record.status = "running".into();
                }
                journal.save(&self.inner.state_dir)?;
            }
        } else {
            let mut journal = self.inner.journal.lock().map_err(lock_error)?;
            if let Some(record) = journal.commands.iter_mut().find(|record| record.id == id) {
                record.status = "running".into();
            }
            journal.save(&self.inner.state_dir)?;
        }
        session.set_status("running", None)?;
        let mut process = match CommandProcess::spawn_with_options(
            self,
            &input.command,
            network_access,
            CommandProcessOptions {
                unrestricted,
                write_paths: (access == CommandAccess::ReadOnly).then(Vec::new),
                temporary_directory: scratch.as_ref().map(|scratch| scratch.path().to_owned()),
            },
        )
        .await
        {
            Ok(process) => process,
            Err(error) => {
                session.append(format!("Could not start command: {error}").as_bytes())?;
                guard.finish_after_cleanup("failed", None, || {
                    drop(scratch);
                    drop(slot);
                    drop(operation);
                    drop(shared_operation);
                })?;
                return Err(error);
            }
        };
        let stdout = process
            .child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing command stdout"))?;
        let stderr = process
            .child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("missing command stderr"))?;
        let worker_session = session.clone();
        tokio::spawn(async move {
            // Only write commands retain mutation locks and a journal baseline.
            // Read-only commands have their own OS-writable temporary directory.
            let (operation, shared_operation, slot, scratch) =
                (operation, shared_operation, slot, scratch);
            let stdout = tokio::spawn(drain(stdout, worker_session.clone()));
            let stderr = tokio::spawn(drain(stderr, worker_session.clone()));
            let timeout = async {
                if let Some(seconds) = input.timeout_seconds {
                    tokio::time::sleep(Duration::from_secs(seconds)).await;
                } else {
                    std::future::pending::<()>().await;
                }
            };
            let (status, code) = if *cancelled.borrow() {
                ("cancelled", None)
            } else {
                tokio::select! {
                    result = process.child.wait() => match result {
                        Ok(exit) => (if exit.success() { "completed" } else { "failed" }, exit.code()),
                        Err(error) => { let _ = worker_session.append(format!("\n{error}").as_bytes()); ("failed", None) }
                    },
                    _ = cancelled.changed() => ("cancelled", None),
                    _ = timeout => ("timed_out", None),
                }
            };
            process.terminate();
            let _ = process.child.wait().await;
            for pipe in [stdout, stderr] {
                let result = pipe
                    .await
                    .map_err(io::Error::other)
                    .and_then(|result| result);
                if let Err(error) = result {
                    let _ = worker_session
                        .append(format!("\nCould not collect command output: {error}").as_bytes());
                }
            }
            let _ = guard.finish_after_cleanup(status, code, || {
                drop(process);
                drop(scratch);
                drop(slot);
                drop(operation);
                drop(shared_operation);
            });
        });
        // Dropping the initial tool future (e.g. stopping the run while it is
        // awaiting output) cancels it. A successful yield transfers ownership to
        // the host registry, which is explicitly stopped or cancelled at teardown.
        let mut initial_wait = InitialWait {
            session,
            yielded: false,
        };
        wait_until_finished(&initial_wait.session, input.yield_time_ms).await;
        let result = initial_wait.session.snapshot(&id, 0)?;
        initial_wait.yielded = true;
        serde_json::to_string(&result).map_err(io::Error::other)
    }

    pub(crate) async fn manage_command(&self, call: agent_runtime::ToolCall) -> io::Result<String> {
        let input: ManageInput = parse(&call.arguments)?;
        self.authorize_command(CommandAccess::ReadOnly)?;
        let result = match input.action.as_str() {
            "output" | "status" => {
                self.command_status(&input.command_id, input.cursor, input.wait_ms)
                    .await?
            }
            "stop" => self.stop_command(&input.command_id).await?,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "action must be output, status or stop",
                ));
            }
        };
        serde_json::to_string(&result).map_err(io::Error::other)
    }

    /// Wait only for new output/completion, never for the workspace mutation lock.
    pub async fn command_status(
        &self,
        id: &str,
        cursor: u64,
        wait_ms: u64,
    ) -> io::Result<CommandStatus> {
        if wait_ms > 30_000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "wait_ms must be between 0 and 30000",
            ));
        }
        let session = self.inner.commands.get(id)?;
        let wait = async {
            loop {
                let changed = session.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let snapshot = session.snapshot(id, cursor)?;
                if !session.active() || snapshot.next_cursor > cursor || snapshot.output_truncated {
                    return Ok(snapshot);
                }
                changed.await;
            }
        };
        match tokio::time::timeout(Duration::from_millis(wait_ms), wait).await {
            Ok(result) => result,
            Err(_) => session.snapshot(id, cursor),
        }
    }

    pub async fn stop_command(&self, id: &str) -> io::Result<CommandStatus> {
        let session = self.inner.commands.get(id)?;
        self.command_permissions.remove_pending(id)?;
        session.cancel();
        wait_until_finished(&session, 5000).await;
        session.snapshot(id, 0)
    }

    /// Signal every live process and pending approval when the owning run stops.
    pub fn cancel_commands(&self) {
        let _ = self
            .command_permissions
            .cancel_pending(&self.inner.state_dir);
        self.inner.commands.cancel_all();
    }

    pub fn commands_running(&self) -> bool {
        self.inner.commands.is_active()
    }

    /// The host must prevent new command admission before invoking shutdown.
    pub async fn cancel_commands_and_wait(&self) -> io::Result<()> {
        self.command_permissions
            .cancel_pending(&self.inner.state_dir)?;
        let sessions = self.inner.commands.active_sessions();
        for session in &sessions {
            session.cancel();
        }
        let wait = async {
            for session in sessions {
                while session.active() {
                    wait_until_finished(&session, 1000).await;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "command processes have not finished stopping",
                )
            })
    }
}

async fn await_approval(
    receiver: oneshot::Receiver<Option<u64>>,
    cancelled: &mut tokio::sync::watch::Receiver<bool>,
) -> Option<u64> {
    if *cancelled.borrow() {
        return None;
    }
    tokio::select! {
        biased;
        _ = cancelled.changed() => None,
        result = receiver => result.unwrap_or(None),
    }
}

async fn wait_until_finished(session: &CommandSession, wait_ms: u64) {
    let wait = async {
        loop {
            let changed = session.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !session.active() {
                break;
            }
            changed.await;
        }
    };
    let _ = tokio::time::timeout(Duration::from_millis(wait_ms), wait).await;
}

async fn drain(mut reader: impl AsyncRead + Unpin, session: Arc<CommandSession>) -> io::Result<()> {
    let mut buffer = [0_u8; 4096];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        session.append(&buffer[..count])?;
    }
}

struct InitialWait {
    session: Arc<CommandSession>,
    yielded: bool,
}
impl Drop for InitialWait {
    fn drop(&mut self) {
        if !self.yielded {
            self.session.cancel();
        }
    }
}

struct CommandGuard {
    inner: Weak<Inner>,
    permissions: Arc<CommandPermissions>,
    id: String,
    session: Arc<CommandSession>,
    finished: bool,
    owns_baseline: bool,
}
impl CommandGuard {
    fn finish(&mut self, status: &str, code: Option<i32>) -> io::Result<()> {
        self.finish_after_cleanup(status, code, || {})
    }

    fn finish_after_cleanup(
        &mut self,
        status: &str,
        code: Option<i32>,
        cleanup: impl FnOnce(),
    ) -> io::Result<()> {
        let persisted = (|| -> io::Result<()> {
            self.permissions.remove_pending(&self.id)?;
            if let Some(inner) = self.inner.upgrade() {
                let mut journal = inner.journal.lock().map_err(lock_error)?;
                if self.owns_baseline
                    && let Some(before) = journal.command_baseline.take()
                {
                    let tools = LocalTools {
                        command_permissions: Default::default(),
                        inner: inner.clone(),
                        collaboration: None,
                    };
                    match tools
                        .verify_root()
                        .and_then(|_| journal::snapshot(&inner.root))
                    {
                        Ok(after) => journal.reconcile_command(before, after),
                        Err(error) => {
                            journal.command_baseline = Some(before);
                            self.session.append(
                                format!("\nCould not inspect command changes: {error}").as_bytes(),
                            )?;
                        }
                    }
                }
                if let Some(record) = journal
                    .commands
                    .iter_mut()
                    .find(|record| record.id == self.id)
                {
                    record.status = status.into();
                    record.exit_code = code;
                    record.output = self
                        .session
                        .state
                        .lock()
                        .map_err(lock_error)?
                        .archive
                        .text();
                }
                journal.save(&inner.state_dir)?;
            }
            Ok(())
        })();
        // Baseline reconciliation must finish under the mutation locks. Until
        // cleanup returns, CommandSessions::records overlays the journal with
        // the live session's running status, so neither review nor tool output
        // advertises completion while a lock/slot or scratch is still held.
        cleanup();
        // Failure to persist must not leave an already stopped process marked
        // running forever or retain a pending approval.
        self.finished = true;
        if let Err(error) = &persisted {
            let _ = self
                .session
                .append(format!("\nCould not save command review: {error}").as_bytes());
        }
        self.session.set_status(status, code)?;
        persisted
    }
}
impl Drop for CommandGuard {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish("cancelled", None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_status_is_published_only_after_cleanup_including_workspace_review() {
        let base = std::env::temp_dir().join(format!(
            "openharness-command-publication-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(base.join("project")).unwrap();
        let tools = LocalTools::open(
            base.join("project"),
            base.join("state"),
            PermissionMode::WorkspaceWrite,
        )
        .unwrap();
        let session = Arc::new(CommandSession::new());
        session.set_status("running", None).unwrap();
        tools
            .inner
            .commands
            .insert("publication".into(), session.clone())
            .unwrap();
        tools
            .inner
            .journal
            .lock()
            .unwrap()
            .commands
            .push(CommandRecord {
                id: "publication".into(),
                execution_mode: "sandboxed".into(),
                task_title: None,
                command: "test".into(),
                status: "running".into(),
                output: String::new(),
                exit_code: None,
                network_access: NetworkAccess::Denied,
                access: CommandAccess::ReadOnly,
            });
        let mut guard = CommandGuard {
            inner: Arc::downgrade(&tools.inner),
            permissions: tools.command_permissions.clone(),
            id: "publication".into(),
            session: session.clone(),
            finished: false,
            owns_baseline: false,
        };
        let mut cleaned = false;
        guard
            .finish_after_cleanup("completed", Some(0), || {
                assert!(session.active());
                // Durable reconciliation precedes cleanup, but the live session
                // stays authoritative while deleting a possibly large cache.
                assert_eq!(
                    tools.inner.journal.lock().unwrap().commands[0].status,
                    "completed"
                );
                assert_eq!(tools.review().unwrap().commands[0].status, "running");
                cleaned = true;
            })
            .unwrap();
        assert!(cleaned);
        assert!(!session.active());
        assert_eq!(tools.review().unwrap().commands[0].status, "completed");
        drop(guard);
        drop(tools);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn cancellation_before_approval_subscription_does_not_wait_for_a_new_change() {
        let session = CommandSession::new();
        session.cancel();
        let mut cancellation = session.cancellation();
        let (_sender, receiver) = oneshot::channel();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                await_approval(receiver, &mut cancellation)
            )
            .await
            .unwrap()
            .is_none()
        );
    }
}
