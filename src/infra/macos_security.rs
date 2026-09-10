use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::ptr;

// Darwin's extended ACL functions are not exposed by the locked libc crate.
// These signatures and constants come from the macOS SDK's sys/acl.h.
const ACL_TYPE_EXTENDED: libc::c_int = 0x100;
const ACL_FIRST_ENTRY: libc::c_int = 0;

unsafe extern "C" {
    fn acl_init(count: libc::c_int) -> *mut libc::c_void;
    fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
    fn acl_valid(acl: *mut libc::c_void) -> libc::c_int;
    fn acl_set_fd_np(fd: libc::c_int, acl: *mut libc::c_void, kind: libc::c_int) -> libc::c_int;
    fn acl_get_fd_np(fd: libc::c_int, kind: libc::c_int) -> *mut libc::c_void;
    fn acl_get_entry(
        acl: *mut libc::c_void,
        entry_id: libc::c_int,
        entry: *mut *mut libc::c_void,
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

// Only the fresh staging descriptor is modified. Its owner and POSIX mode are
// established by the caller; inherited ACLs must be removed before any data write.
pub(crate) fn set_private_file_access(file: &File) -> io::Result<()> {
    let empty = Acl::checked(unsafe { acl_init(0) })?;
    if unsafe { acl_set_fd_np(file.as_raw_fd(), empty.0, ACL_TYPE_EXTENDED) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if !file_has_no_extended_acl(file)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the private config still has an extended ACL",
        ));
    }
    Ok(())
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
