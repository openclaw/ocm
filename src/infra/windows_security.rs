use std::fs::{File, OpenOptions};
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, AddAccessAllowedAce,
    DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetKernelObjectSecurity, GetLengthSid,
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor, IsValidSid,
    OWNER_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
    SetSecurityDescriptorControl, SetSecurityDescriptorDacl, SetSecurityDescriptorOwner,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, READ_CONTROL,
};
use windows_sys::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const MAX_SECURITY_BYTES: usize = 64 * 1024;

pub(crate) fn with_user_only_security_attributes<T>(
    user: &UserSid,
    action: impl FnOnce(&SECURITY_ATTRIBUTES) -> io::Result<T>,
) -> io::Result<T> {
    let sid_bytes = unsafe { GetLengthSid(user.as_sid()) } as usize;
    let acl_bytes =
        size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_bytes;
    let mut acl_storage = vec![0_usize; acl_bytes.div_ceil(size_of::<usize>())];
    let acl = acl_storage.as_mut_ptr().cast::<ACL>();
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let descriptor_ptr = ptr::from_mut(&mut descriptor).cast();
    if unsafe { InitializeAcl(acl, acl_bytes as u32, ACL_REVISION) } == 0
        || unsafe { AddAccessAllowedAce(acl, ACL_REVISION, FILE_ALL_ACCESS, user.as_sid()) } == 0
        || unsafe { InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION) }
            == 0
        || unsafe { SetSecurityDescriptorOwner(descriptor_ptr, user.as_sid(), 0) } == 0
        || unsafe { SetSecurityDescriptorDacl(descriptor_ptr, 1, acl, 0) } == 0
        || unsafe {
            SetSecurityDescriptorControl(descriptor_ptr, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
        } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // Protected DACLs exclude inheritable permissions on custom parent roots.
    // Keep the SID, ACL, and descriptor alive until the creation call returns.
    action(&SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor_ptr,
        bInheritHandle: 0,
    })
}

pub(crate) fn create_private_file_new(path: &Path) -> io::Result<File> {
    let mut path = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid private file path",
        ));
    }
    path.push(0);
    let user = UserSid::current()?;
    with_user_only_security_attributes(&user, |attributes| {
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        owned_handle(handle).map(File::from)
    })
}

pub(crate) fn owned_handle(raw: HANDLE) -> io::Result<OwnedHandle> {
    if raw.is_null() || raw == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
}

pub(crate) struct UserSid {
    // TOKEN_USER contains a pointer into this allocation. Keep its alignment
    // and allocation stable until all SID comparisons have completed.
    storage: Vec<usize>,
}

impl UserSid {
    pub(crate) fn current() -> io::Result<Self> {
        Self::for_process(unsafe { GetCurrentProcess() })
    }

    pub(crate) fn for_process(process: HANDLE) -> io::Result<Self> {
        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = owned_handle(token)?;
        let mut needed = 0;
        let result = unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        if result != 0
            || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "could not size the process user identity",
            ));
        }
        if !(size_of::<TOKEN_USER>()..=MAX_SECURITY_BYTES).contains(&(needed as usize)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid process user identity size",
            ));
        }
        let mut storage = vec![0_usize; (needed as usize).div_ceil(size_of::<usize>())];
        let mut returned = 0;
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                storage.as_mut_ptr().cast(),
                needed,
                &mut returned,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if returned < size_of::<TOKEN_USER>() as u32 || returned > needed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid process user identity data",
            ));
        }
        let user = Self { storage };
        if user.as_sid().is_null() || unsafe { IsValidSid(user.as_sid()) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid process user SID",
            ));
        }
        Ok(user)
    }

    pub(crate) fn as_sid(&self) -> PSID {
        unsafe { &*self.storage.as_ptr().cast::<TOKEN_USER>() }
            .User
            .Sid
    }
}

pub(crate) fn file_has_user_only_access(path: &Path) -> io::Result<bool> {
    let file = match OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    // Inspect the opened entry itself. A link target's ACL does not identify
    // the authored link as one of our private regular config files.
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(false);
    }
    has_user_only_access(&file)
}

pub(crate) fn has_user_only_access(file: &File) -> io::Result<bool> {
    let mut needed = 0;
    let sized = unsafe {
        GetKernelObjectSecurity(
            file.as_raw_handle(),
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
            ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if sized != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid file security size",
        ));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
        return Err(error);
    }
    if needed == 0 || needed as usize > MAX_SECURITY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid file security size",
        ));
    }
    let mut storage = vec![0_usize; (needed as usize).div_ceil(size_of::<usize>())];
    let descriptor = storage.as_mut_ptr().cast();
    let mut returned = 0;
    if unsafe {
        GetKernelObjectSecurity(
            file.as_raw_handle(),
            DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
            descriptor,
            needed,
            &mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if returned == 0 || returned > needed {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid file security data",
        ));
    }
    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut owner = ptr::null_mut();
    let mut owner_defaulted = 0;
    if unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut owner_defaulted) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if owner.is_null() {
        return Ok(false);
    }
    if unsafe { IsValidSid(owner) } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid file owner identity",
        ));
    }
    let user = UserSid::current()?;
    if unsafe { EqualSid(owner, user.as_sid()) } == 0 {
        return Ok(false);
    }
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl = MaybeUninit::<*mut ACL>::uninit();
    if unsafe {
        GetSecurityDescriptorDacl(descriptor, &mut present, dacl.as_mut_ptr(), &mut defaulted)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_PROTECTED == 0 || present == 0 || defaulted != 0 {
        return Ok(false);
    }
    // SAFETY: success with a present DACL initializes this output, including
    // a possible null DACL. A nonnull pointer borrows the still-owned `storage`.
    let dacl = unsafe { dacl.assume_init() };
    if dacl.is_null() {
        return Ok(false);
    }
    if unsafe { (*dacl).AceCount } != 1 {
        return Ok(false);
    }
    let mut ace = MaybeUninit::<*mut std::ffi::c_void>::uninit();
    if unsafe { GetAce(dacl, 0, ace.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetAce initializes the entry pointer on success. The entry stays
    // inside the descriptor buffer, which is neither resized nor freed here.
    let ace = unsafe { ace.assume_init() };
    if ace.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "missing file access entry",
        ));
    }
    let header = unsafe { &*ace.cast::<ACE_HEADER>() };
    if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE
        || header.AceFlags != 0
        || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
    {
        return Ok(false);
    }
    let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
    if ace.Mask != FILE_ALL_ACCESS {
        return Ok(false);
    }
    let sid = ptr::from_ref(&ace.SidStart).cast_mut().cast();
    if unsafe { IsValidSid(sid) } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid file access identity",
        ));
    }
    Ok(unsafe { EqualSid(sid, user.as_sid()) } != 0)
}

#[cfg(test)]
pub(crate) fn assert_private_file_security(file: &File) {
    assert!(has_user_only_access(file).unwrap());
}
