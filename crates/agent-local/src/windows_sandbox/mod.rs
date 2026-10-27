//! AppContainer is the filesystem/network boundary; Jobs only contain lifetime.
//! Every command has a distinct package SID so concurrent tasks cannot access
//! each other's grants. Only that SID's ACEs are removed during cleanup.
mod acl;
mod lease;
mod process;
#[path = "../protected_paths.rs"]
mod protected_paths;

use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    io::{self, BufRead, Read},
    os::windows::ffi::OsStrExt,
    path::PathBuf,
};

/// Versioned stdin protocol from the trusted host. Paths must exist and be
/// absolute; missing deny paths are harmless (they contain no existing data).
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub read_paths: Vec<PathBuf>,
    pub write_paths: Vec<PathBuf>,
    pub denied_paths: Vec<PathBuf>,
    pub network_allowed: bool,
    #[serde(default)]
    pub workspace: Option<PathBuf>,
}

pub fn run() -> io::Result<u32> {
    // Bound the trusted protocol, and never pass it through to command stdin.
    let mut line = String::new();
    io::stdin()
        .lock()
        .take(4 * 1024 * 1024 + 1)
        .read_line(&mut line)?;
    if line.len() > 4 * 1024 * 1024 || !line.ends_with('\n') {
        return Err(io::Error::other("invalid sandbox launch request"));
    }
    let request: Request = serde_json::from_str(&line).map_err(io::Error::other)?;
    if request.version != 1
        || !request.program.is_absolute()
        || !request.cwd.is_absolute()
        || !request.program.is_file()
        || !request.cwd.is_dir()
        || request.write_paths.is_empty()
    {
        return Err(io::Error::other(
            "invalid sandbox version, executable or workspace",
        ));
    }
    let mut lease = lease::Lease::prepare(&request)?;
    let result = process::execute(&request, lease.sid());
    if result
        .as_ref()
        .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
    {
        lease.defer_cleanup();
        return result;
    }
    // Cleanup failures retain a recovery journal; they never trigger an
    // unrestricted retry or overwrite another process's DACL edits.
    if let Err(error) = lease.cleanup() {
        eprintln!("Windows sandbox cleanup will be retried: {error}");
    }
    result
}

pub(super) fn wide(value: impl AsRef<OsStr>) -> io::Result<Vec<u16>> {
    let mut result: Vec<u16> = value.as_ref().encode_wide().collect();
    if result.contains(&0) {
        return Err(io::Error::other("NUL in Windows sandbox argument"));
    }
    result.push(0);
    Ok(result)
}
