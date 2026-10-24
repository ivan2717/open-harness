use std::{io, os::windows::io::AsRawHandle, process::Child, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    },
};

/// Chrome/runtime own the launcher; ending it must also end its Node child.
pub struct Job(HANDLE);

impl Job {
    pub fn new() -> io::Result<Self> {
        // SAFETY: no name/security structure is supplied; the returned handle
        // is owned by this wrapper and closed exactly once in Drop.
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(handle);
        let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // Only the dedicated Chrome launch command explicitly breaks away.
        // Ordinary Node/helper/browser children still inherit this job.
        information.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        // SAFETY: information points to the matching structure for this class,
        // and its storage remains alive until this synchronous call returns.
        if unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&information).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    pub fn assign(&self, child: &Child) -> io::Result<()> {
        // SAFETY: both handles refer to live objects for the duration of the
        // synchronous assignment; ownership remains with their wrappers.
        if unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner of the non-null job handle.
        unsafe { CloseHandle(self.0) };
    }
}
