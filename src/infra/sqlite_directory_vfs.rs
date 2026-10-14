//! Linux SQLite VFS for a database in an already-open parent directory.
//!
//! Compile this module only on Linux. Supply a path of the form
//! `/proc/self/fd/<parent-fd>/<leaf>` to `Connection::open_with_flags_and_vfs`
//! with `pinned_directory_vfs()?`. The caller must retain the parent `File`
//! until the connection closes, including all backup operations.
//!
//! SQLite's normal Unix canonicalization resolves the procfs descriptor link
//! back to a pathname. Preserving it instead keeps the database and SQLite's
//! derived WAL, journal and SHM names relative to that same open directory.
//! All file operations and locking still use SQLite's built-in Unix VFS.

use std::ffi::{CStr, c_char, c_int};
use std::ptr;
use std::sync::OnceLock;

use rusqlite::ffi;

const VFS_NAME: &CStr = c"ocm-pinned-directory";
static REGISTERED: OnceLock<Result<(), c_int>> = OnceLock::new();

pub(super) fn pinned_directory_vfs() -> Result<&'static CStr, String> {
    match REGISTERED.get_or_init(register_vfs) {
        Ok(()) => Ok(VFS_NAME),
        Err(code) => Err(format!(
            "failed to register SQLite directory VFS: {}",
            ffi::Error::new(*code)
        )),
    }
}

fn register_vfs() -> Result<(), c_int> {
    // SQLite serializes initialization and registry changes. OnceLock also
    // ensures this module allocates and registers only one VFS instance.
    let result = unsafe { ffi::sqlite3_initialize() };
    if result != ffi::SQLITE_OK {
        return Err(result);
    }
    let native = unsafe { ffi::sqlite3_vfs_find(c"unix".as_ptr()) };
    if native.is_null() {
        return Err(ffi::SQLITE_NOTFOUND);
    }

    // Copy immutable fields individually: another registry operation may
    // mutate native.pNext, so do not copy or borrow the complete native VFS.
    // The native pAppData and callbacks belong to the built-in Unix VFS and
    // have process lifetime. No callback or native syscall table is changed.
    let vfs = unsafe {
        ffi::sqlite3_vfs {
            iVersion: (*native).iVersion,
            szOsFile: (*native).szOsFile,
            mxPathname: (*native).mxPathname,
            pNext: ptr::null_mut(),
            zName: VFS_NAME.as_ptr(),
            pAppData: (*native).pAppData,
            xOpen: (*native).xOpen,
            xDelete: (*native).xDelete,
            xAccess: (*native).xAccess,
            xFullPathname: Some(pinned_full_pathname),
            xDlOpen: (*native).xDlOpen,
            xDlError: (*native).xDlError,
            xDlSym: (*native).xDlSym,
            xDlClose: (*native).xDlClose,
            xRandomness: (*native).xRandomness,
            xSleep: (*native).xSleep,
            xCurrentTime: (*native).xCurrentTime,
            xGetLastError: (*native).xGetLastError,
            xCurrentTimeInt64: (*native).xCurrentTimeInt64,
            xSetSystemCall: (*native).xSetSystemCall,
            xGetSystemCall: (*native).xGetSystemCall,
            xNextSystemCall: (*native).xNextSystemCall,
        }
    };
    let vfs = Box::into_raw(Box::new(vfs));
    let result = unsafe { ffi::sqlite3_vfs_register(vfs, 0) };
    if result != ffi::SQLITE_OK {
        // Registration failed, so SQLite does not retain this allocation.
        unsafe { drop(Box::from_raw(vfs)) };
        return Err(result);
    }

    // SQLite retains this pointer. Leave the single allocation registered
    // for process lifetime; never replace the default VFS or unregister it
    // while a connection may still refer to it.
    Ok(())
}

unsafe extern "C" fn pinned_full_pathname(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    capacity: c_int,
    output: *mut c_char,
) -> c_int {
    if name.is_null() || output.is_null() || capacity <= 0 {
        return ffi::SQLITE_CANTOPEN;
    }
    // SQLite supplies a NUL-terminated input and an output buffer of capacity
    // bytes. Validate bytes directly so non-UTF-8 database names work too.
    let name = unsafe { CStr::from_ptr(name) };
    let bytes = name.to_bytes();
    let Some(relative) = bytes.strip_prefix(b"/proc/self/fd/") else {
        return ffi::SQLITE_CANTOPEN;
    };
    let Some(separator) = relative.iter().position(|byte| *byte == b'/') else {
        return ffi::SQLITE_CANTOPEN;
    };
    let descriptor = &relative[..separator];
    let leaf = &relative[separator + 1..];
    if descriptor.is_empty()
        || !descriptor.iter().all(u8::is_ascii_digit)
        || leaf.is_empty()
        || leaf == b"."
        || leaf == b".."
        || leaf.contains(&b'/')
        || bytes.len() >= capacity as usize
    {
        return ffi::SQLITE_CANTOPEN;
    }
    // Keep the procfs link intact. Copy the terminator too; ptr::copy permits
    // overlapping buffers and this callback neither allocates nor panics.
    unsafe { ptr::copy(name.as_ptr(), output, bytes.len() + 1) };
    ffi::SQLITE_OK
}
