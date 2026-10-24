#![cfg_attr(windows, windows_subsystem = "windows")]

mod launch;
#[cfg(windows)]
mod windows_job;

use std::process::ExitCode;

fn main() -> ExitCode {
    match launch::run() {
        Ok(code) => ExitCode::from(code),
        Err(_) => {
            // stdout is exclusively the binary Native Messaging / MCP channel.
            // Do not expose paths, launch configuration or inherited arguments.
            eprintln!(
                "Open Harness Chrome launcher could not start. Repair the connection in Open Harness and retry."
            );
            ExitCode::FAILURE
        }
    }
}
