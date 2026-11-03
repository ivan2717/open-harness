//! Read-only volume-name resolution for native realpath/canonicalize. Windows
//! resolves DOS paths through these fixed NT objects, independently of access
//! to the file itself. Only explicit elevated host preparation changes DACLs;
//! never accept object names, rights or capabilities from command input.
use super::{acl, metadata, wide};
use std::{
    collections::BTreeSet,
    io,
    path::{Component, PathBuf, Prefix},
    ptr,
    sync::OnceLock,
};
use windows_sys::{
    Wdk::Foundation::OBJECT_ATTRIBUTES,
    Win32::{
        Foundation::{
            CloseHandle, HANDLE, INVALID_HANDLE_VALUE, RtlNtStatusToDosError, UNICODE_STRING,
        },
        Security::{GENERIC_MAPPING, PSID},
        Storage::FileSystem::{
            CreateFileW, FILE_ALL_ACCESS, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ,
            FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, READ_CONTROL,
            WRITE_DAC,
        },
    },
};

const CAPABILITY: &str = "OpenHarness.Command.VolumeQuery.v1";
const DIRECTORY_QUERY: u32 = 0x1;
const DIRECTORY_TRAVERSE: u32 = 0x2;
const SYMBOLIC_LINK_QUERY: u32 = 0x1;
const STANDARD_RIGHTS_REQUIRED: u32 = 0x000f_0000;
// Query-DOS-volume-path IOCTLs are FILE_ANY_ACCESS. Do not grant GENERIC_READ:
// other mount-manager controls requiring FILE_READ_ACCESS can change state.
const MOUNT_MANAGER_RIGHTS: u32 = metadata::RIGHTS;

// These documented ntdll entry points open NT object-manager objects, not
// filesystem directories or filesystem reparse points.
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtOpenDirectoryObject(
        handle: *mut HANDLE,
        access: u32,
        attributes: *const OBJECT_ATTRIBUTES,
    ) -> i32;
    fn NtOpenSymbolicLinkObject(
        handle: *mut HANDLE,
        access: u32,
        attributes: *const OBJECT_ATTRIBUTES,
    ) -> i32;
}

pub(super) fn sid() -> io::Result<PSID> {
    static SID: OnceLock<Result<Vec<u32>, i32>> = OnceLock::new();
    SID.get_or_init(|| {
        metadata::derive_sid(CAPABILITY).map_err(|error| error.raw_os_error().unwrap_or(13))
    })
    .as_ref()
    .map(|words| words.as_ptr().cast_mut().cast())
    .map_err(|code| io::Error::from_raw_os_error(*code))
}

#[derive(Clone, Copy)]
enum Kind {
    Directory,
    Link,
    MountManager,
}

struct Target {
    name: String,
    kind: Kind,
}

impl Target {
    fn display(&self) -> PathBuf {
        PathBuf::from(format!(r"\\?\GLOBALROOT{}", self.name))
    }

    fn rights(&self) -> u32 {
        match self.kind {
            Kind::Directory => DIRECTORY_QUERY | DIRECTORY_TRAVERSE | READ_CONTROL,
            Kind::Link => SYMBOLIC_LINK_QUERY | READ_CONTROL,
            Kind::MountManager => MOUNT_MANAGER_RIGHTS,
        }
    }

    fn mapping(&self) -> GENERIC_MAPPING {
        match self.kind {
            Kind::Directory => GENERIC_MAPPING {
                GenericRead: READ_CONTROL | DIRECTORY_QUERY,
                GenericWrite: READ_CONTROL | 0xc,
                GenericExecute: READ_CONTROL | DIRECTORY_TRAVERSE,
                GenericAll: STANDARD_RIGHTS_REQUIRED | 0xf,
            },
            Kind::Link => GENERIC_MAPPING {
                GenericRead: READ_CONTROL | SYMBOLIC_LINK_QUERY,
                GenericWrite: READ_CONTROL,
                GenericExecute: READ_CONTROL | SYMBOLIC_LINK_QUERY,
                GenericAll: STANDARD_RIGHTS_REQUIRED | SYMBOLIC_LINK_QUERY,
            },
            Kind::MountManager => GENERIC_MAPPING {
                GenericRead: FILE_GENERIC_READ,
                GenericWrite: FILE_GENERIC_WRITE,
                GenericExecute: FILE_GENERIC_EXECUTE,
                GenericAll: FILE_ALL_ACCESS,
            },
        }
    }

