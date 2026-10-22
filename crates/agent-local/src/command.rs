use crate::{
    Approval, CommandRecord, LocalTools, PendingApproval, PermissionMode, files::parse, journal,
    lock_error, output::CommandOutput, paths,
};
use serde::Deserialize;
use std::{
    fs, io,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::oneshot,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandInput {
    command: String,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
}
fn default_timeout() -> u64 {
    60
}

pub(crate) fn sandbox_available() -> bool {
    cfg!(target_os = "macos") && Path::new("/usr/bin/sandbox-exec").is_file()
}

impl LocalTools {
    pub(crate) async fn command(&self, call: agent_runtime::ToolCall) -> io::Result<String> {
        let input: CommandInput = parse(&call.arguments)?;
        if self.inner.mode != PermissionMode::WorkspaceWrite {
            return Err(paths::denied("commands are disabled in read-only mode"));
        }
        if input.command.trim().is_empty()
            || input.command.len() > 8192
            || input.command.contains('\0')
        {
            return Err(io::Error::other(
                "command must contain 1 to 8192 bytes without NUL",
            ));
        }
        if !(1..=120).contains(&input.timeout_seconds) {
            return Err(io::Error::other(
                "timeout_seconds must be between 1 and 120",
            ));
        }
        if !sandbox_available() {
            return Err(paths::denied(
                "no supported OS command sandbox is available; commands are disabled",
            ));
        }
        let id = format!(
            "command-{}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            self.inner.next_id.fetch_add(1, Ordering::Relaxed)
        );
        let mut guard = CommandGuard {
            tools: self.clone(),
            id: id.clone(),
            process_group: None,
            finished: false,
            output: Arc::new(Mutex::new(CommandOutput::default())),
        };
        {
            let mut journal = self.inner.journal.lock().map_err(lock_error)?;
            if journal.commands.len() >= 50 {
                journal.commands.remove(0);
            }
            journal.commands.push(CommandRecord {
                id: id.clone(),
                command: input.command.clone(),
                status: "awaiting_approval".into(),
                output: String::new(),
                exit_code: None,
            });
            journal.save(&self.inner.state_dir)?;
        }
        let (sender, receiver) = oneshot::channel();
        self.inner.approvals.lock().map_err(lock_error)?.insert(
            id.clone(),
            Approval {
                info: PendingApproval {
                    id: id.clone(),
                    command: input.command.clone(),
                    cwd: self.root().display().to_string(),
                },
                sender,
            },
        );
        if !receiver.await.unwrap_or(false) {
            guard.finish("denied", None)?;
            return Err(paths::denied("user denied the command"));
        }

        self.verify_root()?;
        // A failed previous scan must be reconciled before replacing its
        // baseline, otherwise earlier command effects would disappear.
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
            if let Some(record) = journal.commands.iter_mut().find(|record| record.id == id) {
                record.status = "running".into();
            }
            journal.save(&self.inner.state_dir)?;
        }
        let scratch = self.root().join(".openharness-tmp");
        if fs::symlink_metadata(&scratch).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(paths::denied(
                "command scratch directory cannot be a symbolic link",
            ));
        }
        fs::create_dir_all(&scratch)?;
        self.verify_root()?;
        // The trusted wrapper owns the process group. Its watchdog retains a
        // group member until this host closes stdin, including on SIGKILL.
        // User code only enters the sandbox child and cannot inherit that pipe.
        const WRAPPER: &str = "owner=$$\n(while IFS= read -r line; do :; done; kill -KILL -- \"-$owner\") <&0 >/dev/null 2>&1 &\n/usr/bin/sandbox-exec -p \"$1\" /bin/sh -c \"$2\" </dev/null\nexit \"$?\"";
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            WRAPPER,
            "openharness-command",
            &sandbox_profile(self.root(), &self.inner.state_dir),
            &input.command,
        ]);
        command
            .current_dir(self.root())
            .env_clear()
            .env(
                "PATH",
                std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            )
            .env("HOME", self.root())
            .env("TMPDIR", &scratch)
            .env("LANG", "en_US.UTF-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        guard.process_group = child.id();
        // Child::wait closes its own stdin to avoid deadlocks. Keep this one
        // separately: EOF is a cancellation signal to the watchdog.
        let _supervision = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing command stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("missing command stderr"))?;
        let output = guard.output.clone();
        let result = tokio::time::timeout(Duration::from_secs(input.timeout_seconds), async {
            let (status, stdout, stderr) = tokio::join!(
                child.wait(),
                drain(stdout, output.clone()),
                drain(stderr, output)
            );
            stdout?;
            stderr?;
            status
        })
        .await;
        kill_group(guard.process_group.take());
        let (status, code) =
            match result {
                Ok(Ok(status)) => {
                    if !status.success() && status.code().is_none() {
                        guard.output.lock().map_err(lock_error)?.extend_from_slice(
                            format!("\nCommand terminated: {status}").as_bytes(),
                        );
                    }
                    (
                        if status.success() {
                            "completed"
                        } else {
                            "failed"
                        },
                        status.code(),
                    )
                }
                Ok(Err(error)) => {
                    guard
                        .output
                        .lock()
                        .map_err(lock_error)?
                        .extend_from_slice(format!("\n{error}").as_bytes());
                    ("failed", None)
                }
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    ("timed_out", None)
                }
            };
        guard.finish(status, code)?;
        let output = guard.output.lock().map_err(lock_error)?.text();
        Ok(serde_json::json!({"command_id":id,"status":status,"exit_code":code,"output":paths::bounded(&output,paths::OUTPUT_LIMIT),"note":"Network access is disabled. Command changes require review; automatic undo is unavailable for command changes."}).to_string())
    }
}

