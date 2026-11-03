use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    path::{Component, Path, PathBuf, Prefix},
    ptr,
};
use tokio::process::Child;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
    },
};

/// Supervises the trusted launcher and every sandboxed descendant. AppContainer
/// in the launcher supplies filesystem/network isolation; this Job supplies
/// cancellation and host-death cleanup.
pub(super) struct Job(HANDLE);
// The sole owned kernel handle is safe to transfer between executor threads.
unsafe impl Send for Job {}
impl Job {
    pub fn assign_and_resume(child: &Child) -> io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(handle);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("command process already exited"))?;
        if unsafe { AssignProcessToJobObject(job.0, process) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("missing command process ID"))?;
        resume_primary_thread(pid)?;
        Ok(job)
    }
    pub fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.0, 1);
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn resume_primary_thread(pid: u32) -> io::Result<()> {
    // CREATE_SUSPENDED guarantees that the new process has only its primary
    // thread and cannot run before the kill-on-close Job is assigned.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut present = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    let mut result = Err(io::Error::other("could not find suspended command thread"));
    while present {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                result = Err(io::Error::last_os_error());
            } else {
                result = if unsafe { ResumeThread(thread) } == u32::MAX {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                };
                unsafe {
                    CloseHandle(thread);
                }
            }
            break;
        }
        present = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }
    result
}

pub(super) fn shell() -> PathBuf {
    if let Some(programs) = std::env::var_os("ProgramFiles") {
        let pwsh = PathBuf::from(programs).join("PowerShell/7/pwsh.exe");
        if pwsh.is_file() {
            return pwsh;
        }
    }
    // Windows PowerShell is bundled with supported desktop Windows versions.
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"))
        .join("System32/WindowsPowerShell/v1.0/powershell.exe")
}

/// Keep canonical paths for access checks, but hand shells a normal DOS/UNC
/// working directory. PowerShell's provider location can otherwise differ from
/// the process directory, and it uses that provider location for native children.
pub(super) fn shell_working_directory(root: &Path) -> io::Result<PathBuf> {
    let mut components = root.components();
    let mut directory = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:", char::from(drive))),
            Prefix::VerbatimUNC(server, share) => {
                let mut directory = PathBuf::from(r"\\");
                directory.push(server);
                directory.push(share);
                directory
            }
            _ => return Ok(root.to_owned()),
        },
        _ => return Err(io::Error::other("shell working directory must be absolute")),
    };
    directory.extend(components);
    // Removing the verbatim prefix can change names ending in a dot or space.
    // Never silently execute in a different directory after normalizing syntax.
    if fs::canonicalize(&directory)? != root {
        return Err(io::Error::other(
            "workspace cannot be represented as a shell working directory",
        ));
    }
    Ok(directory)
}

pub(super) fn shell_script(
    script: &str,
    directory: &Path,
    environment: &BTreeMap<&str, OsString>,
) -> io::Result<String> {
    let directory = single_quoted_contents(
        directory
            .to_str()
            .ok_or_else(|| io::Error::other("shell working directory is not valid Unicode"))?,
    );
    // AppContainer startup rewrites TEMP, TMP and LOCALAPPDATA to its package
    // profile. Restore the host's already-filtered environment after entering
    // the token, so tools use the scratch directory that the ACL lease grants.
    // This changes path values, never file permissions or the sandbox token.
    let mut restore_environment = String::new();
    for key in ["TEMP", "TMP", "TMPDIR", "LOCALAPPDATA"] {
        if let Some(value) = environment.get(key) {
            let value = single_quoted_contents(
                value
                    .to_str()
                    .ok_or_else(|| io::Error::other("shell environment is not valid Unicode"))?,
            );
            restore_environment.push_str(&format!(
                "[System.Environment]::SetEnvironmentVariable('{key}', '{value}', 'Process'); "
            ));
        }
    }
    // PowerShell maintains a per-runspace location separately from .NET's
    // process-wide directory. Set both before executing the user's script and
    // fail closed if provider initialization cannot reach the selected folder.
    Ok(format!(
        "try {{ {restore_environment}Set-Location -LiteralPath '{directory}' -ErrorAction Stop; [System.Environment]::CurrentDirectory = '{directory}' }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}; [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [Console]::OutputEncoding; {script}"
    ))
}

fn single_quoted_contents(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        // PowerShell's IsSingleQuote recognizes smart quotes as delimiters too.
        // Doubling each delimiter preserves the original Unicode character.
        if matches!(character, '\'' | '\u{2018}'..='\u{201b}') {
            escaped.push(character);
        }
        escaped.push(character);
    }
    escaped
}
