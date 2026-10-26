use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FileIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows {
        volume_serial_number: u32,
        file_index: u64,
    },
    #[cfg(not(any(unix, windows)))]
    Portable {
        created: Option<std::time::SystemTime>,
    },
}

#[cfg(unix)]
pub(crate) fn file_identity(_file: &File, metadata: &fs::Metadata) -> Result<FileIdentity, String> {
    use std::os::unix::fs::MetadataExt;
    Ok(FileIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
pub(crate) fn file_identity(file: &File, _metadata: &fs::Metadata) -> Result<FileIdentity, String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    // The handle and output pointer remain valid for the duration of the call.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(FileIdentity::Windows {
        volume_serial_number: information.dwVolumeSerialNumber,
        file_index: ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64,
    })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn file_identity(_file: &File, metadata: &fs::Metadata) -> Result<FileIdentity, String> {
    Ok(FileIdentity::Portable {
        created: metadata.created().ok(),
    })
}

pub(crate) fn file_is_excluded(
    file: &File,
    metadata: &fs::Metadata,
    excluded_paths: &[PathBuf],
) -> Result<bool, String> {
    if excluded_paths.is_empty() {
        return Ok(false);
    }
    let identity = file_identity(file, metadata)?;
    for path in excluded_paths {
        #[cfg(unix)]
        let excluded_identity = match fs::metadata(path) {
            Ok(metadata) => Some(file_identity(file, &metadata)?),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        #[cfg(not(unix))]
        let excluded_identity = match File::open(path) {
            Ok(excluded) => {
                let metadata = excluded.metadata().map_err(|error| error.to_string())?;
                Some(file_identity(&excluded, &metadata)?)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        if excluded_identity.as_ref() == Some(&identity) {
            return Ok(true);
        }
    }
    Ok(false)
}
