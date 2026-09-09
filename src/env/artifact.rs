use std::io::Write;
use std::path::Path;

use super::EnvironmentService;

impl EnvironmentService<'_> {
    /// Export untrusted file bytes, not an attestation. Discard output on error.
    pub fn export_artifact(
        &self,
        name: &str,
        path: &Path,
        max_bytes: u64,
        output: &mut impl Write,
    ) -> Result<(), String> {
        let meta = self.get(name)?;
        export_rooted(Path::new(&meta.root), path, max_bytes, output)
    }
}

#[cfg(not(unix))]
fn export_rooted(
    _root: &Path,
    _path: &Path,
    _max_bytes: u64,
    _output: &mut impl Write,
) -> Result<(), String> {
    Err("artifact export requires Unix descriptor-relative file access".to_string())
}

#[cfg(unix)]
fn export_rooted(
    root: &Path,
    path: &Path,
    max_bytes: u64,
    output: &mut impl Write,
) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::Component;

    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => Err("artifact must be a relative path without traversal".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some((leaf, parents)) = components.split_last() else {
        return Err("artifact must be a nonempty relative path".to_string());
    };
    if !root.is_absolute() {
        return Err("registered environment home must be absolute".to_string());
    }

    // Pin the selected home and each child directory. Pathname prechecks alone
    // would allow a candidate to replace a checked parent with a symlink.
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)
        .map_err(|error| format!("failed to open artifact home: {error}"))?;
    for parent in parents {
        directory = open_relative(&directory, parent, true)?;
    }
    let mut file = open_relative(&directory, leaf, false)?;
    let before = file
        .metadata()
        .map_err(|error| format!("failed to inspect artifact: {error}"))?;
    if !before.is_file() || before.nlink() != 1 {
        return Err("artifact must be a regular file with exactly one hard link".to_string());
    }
    if before.len() > max_bytes {
        return Err("artifact exceeds --max-bytes".to_string());
    }

    // Emit at most the original size, never a growing file or an allocation
    // based on candidate metadata. The caller must discard partial output on
    // any failure, and independently bound its transport and destination.
    let mut remaining = before.len();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = file
            .read(&mut buffer[..limit])
            .map_err(|error| format!("failed to read artifact: {error}"))?;
        if count == 0 {
            return Err("artifact changed during export".to_string());
        }
        output
            .write_all(&buffer[..count])
            .map_err(|error| format!("failed to write artifact: {error}"))?;
        remaining -= count as u64;
    }
    let mut extra = [0_u8; 1];
    if file
        .read(&mut extra)
        .map_err(|error| format!("failed to read artifact: {error}"))?
        != 0
    {
        return Err("artifact changed during export".to_string());
    }
    let after = file
        .metadata()
        .map_err(|error| format!("failed to inspect artifact: {error}"))?;
    let current = open_relative(&directory, leaf, false)
        .and_then(|file| file.metadata().map_err(|error| error.to_string()))
        .map_err(|_| "artifact changed during export".to_string())?;
    if !same_file(&before, &after) || !same_file(&before, &current) {
        return Err("artifact changed during export".to_string());
    }
    output
        .flush()
        .map_err(|error| format!("failed to write artifact: {error}"))?;
    Ok(())
}

#[cfg(unix)]
fn open_relative(
    directory: &std::fs::File,
    name: &std::ffi::OsStr,
    is_directory: bool,
) -> Result<std::fs::File, String> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    let name = CString::new(name.as_bytes())
        .map_err(|_| "artifact path contains a null byte".to_string())?;
    let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    if is_directory {
        flags |= libc::O_DIRECTORY;
    }
    // The live directory descriptor and one validated component prevent path
    // traversal. O_NONBLOCK prevents a FIFO replacement from waiting for a writer.
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(format!(
            "failed to open artifact: {}",
            std::io::Error::last_os_error()
        ));
    }
    // openat returned a new owned descriptor, transferred exactly once.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn same_file(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    after.is_file()
        && before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.nlink() == after.nlink()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}
