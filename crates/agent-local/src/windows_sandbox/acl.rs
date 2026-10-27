use super::wide;
use std::{
    ffi::OsString,
    io,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    ptr,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACL,
        Authorization::{
            ACCESS_MODE, DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetSecurityInfo,
            NO_MULTIPLE_TRUSTEE, REVOKE_ACCESS, SE_FILE_OBJECT, SetEntriesInAclW, SetSecurityInfo,
            TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
        },
        DACL_SECURITY_INFORMATION, DeleteAce, EqualSid, GetAce, PSID,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_APPEND_DATA,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, GetFileInformationByHandle,
        GetFinalPathNameByHandleW, OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
    },
};

// winnt.h ACCESS_MASK; requesting this explicitly disables SetSecurityInfo's
// recursive propagation (see the API documentation below).
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

/// Open the named object itself, never a junction/symlink's destination. The
/// MAXIMUM_ALLOWED handles disable SetSecurityInfo propagation, as documented
/// by Microsoft. Walk ordinary children explicitly; directory ACEs still apply
/// to newly created children. No user-wide grant or automatic junction traversal.
/// https://learn.microsoft.com/windows/win32/api/aclapi/nf-aclapi-setsecurityinfo
pub(super) fn change(path: &Path, sid: PSID, rights: u32, mode: ACCESS_MODE) -> io::Result<()> {
    change_inheritance(path, sid, rights, mode, true)
}

/// Read-only project grants cover the scanned objects, never future children
/// that have not passed the protected-name and hardlink checks.
pub(super) fn change_without_inheritance(
    path: &Path,
    sid: PSID,
    rights: u32,
    mode: ACCESS_MODE,
) -> io::Result<()> {
    change_inheritance(path, sid, rights, mode, false)
}

fn change_inheritance(
    path: &Path,
    sid: PSID,
    rights: u32,
    mode: ACCESS_MODE,
    inherit: bool,
) -> io::Result<()> {
    let mut handle = open(
        path,
        if mode == REVOKE_ACCESS {
            READ_CONTROL
        } else {
            MAXIMUM_ALLOWED
        },
    )?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    bool_check(unsafe { GetFileInformationByHandle(handle.0, &mut info) })?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other(
            "sandbox ACL target became a reparse point",
        ));
    }
    let mut descriptor = ptr::null_mut();
    let mut original: *mut ACL = ptr::null_mut();
    check(unsafe {
        GetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut original,
            ptr::null_mut(),
            &mut descriptor,
        )
    })?;
    let _descriptor = Allocation(descriptor);
    if original.is_null() {
        return Err(io::Error::other(format!(
            "sandbox requires a filesystem ACL: {}",
            path.display()
        )));
    }
    let mut filtered;
    let allocated;
    let updated = if mode == REVOKE_ACCESS {
        // SetEntriesInAcl(REVOKE_ACCESS) does not remove DENY ACEs. Remove both
        // explicit and inherited ACEs for only this command's unique package SID.
        let bytes = unsafe { (*original).AclSize as usize };
        filtered = vec![0u32; bytes.div_ceil(4)];
        unsafe {
            ptr::copy_nonoverlapping(original.cast::<u8>(), filtered.as_mut_ptr().cast(), bytes);
        }
        let acl = filtered.as_mut_ptr().cast::<ACL>();
        let mut removed = false;
        for index in (0..unsafe { (*acl).AceCount as u32 }).rev() {
            let mut ace = ptr::null_mut();
            bool_check(unsafe { GetAce(acl, index, &mut ace) })?;
            let entry = ace.cast::<ACCESS_ALLOWED_ACE>();
            // ACCESS_ALLOWED_ACE_TYPE=0 and ACCESS_DENIED_ACE_TYPE=1 share layout.
            if unsafe {
                (*entry).Header.AceType <= 1
                    && EqualSid(ptr::addr_of_mut!((*entry).SidStart).cast(), sid) != 0
            } {
                bool_check(unsafe { DeleteAce(acl, index) })?;
                removed = true;
            }
        }
        if !removed {
            return Ok(());
        }
        let writable = open(path, MAXIMUM_ALLOWED)?;
        let mut current = BY_HANDLE_FILE_INFORMATION::default();
        bool_check(unsafe { GetFileInformationByHandle(writable.0, &mut current) })?;
        if (
            current.dwVolumeSerialNumber,
            current.nFileIndexHigh,
            current.nFileIndexLow,
        ) != (
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ) {
            return Err(io::Error::other(
                "sandbox ACL target was replaced during cleanup",
            ));
        }
        handle = writable;
        acl
    } else {
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: rights,
            grfAccessMode: mode,
            grfInheritance: if inherit && std::fs::symlink_metadata(path)?.is_dir() {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                0
            },
            Trustee: TRUSTEE_W {
                pMultipleTrustee: ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: sid.cast(),
            },
        };
        let mut entries = vec![entry];
        // Check link count on the same handle we mutate, not an earlier path
        // lookup. A workspace hardlink must not make an external file writable.
        if mode == GRANT_ACCESS && info.nNumberOfLinks > 1 {
            let deny = if rights & FILE_WRITE_DATA != 0 {
                windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS
            } else {
                WRITE_RIGHTS
            };
            entries.push(EXPLICIT_ACCESS_W {
                grfAccessPermissions: deny,
                grfAccessMode: DENY_ACCESS,
                ..entry
            });
        }
        let mut updated = ptr::null_mut();
        check(unsafe {
            SetEntriesInAclW(
                entries.len() as u32,
                entries.as_ptr(),
                original,
                &mut updated,
            )
        })?;
        allocated = Allocation(updated.cast());
        allocated.0.cast::<ACL>()
    };
    check(unsafe {
        SetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            updated,
            ptr::null(),
        )
    })
}

pub(super) fn links(path: &Path) -> io::Result<u32> {
    let handle = open(path, 0)?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    bool_check(unsafe { GetFileInformationByHandle(handle.0, &mut info) })?;
    Ok(info.nNumberOfLinks)
}

fn open(path: &Path, rights: u32) -> io::Result<Handle> {
    let name = wide(path)?;
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            rights,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        let handle = Handle(handle);
        // OPEN_REPARSE_POINT only protects the final component. Check the
        // resolved handle path as well to reject replaced ancestor junctions.
        let mut resolved = vec![0u16; 32_768];
        let length = unsafe {
            GetFinalPathNameByHandleW(handle.0, resolved.as_mut_ptr(), resolved.len() as u32, 0)
        };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length as usize >= resolved.len() {
            return Err(io::Error::other("sandbox resolved path is too long"));
        }
        let resolved = PathBuf::from(OsString::from_wide(&resolved[..length as usize]));
        if !resolved.as_os_str().eq_ignore_ascii_case(path.as_os_str()) {
            return Err(io::Error::other(format!(
                "sandbox ACL target moved: {}",
                path.display()
            )));
        }
        Ok(handle)
    }
}

pub(super) const WRITE_RIGHTS: u32 = FILE_WRITE_DATA
    | FILE_DELETE_CHILD
    | FILE_APPEND_DATA
    | FILE_WRITE_ATTRIBUTES
    | FILE_WRITE_EA
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER;
fn bool_check(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn check(value: u32) -> io::Result<()> {
    if value == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(value as i32))
    }
}
struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Allocation(*mut std::ffi::c_void);
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
