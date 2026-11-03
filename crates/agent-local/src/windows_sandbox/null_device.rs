//! Fixed null-device access for ignored child-process stdio. Preparation is
//! called only by the explicit elevated metadata-preparation entry point, under
//! its ACL mutex. Never accept a device name, rights or SID from launch input.
use super::{acl, metadata, wide};
use std::{io, ptr, sync::OnceLock};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    Security::PSID,
    Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_TYPE_CHAR, GetFileType, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
    },
};

pub(super) const DISPLAY_PATH: &str = r"\\.\NUL";
// Address the kernel's null device directly, not a caller's DOS-device mapping.
const DEVICE_PATH: &str = r"\\?\GLOBALROOT\Device\Null";
const CAPABILITY: &str = "OpenHarness.Command.NullDevice.v1";
const RIGHTS: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE;

pub(super) fn sid() -> io::Result<PSID> {
    static SID: OnceLock<Result<Vec<u32>, i32>> = OnceLock::new();
    SID.get_or_init(|| {
        metadata::derive_sid(CAPABILITY).map_err(|error| error.raw_os_error().unwrap_or(13))
    })
    .as_ref()
    .map(|words| words.as_ptr().cast_mut().cast())
    .map_err(|code| io::Error::from_raw_os_error(*code))
}

pub(super) fn ready() -> io::Result<bool> {
    let handle = match open(READ_CONTROL | RIGHTS) {
        Ok(handle) => handle,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(false),
        Err(error) => return Err(error),
    };
    acl::has_capability_access(handle.0, sid()?, RIGHTS)
}

pub(super) fn prepare() -> io::Result<()> {
    let handle = open(READ_CONTROL | WRITE_DAC)?;
    // Read returns EOF and write discards bytes. This grants no filesystem,
    // other-device, execution, ownership, DACL-management or network access.
    // Do not change the device's integrity label to work around host policy.
    acl::grant_to_handle(handle.0, sid()?, RIGHTS)
}

fn open(rights: u32) -> io::Result<Handle> {
    let handle = unsafe {
        CreateFileW(
            wide(DEVICE_PATH)?.as_ptr(),
            rights,
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
    let handle = Handle(handle);
    if unsafe { GetFileType(handle.0) } != FILE_TYPE_CHAR {
        return Err(io::Error::other(
            "sandbox null target is not a character device",
        ));
    }
    Ok(handle)
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}
