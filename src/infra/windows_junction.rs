//! Native directory junctions for installed runtime bindings and their recovery copies.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_NOT_A_REPARSE_POINT, GENERIC_WRITE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{FSCTL_GET_REPARSE_POINT, FSCTL_SET_REPARSE_POINT};
use windows_sys::Win32::System::SystemServices::IO_REPARSE_TAG_MOUNT_POINT;

// The Windows reparse buffer limit includes its eight-byte common header.
const MAX_REPARSE_BYTES: usize = 16 * 1024;
const REPARSE_HEADER_BYTES: usize = 8;
const MOUNT_POINT_HEADER_BYTES: usize = 16;

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn open_link(path: &Path, write: bool) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(if write { GENERIC_WRITE } else { 0 })
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

fn short(data: &[u8], offset: usize) -> usize {
    u16::from_le_bytes([data[offset], data[offset + 1]]) as usize
}

fn name_bytes(data: &[u8], offset: usize, length: usize) -> io::Result<&[u8]> {
    if offset % 2 != 0 || length % 2 != 0 {
        return Err(invalid_data("unaligned junction name"));
    }
    let start = MOUNT_POINT_HEADER_BYTES
        .checked_add(offset)
        .ok_or_else(|| invalid_data("junction name offset overflow"))?;
    let end = start
        .checked_add(length)
        .filter(|end| *end <= data.len())
        .ok_or_else(|| invalid_data("junction name exceeds its reparse buffer"))?;
    Ok(&data[start..end])
}

fn substitute_name(data: &[u8]) -> io::Result<&[u8]> {
    if data.len() < MOUNT_POINT_HEADER_BYTES {
        return Err(invalid_data("truncated junction header"));
    }
    let substitute = name_bytes(data, short(data, 8), short(data, 10))?;
    name_bytes(data, short(data, 12), short(data, 14))?;
    if substitute.is_empty() || substitute.chunks_exact(2).any(|unit| unit == [0, 0]) {
        return Err(invalid_data("invalid junction substitute name"));
    }
    Ok(substitute)
}

fn read_junction(file: &File) -> io::Result<Option<Vec<u8>>> {
    let mut data = vec![0_u8; MAX_REPARSE_BYTES];
    let mut returned = 0;
    // The handle opens the reparse entry, and the output buffer stays live.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_GET_REPARSE_POINT,
            ptr::null(),
            0,
            data.as_mut_ptr().cast(),
            data.len() as u32,
            &mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_NOT_A_REPARSE_POINT as i32) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    if !(REPARSE_HEADER_BYTES..=data.len()).contains(&(returned as usize)) {
        return Err(invalid_data("invalid reparse response length"));
    }
    let length = REPARSE_HEADER_BYTES + short(&data, 4);
    if length > returned as usize {
        return Err(invalid_data("truncated reparse payload"));
    }
    data.truncate(length);
    let tag = u32::from_le_bytes(data[..4].try_into().unwrap());
    if tag != IO_REPARSE_TAG_MOUNT_POINT {
        return Ok(None);
    }
    substitute_name(&data)?;
    Ok(Some(data))
}

/// Normalize absolute DOS/verbatim spellings without resolving the target.
/// No case folding or filesystem-alias matching is performed.
fn target_name(target: &Path) -> io::Result<Vec<u16>> {
    if !target.is_absolute() || target.as_os_str().encode_wide().any(|unit| unit == 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "junction target must be an absolute path without NUL",
        ));
    }
    let absolute = std::path::absolute(target)?;
    let units = absolute.as_os_str().encode_wide().collect::<Vec<_>>();
    let mut name = r"\??\".encode_utf16().collect::<Vec<_>>();
    if units.starts_with(&[92, 92, 63, 92]) || units.starts_with(&[92, 92, 46, 92]) {
        name.extend_from_slice(&units[4..]);
    } else if units.starts_with(&[92, 92]) {
        name.extend("UNC\\".encode_utf16());
        name.extend_from_slice(&units[2..]);
    } else {
        name.extend_from_slice(&units);
    }
    Ok(name)
}

