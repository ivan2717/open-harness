use super::{Request, acl, protected_paths, wide};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    ptr,
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0},
    Security::{
        Authorization::{DENY_ACCESS, GRANT_ACCESS, REVOKE_ACCESS},
        FreeSid,
        Isolation::{
            CreateAppContainerProfile, DeleteAppContainerProfile,
            DeriveAppContainerSidFromAppContainerName,
        },
        PSID,
    },
    Storage::FileSystem::{
        DELETE, FILE_ALL_ACCESS, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD,
        FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    },
    System::Threading::{
        CreateMutexW, GetCurrentProcess, GetProcessTimes, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION, ReleaseMutex, WaitForSingleObject,
    },
};

#[derive(Deserialize, Serialize)]
struct Journal {
    profile: String,
    pid: u32,
    created: u64,
    paths: Vec<PathBuf>,
    write_roots: Vec<PathBuf>,
    #[serde(default)]
    read_roots: Vec<PathBuf>,
}

pub(super) struct Lease {
    journal: Journal,
    journal_path: PathBuf,
    sid: PSID,
    cleaned: bool,
    deferred: bool,
}

impl Lease {
    pub fn prepare(request: &Request) -> io::Result<Self> {
        let _lock = AclLock::acquire()?;
        let state = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| {
                io::Error::other("LOCALAPPDATA is required for sandbox cleanup records")
            })?
            .join("OpenHarness/command-sandbox");
        fs::create_dir_all(&state)?;
        let state = checked(&state)?;
        let mut grants = Vec::new();
        for path in &request.read_paths {
            let root = checked(path)?;
            if system_executable(&root) {
                continue;
            }
            for path in walk(&root)? {
                grants.push((path, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE, GRANT_ACCESS));
            }
        }
        // Windows / Program Files executables use their existing system ACLs.
        // Ordinary users cannot and must not edit these executables' DACLs.
        let program = checked(&request.program)?;
        if !system_executable(&program) && !grants.iter().any(|(path, _, _)| path == &program) {
            grants.push((
                program,
                FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
                GRANT_ACCESS,
            ));
        }
        let workspace = checked(request.workspace.as_deref().unwrap_or(&request.cwd))?;
        // Workspace reads are separate from managed runtime reads: commands may
        // write only their explicit grants while still reading the project.
        let workspace_reads_start = grants.len();
        for item in walk(&workspace)? {
            let relative = item.strip_prefix(&workspace).unwrap_or(&item);
            let secret = relative.components().any(|part| {
                part.as_os_str().to_str().is_none_or(|name| {
                    !name.eq_ignore_ascii_case(".git") && protected_paths::denied_component(name)
                })
            });
            let metadata = fs::symlink_metadata(&item)?;
            let linked = metadata.is_file() && acl::links(&item)? > 1;
            if metadata.is_dir() {
                // A parent grant must not bypass a protected child's own ACL.
                // Normal files are deleted through their own DELETE grant.
                grants.push((item.clone(), FILE_DELETE_CHILD, DENY_ACCESS));
            }
            if secret || linked {
                grants.push((item, FILE_ALL_ACCESS, DENY_ACCESS));
            } else {
                grants.push((item, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE, GRANT_ACCESS));
            }
        }
        let workspace_reads = workspace_reads_start..grants.len();
        let mut write_roots = Vec::new();
        for path in &request.write_paths {
            let path = checked(path)?;
            if state.starts_with(&path) || path.starts_with(&state) || path.parent().is_none() {
                return Err(io::Error::other(
                    "sandbox write root overlaps trusted state or a volume root",
                ));
            }
            write_roots.push(path.clone());
            for item in walk(&path)? {
                if fs::symlink_metadata(&item)?.is_dir() {
                    grants.push((item.clone(), FILE_DELETE_CHILD, DENY_ACCESS));
                }
                let relative = item.strip_prefix(&path).unwrap_or(&item);
                let secret = relative.components().any(|part| {
                    part.as_os_str().to_str().is_none_or(|name| {
                        !name.eq_ignore_ascii_case(".git")
                            && protected_paths::denied_component(name)
                    })
                });
                let git = relative
                    .components()
                    .any(|part| part.as_os_str().eq_ignore_ascii_case(".git"));
                if secret {
                    grants.push((item, FILE_ALL_ACCESS, DENY_ACCESS));
                } else {
                    let metadata = fs::symlink_metadata(&item)?;
                    let linked_file = metadata.is_file() && acl::links(&item)? > 1;
                    if linked_file {
                        grants.push((item, FILE_ALL_ACCESS, DENY_ACCESS));
                        continue;
                    }
                    grants.push((
                        item.clone(),
                        FILE_GENERIC_READ
                            | FILE_GENERIC_EXECUTE
                            | if git { 0 } else { FILE_GENERIC_WRITE | DELETE },
                        GRANT_ACCESS,
                    ));
                    if git {
                        grants.push((item, acl::WRITE_RIGHTS, DENY_ACCESS));
                    }
                }
            }
        }
        for path in &request.denied_paths {
            if path.exists() {
                for path in walk(&checked(path)?)? {
                    grants.push((path, FILE_ALL_ACCESS, DENY_ACCESS));
                }
            }
        }
        // Remove workspace write rights for read-only commands. ACL setup fails
        // closed if ambient app-package grants would bypass this restriction.
        // Their private scratch is outside this tree and keeps its own grant.
        let read_only_workspace = !write_roots
            .iter()
            .any(|path| path.starts_with(&workspace) || workspace.starts_with(path));
        if read_only_workspace {
            for path in walk(&workspace)? {
                grants.push((path, acl::WRITE_RIGHTS, DENY_ACCESS));
            }
        }
        // A managed runtime remains read-only even if a user chose a workspace
        // containing it. Restrictions remove this SID's workspace write grants.
        for root in &request.read_paths {
            let root = checked(root)?;
            if write_roots
                .iter()
                .any(|workspace| root.starts_with(workspace))
            {
                for path in walk(&root)? {
                    grants.push((path, acl::WRITE_RIGHTS, DENY_ACCESS));
                }
            }
        }
        // This directory must remain inaccessible even with broad read roots.
        grants.push((state.clone(), FILE_ALL_ACCESS, DENY_ACCESS));
        if !checked(&request.cwd)?.starts_with(&workspace) {
            return Err(io::Error::other(
                "working directory must be inside the workspace",
            ));
        }
        recover(&state)?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let profile = format!("OpenHarness.Command.{}.{nonce:x}", std::process::id());
        let name = wide(&profile)?;
        let mut sid = ptr::null_mut();
        let status = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                ptr::null(),
                0,
                &mut sid,
            )
        };
        if status < 0 {
            return Err(io::Error::other(format!(
                "cannot create AppContainer (HRESULT {status:#x})"
            )));
        }
        let journal = Journal {
            profile,
            pid: std::process::id(),
            created: creation_time(unsafe { GetCurrentProcess() })?,
            paths: grants.iter().map(|(path, _, _)| path.clone()).collect(),
            write_roots,
            read_roots: vec![workspace],
        };
        let journal_path = state.join(format!("{}.json", journal.profile));
        let mut lease = Self {
            journal,
            journal_path,
            sid,
            cleaned: false,
            deferred: false,
        };
        // Persist the cleanup plan before granting anything, including if the
        // API host terminates this helper while setup is in progress.
        fs::write(
            &lease.journal_path,
            serde_json::to_vec(&lease.journal).map_err(io::Error::other)?,
        )?;
        for (index, (path, rights, mode)) in grants.into_iter().enumerate() {
            let result = if read_only_workspace
                && workspace_reads.contains(&index)
                && mode == GRANT_ACCESS
            {
                // Existing files remain live, but a new or atomically replaced
                // file is readable only after the next command scans it.
                acl::change_without_inheritance(&path, sid, rights, mode)
            } else {
                acl::change(&path, sid, rights, mode)
            };
            if let Err(error) = result {
                let _ = lease.cleanup();
                return Err(io::Error::other(format!(
                    "cannot restrict {}: {error}",
                    path.display()
                )));
            }
        }
        Ok(lease)
    }

    pub fn sid(&self) -> PSID {
        self.sid
    }

    pub fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        let _lock = AclLock::acquire()?;
        clean(&self.journal, self.sid)?;
        fs::remove_file(&self.journal_path)?;
        self.cleaned = true;
        Ok(())
    }

    pub fn defer_cleanup(&mut self) {
        self.deferred = true;
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if !self.deferred {
            let _ = self.cleanup();
        }
        unsafe {
            FreeSid(self.sid);
        }
    }
}

