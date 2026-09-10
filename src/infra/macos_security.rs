use std::ffi::CString;
use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

// Darwin's extended security APIs are not exposed by the locked libc crate.
// Signatures/constants come from the macOS SDK's sys/acl.h and sys/fcntl.h.
const ACL_TYPE_EXTENDED: libc::c_int = 0x100;
const ACL_FIRST_ENTRY: libc::c_int = 0;
const ACL_FLAG_NO_INHERIT: libc::c_int = 1 << 17;
const FILESEC_MODE: libc::c_int = 4;
const FILESEC_ACL: libc::c_int = 5;

unsafe extern "C" {
    fn acl_init(count: libc::c_int) -> *mut libc::c_void;
    fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    fn acl_valid(acl: *mut libc::c_void) -> libc::c_int;
    fn acl_get_flagset_np(acl: *mut libc::c_void, flags: *mut *mut libc::c_void) -> libc::c_int;
    fn acl_add_flag_np(flags: *mut libc::c_void, flag: libc::c_int) -> libc::c_int;
    fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut libc::c_void;
    fn acl_get_entry(
        acl: *mut libc::c_void,
        entry_id: libc::c_int,
        entry: *mut *mut libc::c_void,
    ) -> libc::c_int;
    fn filesec_init() -> *mut libc::c_void;
    fn filesec_free(security: *mut libc::c_void);
    fn filesec_set_property(
        security: *mut libc::c_void,
        property: libc::c_int,
        value: *const libc::c_void,
    ) -> libc::c_int;
    fn openx_np(
        path: *const libc::c_char,
        flags: libc::c_int,
        security: *mut libc::c_void,
    ) -> libc::c_int;
}

struct Acl(*mut libc::c_void);

impl Acl {
    fn checked(pointer: *mut libc::c_void) -> io::Result<Self> {
        if pointer.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(pointer))
        }
    }
}

impl Drop for Acl {
    fn drop(&mut self) {
        unsafe { acl_free(self.0) };
    }
}

struct FileSecurity(*mut libc::c_void);

impl Drop for FileSecurity {
    fn drop(&mut self) {
        unsafe { filesec_free(self.0) };
    }
}

pub(crate) fn create_private_file_new(path: &Path) -> io::Result<File> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file path contains a NUL"))?;
    let empty = Acl::checked(unsafe { acl_init(0) })?;
    let mut flags = MaybeUninit::<*mut libc::c_void>::uninit();
    if unsafe { acl_get_flagset_np(empty.0, flags.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // The successful getter returns flags borrowed from the live ACL.
    let flags = unsafe { flags.assume_init() };
    if flags.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing ACL flags",
        ));
    }
    if unsafe { acl_add_flag_np(flags, ACL_FLAG_NO_INHERIT) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let security = unsafe { filesec_init() };
    if security.is_null() {
        return Err(io::Error::last_os_error());
    }
    let security = FileSecurity(security);
    let mode: libc::mode_t = 0o600;
    // FILESEC_ACL copies an acl_t supplied by address, not the ACL object itself.
    if unsafe { filesec_set_property(security.0, FILESEC_MODE, ptr::from_ref(&mode).cast()) } != 0
        || unsafe { filesec_set_property(security.0, FILESEC_ACL, ptr::from_ref(&empty.0).cast()) }
            != 0
    {
        return Err(io::Error::last_os_error());
    }
    // Inheritance must be disabled at creation. Clearing an ACL afterward
    // cannot revoke another user's descriptor opened while the file was empty.
    let fd = unsafe {
        openx_np(
            path.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            security.0,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

pub(crate) fn file_has_no_extended_acl(file: &File) -> io::Result<bool> {
    let pointer = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if pointer.is_null() {
        let error = io::Error::last_os_error();
        // Darwin reports an absent extended ACL as ENOENT on an opened file.
        return if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(true)
        } else {
            Err(error)
        };
    }
    let acl = Acl::checked(pointer)?;
    if unsafe { acl_valid(acl.0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut entry = ptr::null_mut();
    if unsafe { acl_get_entry(acl.0, ACL_FIRST_ENTRY, &mut entry) } == 0 {
        return Ok(false);
    }
    let error = io::Error::last_os_error();
    // Unlike Linux, Darwin returns 0 for an entry and EINVAL at exhaustion.
    // The ACL is valid and the first-entry selector is fixed, so this is empty.
    if error.raw_os_error() == Some(libc::EINVAL) {
        Ok(true)
    } else {
        Err(error)
    }
}
