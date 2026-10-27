use super::wide;
use std::{
    ffi::OsString,
    io,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    ptr,
};
use windows_sys::{
    Wdk::Storage::FileSystem::{NtQuerySecurityObject, NtSetSecurityObject},
    Win32::{
        Foundation::{
            CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree, RtlNtStatusToDosError,
            STATUS_BUFFER_TOO_SMALL,
        },
        Security::{
            ACCESS_ALLOWED_ACE, ACL,
            Authorization::{
                ACCESS_MODE, DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE,
                REVOKE_ACCESS, SetEntriesInAclW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
            },
            DACL_SECURITY_INFORMATION, DeleteAce, EqualSid, GENERIC_MAPPING, GetAce,
            GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSidIdentifierAuthority,
            GetSidSubAuthority, GetSidSubAuthorityCount, InitializeSecurityDescriptor,
            IsWellKnownSid, MapGenericMask, PSECURITY_DESCRIPTOR, PSID, SE_DACL_AUTO_INHERIT_REQ,
            SE_DACL_AUTO_INHERITED, SE_DACL_DEFAULTED, SE_DACL_PROTECTED, SECURITY_DESCRIPTOR,
            SUB_CONTAINERS_AND_OBJECTS_INHERIT, SetSecurityDescriptorControl,
            SetSecurityDescriptorDacl, WinCapabilityInternetClientSid,
            WinCapabilityPrivateNetworkClientServerSid,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_ALL_ACCESS, FILE_APPEND_DATA,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
            FILE_GENERIC_WRITE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, GetFileInformationByHandle,
            GetFinalPathNameByHandleW, OPEN_EXISTING, READ_CONTROL, WRITE_DAC, WRITE_OWNER,
        },
    },
};

/// Open the named object itself, never a junction/symlink's destination. The
/// handle needs only ACL access: MAXIMUM_ALLOWED also requests DELETE, which
/// conflicts with Windows' current-directory handle and files open without
/// delete sharing. The per-object setter never walks descendants; directory
/// ACEs still apply to newly created children. Walk checked children explicitly.
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
            READ_CONTROL | WRITE_DAC
        },
    )?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    bool_check(unsafe { GetFileInformationByHandle(handle.0, &mut info) })?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other(
            "sandbox ACL target became a reparse point",
        ));
    }
    let descriptor = NativeDescriptor::read(handle.0)?;
    let original = descriptor.dacl()?;
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
        let writable = open(path, READ_CONTROL | WRITE_DAC)?;
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
        // Package/capability SID access checks do not use DENY ACEs to subtract
        // existing ALLOW rights. Keep denies as command-owned policy markers,
        // but enforce them by removing the bits from every matching ALLOW ACE.
        // The markers also prevent a later overlapping grant from restoring them.
        let mut forbidden = if mode == DENY_ACCESS {
            file_mask(rights)
        } else {
            0
        };
        for index in 0..unsafe { (*original).AceCount as u32 } {
            let mut ace = ptr::null_mut();
            bool_check(unsafe { GetAce(original, index, &mut ace) })?;
            let entry = ace.cast::<ACCESS_ALLOWED_ACE>();
            if unsafe {
                (*entry).Header.AceType == 1
                    && EqualSid(ptr::addr_of_mut!((*entry).SidStart).cast(), sid) != 0
            } {
                forbidden |= file_mask(unsafe { (*entry).Mask });
            }
        }
        let hardlink_deny = if mode == GRANT_ACCESS && info.nNumberOfLinks > 1 {
            Some(if file_mask(rights) & FILE_WRITE_DATA != 0 {
                windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS
            } else {
                WRITE_RIGHTS
            })
        } else {
            None
        };
        forbidden |= hardlink_deny.unwrap_or_default();
        // Package DENY entries cannot revoke grants to ambient AppContainer or
        // capability SIDs. Reject conflicts without modifying other trustees.
        for index in 0..unsafe { (*original).AceCount as u32 } {
            let mut ace = ptr::null_mut();
            bool_check(unsafe { GetAce(original, index, &mut ace) })?;
            let entry = ace.cast::<ACCESS_ALLOWED_ACE>();
            if unsafe {
                matches!((*entry).Header.AceType, 4 | 5 | 9 | 11)
                    && file_mask((*entry).Mask) & forbidden != 0
            } {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "conditional or object-specific allow ACL cannot be safely restricted for this command",
                ));
            }
            if unsafe { (*entry).Header.AceType == 0 && file_mask((*entry).Mask) & forbidden != 0 }
            {
                let trustee = unsafe { ptr::addr_of_mut!((*entry).SidStart).cast() };
                if ambient_package_sid(trustee)
                    || [
                        WinCapabilityInternetClientSid,
                        WinCapabilityPrivateNetworkClientServerSid,
                    ]
                    .into_iter()
                    .any(|kind| unsafe { IsWellKnownSid(trustee, kind) } != 0)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "pre-existing AppContainer or network-capability grant conflicts with sandbox restrictions",
                    ));
                }
            }
        }
        let bytes = unsafe { (*original).AclSize as usize };
        filtered = vec![0u32; bytes.div_ceil(4)];
        unsafe {
            ptr::copy_nonoverlapping(original.cast::<u8>(), filtered.as_mut_ptr().cast(), bytes);
        }
        let reduced = filtered.as_mut_ptr().cast::<ACL>();
        for index in (0..unsafe { (*reduced).AceCount as u32 }).rev() {
            let mut ace = ptr::null_mut();
            bool_check(unsafe { GetAce(reduced, index, &mut ace) })?;
            let entry = ace.cast::<ACCESS_ALLOWED_ACE>();
            if unsafe {
                (*entry).Header.AceType == 0
                    && EqualSid(ptr::addr_of_mut!((*entry).SidStart).cast(), sid) != 0
            } {
                unsafe {
                    (*entry).Mask = file_mask((*entry).Mask) & !forbidden;
                }
                if unsafe { (*entry).Mask } == 0 {
                    bool_check(unsafe { DeleteAce(reduced, index) })?;
                }
            }
        }
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: if mode == GRANT_ACCESS {
                file_mask(rights) & !forbidden
            } else {
                rights
            },
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
        if let Some(deny) = hardlink_deny {
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
                reduced,
                &mut updated,
            )
        })?;
        allocated = Allocation(updated.cast());
        allocated.0.cast::<ACL>()
    };
    set_dacl(handle.0, descriptor.pointer(), updated)
}