fn checked(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::other("sandbox paths must be absolute"));
    }
    // Never apply ACL grants through a junction/symlink or other reparse point.
    for ancestor in path.ancestors() {
        if fs::symlink_metadata(ancestor)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other(format!(
                "sandbox ACL path is a reparse point: {}",
                ancestor.display()
            )));
        }
    }
    fs::canonicalize(path)
}

fn clean(journal: &Journal, sid: PSID) -> io::Result<()> {
    let mut first_error = None;
    let paths: std::collections::BTreeSet<PathBuf> = journal.paths.iter().cloned().collect();
    // Concurrent host edits can create children in a read-only project too.
    // Remove each parent's ACEs before enumerating children, so new descendants
    // no longer inherit this command's SID while cleanup is in progress.
    for root in journal.write_roots.iter().chain(&journal.read_roots) {
        if root.exists()
            && let Err(error) = clean_tree(root, sid)
        {
            first_error.get_or_insert(error);
        }
    }
    for path in paths.iter().rev() {
        if path.exists()
            && let Err(error) = acl::change(path, sid, 0, REVOKE_ACCESS)
        {
            first_error.get_or_insert(error);
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    let name = wide(&journal.profile)?;
    let result = unsafe { DeleteAppContainerProfile(name.as_ptr()) };
    if result < 0 && result as u32 != 0x80070002 {
        return Err(io::Error::other(format!(
            "cannot remove AppContainer profile ({result:#x})"
        )));
    }
    Ok(())
}

fn clean_tree(root: &Path, sid: PSID) -> io::Result<()> {
    let mut pending = vec![root.to_owned()];
    let mut visited = 0usize;
    while let Some(path) = pending.pop() {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            continue;
        }
        acl::change(&path, sid, 0, REVOKE_ACCESS)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        }
        visited += 1;
        if visited > 500_000 {
            return Err(io::Error::other(
                "workspace is too large for Windows ACL cleanup",
            ));
        }
    }
    Ok(())
}

