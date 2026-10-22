use std::{io, process::Stdio};

use rmcp::{
    RoleClient,
    service::{RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{Transport, async_rw::AsyncRwTransport},
};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::{MAX_MESSAGE_BYTES, McpError, Result};

pub(crate) struct StdioTransport {
    io: AsyncRwTransport<RoleClient, ChildStdout, ChildStdin>,
    child: Child,
    // Keep the group id after the direct child exits, so descendants cannot outlive the session.
    process_id: Option<u32>,
}

impl StdioTransport {
    pub fn spawn(command: &str, args: &[String], env_vars: &[String]) -> Result<Self> {
        let mut process = Command::new(command);
        process
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for name in [
            "PATH",
            "HOME",
            "USERPROFILE",
            "SYSTEMROOT",
            "WINDIR",
            "TMPDIR",
            "TEMP",
            "TMP",
            "LANG",
            "LC_ALL",
        ] {
            if let Some(value) = std::env::var_os(name) {
                process.env(name, value);
            }
        }
        for name in env_vars {
            let value = std::env::var_os(name).ok_or(McpError::Environment)?;
            process.env(name, value);
        }
        #[cfg(unix)]
        process.process_group(0);
        let mut child = process.spawn().map_err(|_| McpError::Launch)?;
        let process_id = child.id();
        let stdout = child.stdout.take().ok_or(McpError::Launch)?;
        let stdin = child.stdin.take().ok_or(McpError::Launch)?;
        Ok(Self {
            io: AsyncRwTransport::new(stdout, stdin).with_max_line_length(MAX_MESSAGE_BYTES),
            child,
            process_id,
        })
    }

    fn kill_group(&self) {
        #[cfg(unix)]
        if let Some(id) = self.process_id {
            // This child was started as its own process group above.
            unsafe {
                libc::kill(-(id as libc::pid_t), libc::SIGKILL);
            }
        }
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.kill_group();
        let _ = self.child.start_kill();
        // Tokio's child reaper handles the non-async drop path.
    }
}

impl Transport<RoleClient> for StdioTransport {
    type Error = io::Error;
    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = io::Result<()>> + Send + 'static {
        self.io.send(item)
    }
    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleClient>>> + Send {
        self.io.receive()
    }
    async fn close(&mut self) -> io::Result<()> {
        let _ = self.io.close().await;
        self.kill_group();
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        self.process_id = None;
        Ok(())
    }
}