fn file_mask(mut mask: u32) -> u32 {
    let mapping = GENERIC_MAPPING {
        GenericRead: FILE_GENERIC_READ,
        GenericWrite: FILE_GENERIC_WRITE,
        GenericExecute: FILE_GENERIC_EXECUTE,
        GenericAll: FILE_ALL_ACCESS,
    };
    unsafe {
        MapGenericMask(&mut mask, &mapping);
    }
    mask
}

fn ambient_package_sid(sid: PSID) -> bool {
    // Regular AppContainers match both catch-all package SIDs. The restricted
    // package SID (S-1-15-2-2) has no WELL_KNOWN_SID_TYPE in this SDK.
    unsafe {
        (*GetSidIdentifierAuthority(sid)).Value == [0, 0, 0, 0, 0, 15]
            && *GetSidSubAuthorityCount(sid) == 2
            && *GetSidSubAuthority(sid, 0) == 2
            && matches!(*GetSidSubAuthority(sid, 1), 1 | 2)
    }
}

// GetSecurityInfo interprets legacy inheritance against the current parent and
// can return synthesized protection flags/ACE flags. Read the stored descriptor
// directly so a per-object update never persists that reconstructed state.
struct NativeDescriptor(Vec<u32>);
impl NativeDescriptor {
    fn read(handle: HANDLE) -> io::Result<Self> {
        let mut needed = 0;
        let status = unsafe {
            NtQuerySecurityObject(
                handle,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        if status != STATUS_BUFFER_TOO_SMALL {
            nt_check(status)?;
        }
        // NTFS security descriptors have a documented 64 KiB maximum.
        if needed == 0 || needed > 64 * 1024 {
            return Err(io::Error::other(
                "invalid filesystem security descriptor size",
            ));
        }
        let mut result = Self(vec![0u32; (needed as usize).div_ceil(4)]);
        nt_check(unsafe {
            NtQuerySecurityObject(
                handle,
                DACL_SECURITY_INFORMATION,
                result.0.as_mut_ptr().cast(),
                (result.0.len() * 4) as u32,
                &mut needed,
            )
        })?;
        Ok(result)
    }
    fn pointer(&self) -> PSECURITY_DESCRIPTOR {
        self.0.as_ptr().cast_mut().cast()
    }
    fn dacl(&self) -> io::Result<*mut ACL> {
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = ptr::null_mut();
        bool_check(unsafe {
            GetSecurityDescriptorDacl(self.pointer(), &mut present, &mut acl, &mut defaulted)
        })?;
        Ok(if present == 0 { ptr::null_mut() } else { acl })
    }
}

fn set_dacl(handle: HANDLE, original: PSECURITY_DESCRIPTOR, acl: *const ACL) -> io::Result<()> {
    let mut control = 0;
    let mut revision = 0;
    bool_check(unsafe { GetSecurityDescriptorControl(original, &mut control, &mut revision) })?;
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let descriptor = ptr::from_mut(&mut descriptor).cast();
    bool_check(unsafe { InitializeSecurityDescriptor(descriptor, revision) })?;
    bool_check(unsafe {
        SetSecurityDescriptorDacl(
            descriptor,
            1,
            acl,
            i32::from(control & SE_DACL_DEFAULTED != 0),
        )
    })?;
    let inheritance = SE_DACL_PROTECTED | SE_DACL_AUTO_INHERITED | SE_DACL_AUTO_INHERIT_REQ;
    bool_check(unsafe {
        SetSecurityDescriptorControl(descriptor, inheritance, control & inheritance)
    })?;
    // SetSecurityInfo recursively propagates directory ACEs when used with a
    // narrowly opened handle. NtSetSecurityObject updates this checked object
    // only, preserving our explicit traversal and per-object protection checks.
    // Microsoft documents this user-mode entry point and its WRITE_DAC access:
    // https://learn.microsoft.com/windows-hardware/drivers/ddi/ntifs/nf-ntifs-zwsetsecurityobject
    nt_check(unsafe { NtSetSecurityObject(handle, DACL_SECURITY_INFORMATION, descriptor) })
}

fn nt_check(status: i32) -> io::Result<()> {
    if status < 0 {
        Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ))
    } else {
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };
    use windows_sys::Win32::{
        Foundation::GENERIC_READ,
        Security::{
            Authorization::ConvertStringSidToSidW, FreeSid,
            Isolation::DeriveAppContainerSidFromAppContainerName,
        },
        Storage::FileSystem::FILE_GENERIC_READ,
    };