async fn drain(
    mut reader: impl AsyncRead + Unpin,
    output: Arc<Mutex<CommandOutput>>,
) -> io::Result<()> {
    let mut buffer = [0_u8; 4096];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        let mut output = output.lock().map_err(lock_error)?;
        output.extend_from_slice(&buffer[..count]);
    }
}

struct CommandGuard {
    tools: LocalTools,
    id: String,
    process_group: Option<u32>,
    finished: bool,
    output: Arc<Mutex<CommandOutput>>,
}
impl CommandGuard {
    fn finish(&mut self, status: &str, exit_code: Option<i32>) -> io::Result<()> {
        let mut journal = self.tools.inner.journal.lock().map_err(lock_error)?;
        if let Some(before) = journal.command_baseline.take() {
            match self
                .tools
                .verify_root()
                .and_then(|_| journal::snapshot(self.tools.root()))
            {
                Ok(after) => journal.reconcile_command(before, after),
                Err(error) => {
                    journal.command_baseline = Some(before);
                    self.output.lock().map_err(lock_error)?.extend_from_slice(
                        format!("\nCould not inspect command changes: {error}").as_bytes(),
                    );
                }
            }
        }
        if let Some(record) = journal
            .commands
            .iter_mut()
            .find(|record| record.id == self.id)
        {
            record.status = status.into();
            record.output = self.output.lock().map_err(lock_error)?.text();
            record.exit_code = exit_code;
        }
        journal.save(&self.tools.inner.state_dir)?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for CommandGuard {
    fn drop(&mut self) {
        kill_group(self.process_group.take());
        if let Ok(mut approvals) = self.tools.inner.approvals.lock() {
            approvals.remove(&self.id);
        }
        if !self.finished {
            let _ = self.finish("cancelled", None);
        }
    }
}

fn kill_group(group: Option<u32>) {
    #[cfg(unix)]
    if let Some(group) = group.and_then(|group| i32::try_from(group).ok()) {
        // The child starts a new process group. Killing that owned group also
        // terminates shell descendants when the tool future is dropped.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = group;
}

fn quoted(path: &Path) -> String {
    serde_json::to_string(&path.to_string_lossy()).expect("path serialization")
}

fn sandbox_profile(root: &Path, state: &Path) -> String {
    let mut profile = String::from(
        "(version 1)\n(deny default)\n(allow process*)\n(allow sysctl-read)\n(allow mach-lookup)\n(allow file-read-metadata)\n(allow file-read* (literal \"/\"))\n",
    );
    for directory in [
        "/usr",
        "/bin",
        "/sbin",
        "/System",
        "/Library",
        "/dev",
        "/private/etc",
        "/private/var/db",
        "/opt/homebrew",
    ] {
        profile.push_str(&format!(
            "(allow file-read* (subpath {}))\n",
            quoted(Path::new(directory))
        ));
    }
    if let Some(home) = std::env::var_os("HOME") {
        for directory in [".cargo", ".rustup", ".nvm", ".local/share/mise", ".bun/bin"] {
            profile.push_str(&format!(
                "(allow file-read* (subpath {}))\n",
                quoted(&Path::new(&home).join(directory))
            ));
        }
        for secret in [".cargo/credentials", ".cargo/credentials.toml"] {
            profile.push_str(&format!(
                "(deny file-read* (literal {}))\n",
                quoted(&Path::new(&home).join(secret))
            ));
        }
    }
    profile.push_str(&format!(
        "(allow file-read* (subpath {}))\n(allow file-write* (subpath {}))\n",
        quoted(root),
        quoted(root)
    ));
    // Regex restrictions apply throughout nested projects as well as at root.
    profile.push_str(&format!(
        "(deny file-write* (regex #\"(^|/){}(/|$)\"))\n",
        paths::regex_literal(".git")
    ));
    for pattern in paths::secret_patterns() {
        profile.push_str(&format!(
            "(deny file-read* file-write* (regex #\"{pattern}\"))\n"
        ));
    }
    profile.push_str("(allow file-write* (literal \"/dev/null\"))\n");
    profile.push_str(&format!(
        "(deny file-read* file-write* (subpath {}))\n",
        quoted(state)
    ));
    profile
}
