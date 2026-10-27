use std::{io, path::PathBuf, ptr};
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
