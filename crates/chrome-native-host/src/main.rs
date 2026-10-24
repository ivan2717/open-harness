#![cfg_attr(windows, windows_subsystem = "windows")]

mod launch;
mod native_path;
#[cfg(windows)]
mod windows_job;

use std::process::ExitCode;

fn main() -> ExitCode {
    match launch::run() {
        Ok(code) => ExitCode::from(code),
        Err(_) => {
            if std::env::args_os()
                .nth(1)
                .is_some_and(|arg| arg == "--native-path")
            {
                eprintln!(
                    "Chrome cannot launch this path safely; use an existing executable at an absolute path without % or !."
                );
                return ExitCode::FAILURE;
            }
            // stdout is exclusively the binary Native Messaging / MCP channel.
            // Do not expose paths, launch configuration or inherited arguments.
            eprintln!(
                "Open Harness Chrome launcher could not start. Repair the connection in Open Harness and retry."
            );
            ExitCode::FAILURE
        }
    }
}