    fn ready(&self) -> io::Result<bool> {
        let handle = match self.open(READ_CONTROL | self.rights()) {
            Ok(handle) => handle,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(false),
            Err(error) => return Err(error),
        };
        acl::has_capability_access_with_mapping(handle.0, sid()?, self.rights(), &self.mapping())
    }

    fn open(&self, access: u32) -> io::Result<Handle> {
        if matches!(self.kind, Kind::MountManager) {
            // Bypass caller-local DOS-device mappings and address only the
            // kernel's fixed mount manager. This is not a volume data handle.
            let handle = unsafe {
                CreateFileW(
                    wide(self.display())?.as_ptr(),
                    access,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    ptr::null(),
                    OPEN_EXISTING,
                    0,
                    ptr::null_mut(),
                )
            };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            return Ok(Handle(handle));
        }
        let mut name = wide(&self.name)?;
        let mut name = UNICODE_STRING {
            Length: ((name.len() - 1) * 2)
                .try_into()
                .map_err(io::Error::other)?,
            MaximumLength: (name.len() * 2).try_into().map_err(io::Error::other)?,
            Buffer: name.as_mut_ptr(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            ObjectName: &mut name,
            Attributes: 0x40, // OBJ_CASE_INSENSITIVE; no caller-local namespace.
            ..Default::default()
        };
        let mut handle = ptr::null_mut();
        let status = unsafe {
            match self.kind {
                Kind::Directory => NtOpenDirectoryObject(&mut handle, access, &attributes),
                Kind::Link => NtOpenSymbolicLinkObject(&mut handle, access, &attributes),
                Kind::MountManager => unreachable!(),
            }
        };
        if status < 0 {
            return Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(status) } as i32,
            ));
        }
        Ok(Handle(handle))
    }
}

/// Roots were resolved from Windows' available fixed drives by metadata.rs.
/// No caller-provided filesystem or object path is accepted by host preparation.
fn targets(roots: &[PathBuf]) -> Vec<Target> {
    let mut result = vec![
        Target {
            name: r"\GLOBAL??".into(),
            kind: Kind::Directory,
        },
        Target {
            name: r"\GLOBAL??\MountPointManager".into(),
            kind: Kind::Link,
        },
        Target {
            name: r"\Device\MountPointManager".into(),
            kind: Kind::MountManager,
        },
    ];
    let mut drives = BTreeSet::new();
    for root in roots {
        if let Some(Component::Prefix(prefix)) = root.components().next()
            && let Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) = prefix.kind()
        {
            drives.insert(char::from(drive).to_ascii_uppercase());
        }
    }
    result.extend(drives.into_iter().map(|drive| Target {
        name: format!(r"\GLOBAL??\{drive}:"),
        kind: Kind::Link,
    }));
    result
}

pub(super) fn status(roots: &[PathBuf]) -> io::Result<Vec<(PathBuf, bool)>> {
    targets(roots)
        .into_iter()
        .map(|target| {
            let ready = target.ready()?;
            Ok((target.display(), ready))
        })
        .collect()
}

/// Called under the host-preparation ACL mutex, after explicit elevation.
pub(super) fn prepare(roots: &[PathBuf]) -> io::Result<()> {
    for target in targets(roots) {
        if !target.ready()? {
            let handle = target.open(READ_CONTROL | WRITE_DAC)?;
            // DACL only: no ownership, integrity-label or namespace changes.
            // No create, delete, retarget, volume-write or file-data grants.
            acl::grant_to_handle(handle.0, sid()?, target.rights())?;
        }
    }
    Ok(())
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