    struct Fixture {
        root: PathBuf,
        sid: PSID,
    }
    impl Fixture {
        fn new() -> Self {
            let name = format!(
                "OpenHarness.AclTest.{}.{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let root = std::env::temp_dir().join(&name);
            fs::create_dir(&root).unwrap();
            let mut sid = ptr::null_mut();
            assert!(
                unsafe {
                    DeriveAppContainerSidFromAppContainerName(
                        wide(&name).unwrap().as_ptr(),
                        &mut sid,
                    )
                } >= 0
            );
            Self {
                root: fs::canonicalize(root).unwrap(),
                sid,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
            unsafe {
                FreeSid(self.sid);
            }
        }
    }

    fn inspect(path: &Path, sid: PSID) -> (u16, Vec<Vec<u8>>, bool) {
        let handle = open(path, READ_CONTROL).unwrap();
        let descriptor = NativeDescriptor::read(handle.0).unwrap();
        let acl = descriptor.dacl().unwrap();
        assert!(!acl.is_null());
        let mut control = 0;
        let mut revision = 0;
        bool_check(unsafe {
            GetSecurityDescriptorControl(descriptor.pointer(), &mut control, &mut revision)
        })
        .unwrap();
        let mut entries = Vec::new();
        let mut present = false;
        for index in 0..unsafe { (*acl).AceCount as u32 } {
            let mut ace = ptr::null_mut();
            bool_check(unsafe { GetAce(acl, index, &mut ace) }).unwrap();
            let entry = ace.cast::<ACCESS_ALLOWED_ACE>();
            unsafe {
                entries.push(
                    std::slice::from_raw_parts(ace.cast::<u8>(), (*entry).Header.AceSize as usize)
                        .to_vec(),
                );
                present |= (*entry).Header.AceType <= 1
                    && EqualSid(ptr::addr_of_mut!((*entry).SidStart).cast(), sid) != 0;
            }
        }
        (control, entries, present)
    }

    fn allowed_mask(path: &Path, sid: PSID) -> u32 {
        let handle = open(path, READ_CONTROL).unwrap();
        let descriptor = NativeDescriptor::read(handle.0).unwrap();
        let acl = descriptor.dacl().unwrap();
        let mut rights = 0;
        for index in 0..unsafe { (*acl).AceCount as u32 } {
            let mut ace = ptr::null_mut();
            bool_check(unsafe { GetAce(acl, index, &mut ace) }).unwrap();
            let entry = ace.cast::<ACCESS_ALLOWED_ACE>();
            if unsafe {
                (*entry).Header.AceType == 0
                    && EqualSid(ptr::addr_of_mut!((*entry).SidStart).cast(), sid) != 0
            } {
                rights |= file_mask(unsafe { (*entry).Mask });
            }
        }
        rights
    }

    #[test]
    fn restrictions_remove_package_allow_bits_and_later_grants_cannot_restore_them() {
        let fixture = Fixture::new();
        let peer = Fixture::new();
        let file = fixture.root.join("config");
        fs::write(&file, "original").unwrap();
        let read_write = FILE_GENERIC_READ | FILE_GENERIC_WRITE;
        change(&file, peer.sid, read_write, GRANT_ACCESS).unwrap();
        change(&file, fixture.sid, read_write, GRANT_ACCESS).unwrap();
        change(&file, fixture.sid, WRITE_RIGHTS, DENY_ACCESS).unwrap();
        assert_eq!(allowed_mask(&file, fixture.sid) & WRITE_RIGHTS, 0);
        assert_eq!(
            allowed_mask(&file, fixture.sid) & FILE_GENERIC_READ,
            FILE_GENERIC_READ
        );
        change(&file, fixture.sid, read_write, GRANT_ACCESS).unwrap();
        assert_eq!(allowed_mask(&file, fixture.sid) & WRITE_RIGHTS, 0);
        assert_eq!(allowed_mask(&file, peer.sid), read_write);
        change(&file, fixture.sid, FILE_ALL_ACCESS, DENY_ACCESS).unwrap();
        assert_eq!(allowed_mask(&file, fixture.sid), 0);
        change(&file, fixture.sid, 0, REVOKE_ACCESS).unwrap();
        assert!(!inspect(&file, fixture.sid).2);
        assert_eq!(allowed_mask(&file, peer.sid), read_write);
    }

    #[test]
    fn conflicting_ambient_package_grants_fail_closed_without_changing_them() {
        let fixture = Fixture::new();
        let file = fixture.root.join("input.txt");
        fs::write(&file, "original").unwrap();
        for text in ["S-1-15-2-1", "S-1-15-2-2"] {
            let mut packages = ptr::null_mut();
            bool_check(unsafe {
                ConvertStringSidToSidW(wide(text).unwrap().as_ptr(), &mut packages)
            })
            .unwrap();
            let _packages = Allocation(packages);
            for (path, grant, restriction) in [
                (&fixture.root, FILE_DELETE_CHILD, FILE_DELETE_CHILD),
                (&file, FILE_GENERIC_READ | FILE_GENERIC_WRITE, WRITE_RIGHTS),
            ] {
                // A non-inheritable parent DELETE_CHILD grant is not visible in
                // a protected child's ACL, but still authorizes its deletion.
                change_without_inheritance(path, packages, grant, GRANT_ACCESS).unwrap();
                let before = inspect(path, fixture.sid);
                let error = change(path, fixture.sid, restriction, DENY_ACCESS).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
                assert_eq!(inspect(path, fixture.sid), before);
                change(path, packages, 0, REVOKE_ACCESS).unwrap();
            }
        }
    }

    #[test]
    fn acl_changes_work_while_directory_and_file_handles_deny_delete_sharing() {
        let fixture = Fixture::new();
        let file = fixture.root.join("in-use.txt");
        fs::write(&file, "unchanged").unwrap();
        for path in [&fixture.root, &file] {
            // A process's current directory denies delete sharing. Open files
            // can also deny write sharing without preventing ACL-only changes.
            let directory = path.is_dir();
            let locked = unsafe {
                CreateFileW(
                    wide(path).unwrap().as_ptr(),
                    GENERIC_READ,
                    FILE_SHARE_READ | if directory { FILE_SHARE_WRITE } else { 0 },
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    ptr::null_mut(),
                )
            };
            assert_ne!(
                locked,
                INVALID_HANDLE_VALUE,
                "{}",
                io::Error::last_os_error()
            );
            let _locked = Handle(locked);
            let before = inspect(path, fixture.sid);
            change(path, fixture.sid, FILE_GENERIC_READ, GRANT_ACCESS).unwrap();
            assert!(inspect(path, fixture.sid).2);
            change(path, fixture.sid, WRITE_RIGHTS, DENY_ACCESS).unwrap();
            change(path, fixture.sid, 0, REVOKE_ACCESS).unwrap();
            assert_eq!(inspect(path, fixture.sid), before);
        }
        assert_eq!(fs::read_to_string(file).unwrap(), "unchanged");
    }

    #[test]
    fn directory_acl_changes_do_not_propagate_to_unchecked_existing_children() {
        let fixture = Fixture::new();
        let existing = fixture.root.join("existing.txt");
        fs::write(&existing, "existing").unwrap();
        let before = inspect(&existing, fixture.sid);
        let original_control = inspect(&fixture.root, fixture.sid).0;
        change(&fixture.root, fixture.sid, FILE_GENERIC_READ, GRANT_ACCESS).unwrap();
        assert_eq!(inspect(&fixture.root, fixture.sid).0, original_control);
        assert_eq!(inspect(&existing, fixture.sid), before);
        let future = fixture.root.join("future.txt");
        fs::write(&future, "future").unwrap();
        assert!(
            inspect(&future, fixture.sid).2,
            "new files must retain intended inheritance"
        );
        change(&fixture.root, fixture.sid, 0, REVOKE_ACCESS).unwrap();
        assert!(
            inspect(&future, fixture.sid).2,
            "cleanup must not silently walk descendants"
        );
        change(&future, fixture.sid, 0, REVOKE_ACCESS).unwrap();
        assert!(!inspect(&future, fixture.sid).2);
        assert_eq!(inspect(&existing, fixture.sid), before);
    }
}
