use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Read an include through the handle whose type, aliases and boundary were checked.
/// The caller owns lexical admission and captures the canonical roots once.
pub(super) fn read_guarded_config_include(
    path: &Path,
    canonical_roots: &[PathBuf],
    max_bytes: u64,
) -> Result<String, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Includes may follow symlinks, but a FIFO replacement must not block.
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("failed to open OpenClaw include: {error}"))?;
    read_opened_include(file, canonical_roots, max_bytes)
}

fn read_opened_include(
    mut file: File,
    canonical_roots: &[PathBuf],
    max_bytes: u64,
) -> Result<String, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect OpenClaw include: {error}"))?;
    if !metadata.is_file() || hard_link_count(&file, &metadata)? != 1 {
        return Err(
            "OpenClaw include must be a regular file with exactly one hard link".to_string(),
        );
    }
    if metadata.len() > max_bytes {
        return Err(format!("OpenClaw include exceeds {max_bytes} bytes"));
    }
    let opened_path = opened_file_path(&file)?;
    if !canonical_roots
        .iter()
        .any(|root| contains_opened_path(root, &opened_path))
    {
        return Err("opened OpenClaw include resolves outside the allowed roots".to_string());
    }

    // Read only this checked descriptor. Reopening a pathname would discard the
    // containment proof, and metadata alone cannot bound a concurrently growing file.
    let limit = max_bytes
        .checked_add(1)
        .ok_or_else(|| "OpenClaw include byte limit is too large".to_string())?;
    let mut raw = String::new();
    (&mut file)
        .take(limit)
        .read_to_string(&mut raw)
        .map_err(|error| format!("failed to read OpenClaw include: {error}"))?;
    if raw.len() as u64 > max_bytes {
        return Err(format!("OpenClaw include exceeds {max_bytes} bytes"));
    }
    Ok(raw)
}

#[cfg(unix)]
fn hard_link_count(_file: &File, metadata: &std::fs::Metadata) -> Result<u64, String> {
    use std::os::unix::fs::MetadataExt;
    Ok(metadata.nlink())
}

#[cfg(windows)]
fn hard_link_count(file: &File, _metadata: &std::fs::Metadata) -> Result<u64, String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    // The borrowed file owns the handle; the output remains live during the call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0 {
        return Err(format!(
            "failed to inspect OpenClaw include links: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(u64::from(information.nNumberOfLinks))
}

#[cfg(not(any(unix, windows)))]
fn hard_link_count(_file: &File, _metadata: &std::fs::Metadata) -> Result<u64, String> {
    Err("guarded OpenClaw include reads are unavailable on this platform".to_string())
}

#[cfg(target_os = "linux")]
fn opened_file_path(file: &File) -> Result<PathBuf, String> {
    use std::os::fd::AsRawFd;

    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|error| format!("failed to resolve opened OpenClaw include: {error}"))
}

#[cfg(target_os = "macos")]
fn opened_file_path(file: &File) -> Result<PathBuf, String> {
    use std::ffi::{CStr, OsStr};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;

    let mut buffer = [0_u8; libc::PATH_MAX as usize];
    // F_GETPATH writes a NUL-terminated path into a PATH_MAX-sized buffer.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) } < 0 {
        return Err(format!(
            "failed to resolve opened OpenClaw include: {}",
            std::io::Error::last_os_error()
        ));
    }
    let path = unsafe { CStr::from_ptr(buffer.as_ptr().cast()) };
    Ok(PathBuf::from(OsStr::from_bytes(path.to_bytes())))
}

