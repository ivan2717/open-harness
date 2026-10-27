use super::{Request, wide};
use std::{io, ptr};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GENERIC_READ, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
        SetHandleInformation, WAIT_OBJECT_0,
    },
    Security::{
        CreateWellKnownSid, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
        WinCapabilityInternetClientSid, WinCapabilityPrivateNetworkClientServerSid,
    },
    Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
    System::{
        Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE},
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
            EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
            InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
            TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

pub(super) fn execute(request: &Request, sid: PSID) -> io::Result<u32> {
    let program = wide(&request.program)?;
    let cwd = wide(&request.cwd)?;
    let mut arguments = vec![request.program.to_string_lossy().into_owned()];
    arguments.extend(request.args.iter().cloned());
    let mut command = wide(
        arguments
            .iter()
            .map(|argument| quote(argument))
            .collect::<Vec<_>>()
            .join(" "),
    )?;
    if command.len() > 32_767 {
        return Err(io::Error::other(
            "Windows command line exceeds 32767 UTF-16 units",
        ));
    }

    let mut capabilities = Vec::new();
    let mut sid_storage = [[0u32; 17]; 2];
    if request.network_allowed {
        // These capabilities allow Internet and LAN traffic. Windows still
        // blocks AppContainer loopback unless an administrator grants an OS
        // exemption; we deliberately never change that machine-wide policy.
        for (storage, kind) in sid_storage.iter_mut().zip([
            WinCapabilityInternetClientSid,
            WinCapabilityPrivateNetworkClientServerSid,
        ]) {
            let mut length = std::mem::size_of_val(storage) as u32;
            check(unsafe {
                CreateWellKnownSid(
                    kind,
                    ptr::null_mut(),
                    storage.as_mut_ptr().cast(),
                    &mut length,
                )
            })?;
            capabilities.push(SID_AND_ATTRIBUTES {
                Sid: storage.as_mut_ptr().cast(),
                Attributes: 0x4,
            });
        }
    }
    let security = SECURITY_CAPABILITIES {
        AppContainerSid: sid,
        Capabilities: if capabilities.is_empty() {
            ptr::null_mut()
        } else {
            capabilities.as_mut_ptr()
        },
        CapabilityCount: capabilities.len() as u32,
        Reserved: 0,
    };
    let attributes = Attributes::new(2)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &security)?;

    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: 1,
    };
    let null_name = wide("NUL")?;
    let stdin = Handle::new(unsafe {
        CreateFileW(
            null_name.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &sa,
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    })?;
    let handles = [
        stdin.0,
        unsafe { GetStdHandle(STD_OUTPUT_HANDLE) },
        unsafe { GetStdHandle(STD_ERROR_HANDLE) },
    ];
    for handle in handles {
        check(unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) })?;
    }
    attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = handles[0];
    startup.StartupInfo.hStdOutput = handles[1];
    startup.StartupInfo.hStdError = handles[2];
    startup.lpAttributeList = attributes.pointer();

    // A nested Job kills spawned descendants before ACL/profile cleanup, even
    // when the initial shell exits while background children are still alive.
    let job = Job::new()?;
    let mut process = PROCESS_INFORMATION::default();
    check(unsafe {
        CreateProcessW(
            program.as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW | CREATE_SUSPENDED,
            ptr::null(),
            cwd.as_ptr(),
            &startup.StartupInfo,
            &mut process,
        )
    })?;
    let process_handle = Handle(process.hProcess);
    let thread = Handle(process.hThread);
    let launch = (|| {
        check(unsafe { AssignProcessToJobObject(job.0.0, process_handle.0) })?;
        if unsafe { ResumeThread(thread.0) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })();
    if let Err(error) = launch {
        unsafe {
            TerminateProcess(process_handle.0, 125);
            WaitForSingleObject(process_handle.0, INFINITE);
        }
        return Err(error);
    }
    let result = (|| {
        if unsafe { WaitForSingleObject(process_handle.0, INFINITE) } != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        let mut exit_code = 125;
        check(unsafe { GetExitCodeProcess(process_handle.0, &mut exit_code) })?;
        Ok(exit_code)
    })();
    // All paths after resuming the untrusted process must establish teardown
    // before modifying its ACLs. Unexpected native failures defer to recovery.
    job.finish().map_err(|error| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("could not confirm sandbox process termination; cleanup deferred: {error}"),
        )
    })?;
    result
}

fn quote(value: &str) -> String {
    // CommandLineToArgvW / CRT quoting, including trailing backslashes and
    // embedded quotes. JSON strings must never become shell interpolation.
    let mut result = String::from("\"");
    let mut slashes = 0;
    for character in value.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        if character == '"' {
            result.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            result.extend(std::iter::repeat_n('\\', slashes));
        }
        slashes = 0;
        result.push(character);
    }
    result.extend(std::iter::repeat_n('\\', slashes * 2));
    result.push('"');
    result
}

struct Attributes(Vec<usize>);
impl Attributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(ptr::null_mut(), count, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        check(unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), count, 0, &mut bytes)
        })?;
        Ok(Self(storage))
    }
    fn pointer(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_ptr().cast_mut().cast()
    }
    fn set<T>(&self, key: u32, value: &T) -> io::Result<()> {
        check(unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                key as usize,
                ptr::from_ref(value).cast(),
                size_of::<T>(),
                ptr::null_mut(),
                ptr::null(),
            )
        })
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.pointer());
        }
    }
}

struct Handle(HANDLE);
impl Handle {
    fn new(value: HANDLE) -> io::Result<Self> {
        if value.is_null() || value == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(value))
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct Job(Handle);
impl Job {
    fn finish(&self) -> io::Result<()> {
        check(unsafe { TerminateJobObject(self.0.0, 1) })?;
        for _ in 0..500 {
            let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            check(unsafe {
                QueryInformationJobObject(
                    self.0.0,
                    JobObjectBasicAccountingInformation,
                    ptr::from_mut(&mut accounting).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    ptr::null_mut(),
                )
            })?;
            if accounting.ActiveProcesses == 0 {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "sandbox child processes have not finished terminating; cleanup deferred",
        ))
    }
    fn new() -> io::Result<Self> {
        let handle = Handle::new(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        check(unsafe {
            SetInformationJobObject(
                handle.0,
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        })?;
        Ok(Self(handle))
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            TerminateJobObject(self.0.0, 1);
        }
    }
}

fn check(success: i32) -> io::Result<()> {
    if success == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::quote;
    #[test]
    fn quotes_windows_arguments_without_interpreting_shell_syntax() {
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("hello world"), "\"hello world\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("C:\\path\\"), "\"C:\\path\\\\\"");
        assert_eq!(quote("$(whoami); & exit"), "\"$(whoami); & exit\"");
    }
}