fn junction_data(target: &Path) -> io::Result<Vec<u8>> {
    let name = target_name(target)?;
    let name_bytes = name
        .len()
        .checked_mul(2)
        .filter(|length| *length <= MAX_REPARSE_BYTES - MOUNT_POINT_HEADER_BYTES - 4)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "junction target is too long")
        })?;
    // Substitute name, its terminator, and an empty terminated print name.
    let mut data = vec![0_u8; MOUNT_POINT_HEADER_BYTES + name_bytes + 4];
    data[..4].copy_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    let payload_length = (data.len() - REPARSE_HEADER_BYTES) as u16;
    data[4..6].copy_from_slice(&payload_length.to_le_bytes());
    data[10..12].copy_from_slice(&(name_bytes as u16).to_le_bytes());
    data[12..14].copy_from_slice(&((name_bytes + 2) as u16).to_le_bytes());
    for (unit, destination) in name
        .iter()
        .zip(data[MOUNT_POINT_HEADER_BYTES..].chunks_exact_mut(2))
    {
        destination.copy_from_slice(&unit.to_le_bytes());
    }
    Ok(data)
}

fn set_junction(file: &File, data: &[u8]) -> io::Result<()> {
    let mut returned = 0;
    // Only locally constructed or validated mount-point buffers reach this call.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_REPARSE_POINT,
            data.as_ptr().cast(),
            data.len() as u32,
            ptr::null_mut(),
            0,
            &mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn create_with_data(link: &Path, data: &[u8]) -> io::Result<()> {
    fs::create_dir(link)?;
    let result = (|| {
        let file = open_link(link, true)?;
        if !file.metadata()?.file_type().is_dir() {
            return Err(invalid_data("new junction directory was replaced"));
        }
        set_junction(&file, data)
    })();
    if let Err(error) = result {
        // Remove only the directory this call created, never a target or subtree.
        if let Err(cleanup) = fs::remove_dir(link) {
            return Err(io::Error::new(
                error.kind(),
                format!("{error}; failed to remove new junction entry: {cleanup}"),
            ));
        }
        return Err(error);
    }
    Ok(())
}

/// Create a fresh junction. Its parent must exist; its target need not exist.
pub(crate) fn create(target: &Path, link: &Path) -> io::Result<()> {
    create_with_data(link, &junction_data(target)?)
}

/// Update only a junction still naming the expected absolute target spelling.
/// Both target paths may be dangling, including projections into the final runtime.
pub(crate) fn retarget(link: &Path, expected_target: &Path, new_target: &Path) -> io::Result<()> {
    let expected = junction_data(expected_target)?;
    let replacement = junction_data(new_target)?;
    let file = open_link(link, true)?;
    let current = read_junction(&file)?
        .ok_or_else(|| invalid_data("retarget entry is not a directory junction"))?;
    if substitute_name(&current)? != substitute_name(&expected)? {
        return Err(invalid_data("junction target changed before retargeting"));
    }
    set_junction(&file, &replacement)
}

/// Preserve a junction's raw payload without resolving it or changing its target.
/// Other reparse tags and ordinary entries remain with the caller's existing copier.
pub(crate) fn copy_if_junction(source: &Path, dest: &Path) -> io::Result<bool> {
    let file = open_link(source, false)?;
    let Some(data) = read_junction(&file)? else {
        return Ok(false);
    };
    create_with_data(dest, &data)?;
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{
        ERROR_NO_TOKEN, ERROR_NOT_ALL_ASSIGNED, ERROR_PRIVILEGE_NOT_HELD, GetLastError, LUID,
        SetLastError,
    };
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, ImpersonateSelf, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
        RevertToSelf, SE_PRIVILEGE_REMOVED, SecurityImpersonation, TOKEN_ADJUST_PRIVILEGES,
        TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

    struct Impersonation;

    impl Drop for Impersonation {
        fn drop(&mut self) {
            assert_ne!(
                unsafe { RevertToSelf() },
                0,
                "revert isolated test impersonation: {}",
                io::Error::last_os_error()
            );
        }
    }

    /// Restrict only this test thread; parallel tests keep their process token.
    /// CI must also disable Developer Mode for the actual refusal probe to pass.
    pub(crate) fn without_symlink_privilege<T>(run: impl FnOnce() -> T) -> T {
        let mut existing = ptr::null_mut();
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut existing) } != 0 {
            drop(unsafe { OwnedHandle::from_raw_handle(existing) });
            panic!("refusing to replace an existing test impersonation token");
        }
        assert_eq!(unsafe { GetLastError() }, ERROR_NO_TOKEN);
        assert_ne!(unsafe { ImpersonateSelf(SecurityImpersonation) }, 0);
        let _impersonation = Impersonation;
        let mut raw_token = ptr::null_mut();
        assert_ne!(
            unsafe {
                OpenThreadToken(
                    GetCurrentThread(),
                    TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES,
                    1,
                    &mut raw_token,
                )
            },
            0
        );
        let token = unsafe { OwnedHandle::from_raw_handle(raw_token) };
        let name = "SeCreateSymbolicLinkPrivilege\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        let mut luid = LUID::default();
        assert_ne!(
            unsafe { LookupPrivilegeValueW(ptr::null(), name.as_ptr(), &mut luid) },
            0
        );
        let removal = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_REMOVED,
            }],
        };
        unsafe { SetLastError(0) };
        let removed = unsafe {
            AdjustTokenPrivileges(
                token.as_raw_handle(),
                0,
                &removal,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        let error = unsafe { GetLastError() };
        assert_ne!(removed, 0, "remove test thread privilege: {error}");
        assert!(error == 0 || error == ERROR_NOT_ALL_ASSIGNED);
        let probe = tempfile::tempdir().unwrap();
        fs::create_dir(probe.path().join("source")).unwrap();
        let result = std::os::windows::fs::symlink_dir("source", probe.path().join("link"));
        if result.is_ok() {
            fs::remove_dir(probe.path().join("link")).unwrap();
            panic!("std::symlink_dir succeeded: the no-privilege prerequisite is not established");
        }
        assert_eq!(
            result.unwrap_err().raw_os_error(),
            Some(ERROR_PRIVILEGE_NOT_HELD as i32)
        );
        run()
    }

    #[test]
    fn junction_copy_and_retarget_preserve_dangling_identity() {
        without_symlink_privilege(|| {
            let root = tempfile::tempdir().unwrap();
            let target = root.path().join("target");
            fs::create_dir(&target).unwrap();
            let canonical_target = fs::canonicalize(&target).unwrap();
            let first = root.path().join("first");
            let second = root.path().join("second");
            fs::create_dir(&first).unwrap();
            fs::create_dir(&second).unwrap();
            let link = first.join("alias");
            create(&canonical_target, &link).unwrap();
            assert_eq!(fs::canonicalize(&link).unwrap(), canonical_target);
            assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
            let original = read_junction(&open_link(&link, false).unwrap())
                .unwrap()
                .unwrap();
            assert!(create(&canonical_target, &link).is_err());
            fs::remove_dir(&target).unwrap();
            let digest = super::super::tree_digest::tree_sha256(&first).unwrap();
            let copy = second.join("alias");
            assert!(copy_if_junction(&link, &copy).unwrap());
            assert_eq!(
                read_junction(&open_link(&copy, false).unwrap()).unwrap(),
                Some(original.clone())
            );
            assert_eq!(
                super::super::tree_digest::tree_sha256(&second).unwrap(),
                digest
            );
            let final_target = root.path().join("future-runtime");
            assert!(retarget(&copy, &final_target, &target).is_err());
            assert_eq!(
                read_junction(&open_link(&copy, false).unwrap()).unwrap(),
                Some(original)
            );
            // The captured canonical spelling remains valid for comparison even
            // though neither the old nor the projected target exists now.
            retarget(&copy, &canonical_target, &final_target).unwrap();
            assert_ne!(
                super::super::tree_digest::tree_sha256(&second).unwrap(),
                digest
            );
            fs::create_dir(&final_target).unwrap();
            fs::write(final_target.join("sentinel"), "keep").unwrap();
            assert_eq!(fs::read_to_string(copy.join("sentinel")).unwrap(), "keep");
            fs::remove_dir(&copy).unwrap();
            assert_eq!(
                fs::read_to_string(final_target.join("sentinel")).unwrap(),
                "keep"
            );
            let untouched = root.path().join("not-created");
            assert!(!copy_if_junction(&final_target, &untouched).unwrap());
            assert!(!untouched.exists());
        });
    }
}