#[cfg(windows)]
fn opened_file_path(file: &File) -> Result<PathBuf, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    let mut buffer = vec![0_u16; 260];
    loop {
        // Use the same normalized DOS path form as std::fs::canonicalize.
        let count = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle().cast(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        if count == 0 {
            return Err(format!(
                "failed to resolve opened OpenClaw include: {}",
                std::io::Error::last_os_error()
            ));
        }
        if count < buffer.len() {
            buffer.truncate(count);
            return Ok(PathBuf::from(OsString::from_wide(&buffer)));
        }
        if count >= 32_768 {
            return Err("opened OpenClaw include path is too long".to_string());
        }
        buffer.resize(count + 1, 0);
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn opened_file_path(_file: &File) -> Result<PathBuf, String> {
    Err("guarded OpenClaw include reads are unavailable on this platform".to_string())
}

#[cfg(not(windows))]
fn contains_opened_path(root: &Path, path: &Path) -> bool {
    root.is_absolute() && path.is_absolute() && path.starts_with(root)
}

#[cfg(windows)]
fn contains_opened_path(root: &Path, path: &Path) -> bool {
    use std::path::{Component, Prefix};

    if !root.is_absolute() || !path.is_absolute() {
        return false;
    }
    let mut root_parts = root.components();
    let mut path_parts = path.components();
    let same_prefix = match (root_parts.next(), path_parts.next()) {
        (Some(Component::Prefix(root)), Some(Component::Prefix(path))) => {
            match (root.kind(), path.kind()) {
                (
                    Prefix::Disk(a) | Prefix::VerbatimDisk(a),
                    Prefix::Disk(b) | Prefix::VerbatimDisk(b),
                ) => a.eq_ignore_ascii_case(&b),
                (
                    Prefix::UNC(a, b) | Prefix::VerbatimUNC(a, b),
                    Prefix::UNC(c, d) | Prefix::VerbatimUNC(c, d),
                ) => a == c && b == d,
                (a, b) => a == b,
            }
        }
        _ => false,
    };
    // Both paths are canonical handle paths, which preserve filesystem casing.
    // Do not fold directory names: Windows supports case-sensitive directories.
    same_prefix && root_parts.all(|part| path_parts.next() == Some(part))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests {
    use super::*;
    use std::fs;

    const SENTINEL: &[u8] = b"private-include-sentinel\xff";

    #[test]
    fn guarded_include_reads_regular_file_and_rejects_outside_handle() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("allowed");
        fs::create_dir(&root).unwrap();
        let roots = vec![fs::canonicalize(&root).unwrap()];
        let inside = root.join("config.json5");
        fs::write(&inside, "{ gateway: {} }").unwrap();
        assert_eq!(
            read_guarded_config_include(&inside, &roots, 1024).unwrap(),
            "{ gateway: {} }"
        );
        let outside = fixture.path().join("outside.json5");
        fs::write(&outside, SENTINEL).unwrap();
        let error = read_opened_include(File::open(outside).unwrap(), &roots, 1024).unwrap_err();
        assert!(error.contains("outside the allowed roots"));
        assert!(!error.contains("private-include-sentinel"));
    }

    #[test]
    fn guarded_include_rejects_hardlinks_oversize_and_nonregular_files() {
        let fixture = tempfile::tempdir().unwrap();
        let roots = vec![fs::canonicalize(fixture.path()).unwrap()];
        let path = fixture.path().join("config.json5");
        fs::write(&path, SENTINEL).unwrap();
        let error = read_guarded_config_include(&path, &roots, 4).unwrap_err();
        assert!(error.contains("exceeds 4 bytes"));
        assert!(!error.contains("private-include-sentinel"));

        fs::hard_link(&path, fixture.path().join("alias.json5")).unwrap();
        let error = read_guarded_config_include(&path, &roots, 1024).unwrap_err();
        assert!(error.contains("exactly one hard link"));
        assert!(!error.contains("private-include-sentinel"));
        assert!(read_guarded_config_include(fixture.path(), &roots, 1024).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn guarded_include_accepts_allowed_symlinks_and_rejects_swapped_target() {
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("allowed");
        let shared = fixture.path().join("shared");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&shared).unwrap();
        let roots = vec![
            fs::canonicalize(&root).unwrap(),
            fs::canonicalize(&shared).unwrap(),
        ];
        let target = shared.join("config.json5");
        fs::write(&target, "{}").unwrap();
        let link = root.join("include.json5");
        symlink(&target, &link).unwrap();
        assert_eq!(
            read_guarded_config_include(&link, &roots, 1024).unwrap(),
            "{}"
        );

        let outside = fixture.path().join("outside.json5");
        fs::write(&outside, SENTINEL).unwrap();
        fs::remove_file(&link).unwrap();
        symlink(outside, &link).unwrap();
        let error = read_guarded_config_include(&link, &roots, 1024).unwrap_err();
        assert!(error.contains("outside the allowed roots"));
        assert!(!error.contains("private-include-sentinel"));
    }

    #[cfg(unix)]
    #[test]
    fn guarded_include_rejects_fifo_without_waiting_for_a_writer() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let fixture = tempfile::tempdir().unwrap();
        let roots = vec![fs::canonicalize(fixture.path()).unwrap()];
        let path = fixture.path().join("include.fifo");
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let error = read_guarded_config_include(&path, &roots, 1024).unwrap_err();
        assert!(error.contains("regular file"));
    }

    #[cfg(windows)]
    #[test]
    fn opened_path_containment_handles_native_prefixes_without_folding_directories() {
        assert!(contains_opened_path(
            Path::new(r"C:\Config"),
            Path::new(r"\\?\C:\Config\a.json5")
        ));
        assert!(contains_opened_path(
            Path::new(r"\\server\share\Config"),
            Path::new(r"\\?\UNC\server\share\Config\a.json5")
        ));
        assert!(!contains_opened_path(
            Path::new(r"C:\Config"),
            Path::new(r"\\?\C:\Config-other\a.json5")
        ));
        assert!(!contains_opened_path(
            Path::new(r"C:\Config"),
            Path::new(r"\\?\C:\config\a.json5")
        ));
    }
}