fn recover(state: &Path) -> io::Result<()> {
    for entry in fs::read_dir(state)? {
        let path = entry?.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(journal) = serde_json::from_slice::<Journal>(&bytes) else {
            continue;
        };
        if !journal.profile.starts_with("OpenHarness.Command.") {
            continue;
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, journal.pid) };
        if !process.is_null() {
            let alive = creation_time(process).is_ok_and(|created| created == journal.created);
            unsafe {
                CloseHandle(process);
            }
            if alive {
                continue;
            }
        }
        let name = wide(&journal.profile)?;
        let mut sid = ptr::null_mut();
        if unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) } < 0 {
            continue;
        }
        let result = clean(&journal, sid);
        unsafe {
            FreeSid(sid);
        }
        if result.is_ok() {
            let _ = fs::remove_file(path);
        }
    }
    Ok(())
}

fn creation_time(process: windows_sys::Win32::Foundation::HANDLE) -> io::Result<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

// Serializes our read/merge/write ACL operations across commands. This does
// not serialize command execution; different package SIDs run concurrently.
struct AclLock(HANDLE);
impl AclLock {
    fn acquire() -> io::Result<Self> {
        let name = wide("Local\\OpenHarness.CommandSandbox.Acl")?;
        let handle = unsafe { CreateMutexW(ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        match unsafe { WaitForSingleObject(handle, 30_000) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self(handle)),
            _ => {
                unsafe {
                    CloseHandle(handle);
                }
                Err(io::Error::other("timed out acquiring sandbox ACL lock"))
            }
        }
    }
}
impl Drop for AclLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

fn system_executable(program: &Path) -> bool {
    ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .any(|key| {
            std::env::var_os(key)
                .and_then(|path| fs::canonicalize(path).ok())
                .is_some_and(|root| program.starts_with(root))
        })
}

fn walk(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut pending = vec![root.to_owned()];
    let mut result = Vec::new();
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            // No grants to junctions, no traversal into their destinations.
            continue;
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        }
        result.push(path);
        if result.len() > 500_000 {
            return Err(io::Error::other(
                "workspace is too large for a safe Windows ACL grant",
            ));
        }
    }
    Ok(result)
}
