//! CI normally runs elevated. Exercise the complete launcher from a child
//! whose Administrators SID is deny-only and whose privileges were removed,
//! rather than accidentally relying on WRITE_DAC at a system drive root.
use std::{ffi::OsStr, os::windows::ffi::OsStrExt, ptr};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_INSUFFICIENT_BUFFER, GENERIC_ALL, HANDLE, HANDLE_FLAG_INHERIT,
        INVALID_HANDLE_VALUE, LocalFree, SetHandleInformation, WAIT_OBJECT_0,
    },
    Security::{
        ACL,
        Authorization::{
            EXPLICIT_ACCESS_W, GRANT_ACCESS, SetEntriesInAclW, TRUSTEE_IS_SID, TRUSTEE_IS_USER,
            TRUSTEE_W,
        },
        CheckTokenMembership, CreateRestrictedToken, CreateWellKnownSid, DISABLE_MAX_PRIVILEGE,
        GetTokenInformation, SID_AND_ATTRIBUTES, SetTokenInformation, TOKEN_ADJUST_DEFAULT,
        TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE, TOKEN_INFORMATION_CLASS,
        TOKEN_QUERY, TOKEN_USER, TokenDefaultDacl, TokenUser, WinBuiltinAdministratorsSid,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING, WRITE_DAC,
    },
    System::{
        Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
        Threading::{
            CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken,
            PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW, TerminateProcess,
            WaitForSingleObject,
        },
    },
};

pub(super) fn run_without_administrator_rights(test: &str) -> bool {
    let mut admin = [0u32; 17];
    let mut length = size_of_val(&admin) as u32;
    check("CreateWellKnownSid", unsafe {
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            ptr::null_mut(),
            admin.as_mut_ptr().cast(),
            &mut length,
        )
    });
    let mut member = 0;
    check("CheckTokenMembership", unsafe {
        CheckTokenMembership(ptr::null_mut(), admin.as_mut_ptr().cast(), &mut member)
    });
    if member == 0 {
        return false;
    }

    let mut original = ptr::null_mut();
    // CreateRestrictedToken preserves these handle access rights. Its result
    // must remain assignable as the primary token of the test-only child.
    check("OpenProcessToken", unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ASSIGN_PRIMARY | TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ADJUST_DEFAULT,
            &mut original,
        )
    });
    let original = Handle(original);
    let disabled = SID_AND_ATTRIBUTES {
        Sid: admin.as_mut_ptr().cast(),
        Attributes: 0,
    };
    let mut restricted = ptr::null_mut();
    check("CreateRestrictedToken", unsafe {
        CreateRestrictedToken(
            original.0,
            DISABLE_MAX_PRIVILEGE,
            1,
            &disabled,
            0,
            ptr::null(),
            0,
            ptr::null(),
            &mut restricted,
        )
    });
    let restricted = Handle(restricted);
    grant_user_default_dacl(restricted.0);
    let program = std::env::current_exe().unwrap();
    let mut command = wide(format!(
        "\"{}\" --exact {test} --nocapture",
        program.display()
    ));
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_USESTDHANDLES,
        hStdInput: unsafe { GetStdHandle(STD_INPUT_HANDLE) },
        hStdOutput: unsafe { GetStdHandle(STD_OUTPUT_HANDLE) },
        hStdError: unsafe { GetStdHandle(STD_ERROR_HANDLE) },
        ..Default::default()
    };
    for handle in [startup.hStdInput, startup.hStdOutput, startup.hStdError] {
        if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
            check("SetHandleInformation", unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)
            });
        }
    }
    let mut process = PROCESS_INFORMATION::default();
    check("CreateProcessAsUserW", unsafe {
        CreateProcessAsUserW(
            restricted.0,
            wide(&program).as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            0,
            ptr::null(),
            ptr::null(),
            &startup,
            &mut process,
        )
    });
    let process_handle = Handle(process.hProcess);
    let _thread = Handle(process.hThread);
    let wait = unsafe { WaitForSingleObject(process_handle.0, 120_000) };
    if wait != WAIT_OBJECT_0 {
        unsafe {
            TerminateProcess(process_handle.0, 125);
            WaitForSingleObject(process_handle.0, 10_000);
        }
        panic!("non-administrator runtime regression did not complete: {wait}");
    }
    let mut code = 125;
    check("GetExitCodeProcess", unsafe {
        GetExitCodeProcess(process_handle.0, &mut code)
    });
    assert_eq!(
        code, 0,
        "runtime regression failed without administrator rights"
    );
    true
}

fn grant_user_default_dacl(token: HANDLE) {
    // An elevated token's default object ACL can grant only Administrators
    // and SYSTEM. After making Administrators deny-only, such an ACL prevents
    // the child from opening its own pipes/processes. Match a regular user's
    // default object ACL by granting this account access, preserving the
    // existing entries. This does not change any filesystem permissions.
    let user = token_information(token, TokenUser);
    let user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
    let defaults = token_information(token, TokenDefaultDacl);
    let defaults = unsafe { &*defaults.as_ptr().cast::<TOKEN_DEFAULT_DACL>() };
    assert!(
        !defaults.DefaultDacl.is_null(),
        "standard-user test token must have an explicit default object DACL"
    );
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: 0,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: user.User.Sid.cast(),
            ..Default::default()
        },
    };
    let mut acl = ptr::null_mut();
    let code = unsafe { SetEntriesInAclW(1, &entry, defaults.DefaultDacl, &mut acl) };
    assert_eq!(
        code,
        0,
        "standard-user default object ACL: {}",
        std::io::Error::from_raw_os_error(code as i32)
    );
    let acl = LocalAcl(acl);
    let defaults = TOKEN_DEFAULT_DACL { DefaultDacl: acl.0 };
    check("SetTokenInformation(TokenDefaultDacl)", unsafe {
        SetTokenInformation(
            token,
            TokenDefaultDacl,
            ptr::from_ref(&defaults).cast(),
            size_of::<TOKEN_DEFAULT_DACL>() as u32,
        )
    });
}

fn token_information(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Vec<usize> {
    let mut length = 0;
    let result = unsafe { GetTokenInformation(token, class, ptr::null_mut(), 0, &mut length) };
    assert_eq!(
        result, 0,
        "token information size query unexpectedly succeeded"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(ERROR_INSUFFICIENT_BUFFER as i32),
        "GetTokenInformation({class}) size query failed"
    );
    let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    check("GetTokenInformation", unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    });
    buffer
}

pub(super) fn assert_system_root_acl_is_not_writable() {
    let root = format!("{}\\", std::env::var("SystemDrive").unwrap());
    let handle = unsafe {
        CreateFileW(
            wide(root).as_ptr(),
            WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
        unsafe {
            CloseHandle(handle);
        }
        panic!("runtime regression token can still rewrite the system root ACL");
    }
    assert_eq!(
        std::io::Error::last_os_error().kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}
#[track_caller]
fn check(operation: &str, value: i32) {
    assert_ne!(
        value,
        0,
        "native standard-user test setup {operation}: {}",
        std::io::Error::last_os_error()
    );
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct LocalAcl(*mut ACL);
impl Drop for LocalAcl {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0.cast());
        }
    }
}
