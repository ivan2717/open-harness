//! Explicit host preparation for runtimes which stat path
//! ancestors. The named capability grants only attributes and synchronization on the
//! selected directories themselves. It cannot enumerate directories, read file
//! contents, write, or access the network. Command-specific ancestors remain
//! temporary, per-command grants in lease.rs. A separate capability enables
//! ignored stdio on the null device; Windows resets that device ACL on reboot.
use super::{acl, lease, null_device, wide};
use serde::Serialize;
use std::{
    collections::BTreeSet, ffi::OsString, io, os::windows::ffi::OsStringExt, path::PathBuf, ptr,
    sync::OnceLock,
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_DATA, ERROR_NOT_READY, LocalFree,
    },
    Security::{
        Authorization::GRANT_ACCESS, CopySid, DeriveCapabilitySidsFromName, GetLengthSid,
        GetTokenInformation, PSID, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    },
    Storage::FileSystem::{
        FILE_READ_ATTRIBUTES, GetDriveTypeW, GetLogicalDriveStringsW, SYNCHRONIZE,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
    UI::Shell::GetProfilesDirectoryW,
};

// libuv opens attributes with a synchronous CreateFileW handle. The NT open
// also requires SYNCHRONIZE; neither right permits reading directory contents.
pub(super) const RIGHTS: u32 = FILE_READ_ATTRIBUTES | SYNCHRONIZE;

const CAPABILITY: &str = "OpenHarness.Command.AncestorMetadata.v1";

/// The immutable SID lives until process exit, including CreateProcessW's use
/// of SECURITY_CAPABILITIES. Never accept a capability name from launch input.
pub(super) fn sid() -> io::Result<PSID> {
    static SID: OnceLock<Result<Vec<u32>, i32>> = OnceLock::new();
    SID.get_or_init(|| derive_sid(CAPABILITY).map_err(|error| error.raw_os_error().unwrap_or(13)))
        .as_ref()
        .map(|words| words.as_ptr().cast_mut().cast())
        .map_err(|code| io::Error::from_raw_os_error(*code))
}

pub(super) fn derive_sid(name: &str) -> io::Result<Vec<u32>> {
    let mut groups = SidArray::default();
    let mut capabilities = SidArray::default();
    check(unsafe {
        DeriveCapabilitySidsFromName(
            wide(name)?.as_ptr(),
            &mut groups.pointer,
            &mut groups.count,
            &mut capabilities.pointer,
            &mut capabilities.count,
        )
    })?;
    if capabilities.count != 1 || capabilities.pointer.is_null() {
        return Err(io::Error::from_raw_os_error(ERROR_INVALID_DATA as i32));
    }
    let source = unsafe { *capabilities.pointer };
    let length = unsafe { GetLengthSid(source) };
    let mut words = vec![0u32; (length as usize).div_ceil(4)];
    check(unsafe { CopySid(length, words.as_mut_ptr().cast(), source) })?;
    Ok(words)
}

#[derive(Default)]
struct SidArray {
    pointer: *mut PSID,
    count: u32,
}
impl Drop for SidArray {
    fn drop(&mut self) {
        if !self.pointer.is_null() {
            unsafe {
                for index in 0..self.count as usize {
                    LocalFree(*self.pointer.add(index));
                }
                LocalFree(self.pointer.cast());
            }
        }
    }
}

#[derive(Serialize)]
pub(super) struct Status {
    ready: bool,
    paths: Vec<PathBuf>,
    missing_paths: Vec<PathBuf>,
}

pub(super) fn status() -> io::Result<Status> {
    let mut paths = system_ancestors()?;
    let mut missing_paths = Vec::new();
    for path in &paths {
        if !acl::has_metadata_access(path, sid()?)? {
            missing_paths.push(path.clone());
        }
    }
    // Windows resets the null-device descriptor on restart. Recheck it on
    // every status request instead of persisting a one-time readiness flag.
    let null_path = PathBuf::from(null_device::DISPLAY_PATH);
    if !null_device::ready()? {
        missing_paths.push(null_path.clone());
    }
    paths.push(null_path);
    Ok(Status {
        ready: missing_paths.is_empty(),
        paths,
        missing_paths,
    })
}

pub(super) fn prepare() -> io::Result<Status> {
    if !elevated()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Windows script environment preparation requires explicit administrator approval",
        ));
    }
    let _lock = lease::AclLock::acquire()?;
    let state = status()?;
    for path in state.missing_paths {
        if path == std::path::Path::new(null_device::DISPLAY_PATH) {
            null_device::prepare()?;
            continue;
        }
        acl::change_without_inheritance(&path, sid()?, RIGHTS, GRANT_ACCESS).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "could not prepare directory metadata for {}: {error}",
                    path.display()
                ),
            )
        })?;
    }
    let state = status()?;
    if !state.ready {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Windows script environment access is still blocked by system policy",
        ));
    }
    Ok(state)
}

/// OS-reported locations only: no environment, launch paths, or arbitrary ACL
/// options supplied by the unelevated caller. This also works when UAC uses a
/// different administrator account. Nothing is granted recursively.
fn system_ancestors() -> io::Result<Vec<PathBuf>> {
    let mut roots = vec![0u16; 32768];
    let length = unsafe { GetLogicalDriveStringsW(roots.len() as u32, roots.as_mut_ptr()) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize >= roots.len() {
        return Err(io::Error::other("Windows drive list is too long"));
    }
    let mut paths = BTreeSet::new();
    for root in roots[..length as usize].split(|value| *value == 0) {
        if root.is_empty() {
            continue;
        }
        let path = PathBuf::from(OsString::from_wide(root));
        // DRIVE_FIXED = 3. Do not change network, removable, or optical media.
        if unsafe { GetDriveTypeW(wide(&path)?.as_ptr()) } == 3 {
            match lease::checked(&path) {
                Ok(root) => {
                    paths.insert(root);
                }
                // An unrelated locked BitLocker volume must not prevent
                // preparing available volumes. It is not reported as ready;
                // ordinary launches still fail closed if they use that path.
                Err(error)
                    if matches!(
                        error.raw_os_error().map(|code| code as u32),
                        Some(ERROR_NOT_READY | ERROR_ACCESS_DENIED)
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
    let mut profiles = vec![0u16; 32768];
    let mut length = profiles.len() as u32;
    check(unsafe { GetProfilesDirectoryW(profiles.as_mut_ptr(), &mut length) })?;
    let length = profiles
        .iter()
        .position(|value| *value == 0)
        .ok_or_else(|| io::Error::other("invalid Windows profile directory"))?;
    let profiles = PathBuf::from(OsString::from_wide(&profiles[..length]));
    let profiles = lease::checked(&profiles)?;
    // The profiles root can have a custom nested location. Its ancestors need
    // the same metadata-only permission; individual user homes do not.
    for ancestor in profiles.ancestors() {
        paths.insert(ancestor.to_path_buf());
    }
    if paths.is_empty() {
        return Err(io::Error::other(
            "no Windows script environment roots found",
        ));
    }
    Ok(paths.into_iter().collect())
}

fn elevated() -> io::Result<bool> {
    let mut token = ptr::null_mut();
    check(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) })?;
    let mut elevation = TOKEN_ELEVATION::default();
    let mut length = 0;
    let result = check(unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            ptr::from_mut(&mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
    });
    unsafe {
        CloseHandle(token);
    }
    result?;
    Ok(elevation.TokenIsElevated != 0)
}

fn check(value: i32) -> io::Result<()> {
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
