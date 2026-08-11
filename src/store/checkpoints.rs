use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(not(target_os = "macos"))]
use filetime::{FileTime, set_file_times, set_symlink_file_times};
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};
#[cfg(not(target_os = "macos"))]
use std::io::Write;

use super::common::{ensure_dir, path_exists};
use super::layout::display_path;

pub(crate) const STORAGE_APFS_CLONE: &str = "apfs-clone-v1";
pub(crate) const STORAGE_FULL_COPY: &str = "full-copy-v1";
pub(crate) const STORAGE_TAR_ARCHIVE: &str = "tar-archive-v1";

pub(crate) fn default_snapshot_storage_kind() -> String {
    STORAGE_TAR_ARCHIVE.to_string()
}

pub(crate) fn create_tree_checkpoint(source: &Path, destination: &Path) -> Result<String, String> {
    if path_exists(destination) {
        return Err(format!(
            "checkpoint destination already exists: {}",
            display_path(destination)
        ));
    }
    if let Some(parent) = destination.parent() {
        ensure_dir(parent)?;
    }

    #[cfg(target_os = "macos")]
    {
        match copyfile_tree(source, destination, true) {
            Ok(()) => {
                verify_tree_checkpoint(source, destination)?;
                verify_sqlite_databases(destination)?;
                sync_tree_root(destination)?;
                return Ok(STORAGE_APFS_CLONE.to_string());
            }
            Err(clone_error) => {
                remove_tree_if_present(destination)?;
                copyfile_tree(source, destination, false).map_err(|copy_error| {
                    format!(
                        "APFS clone was unavailable ({clone_error}); full checkpoint copy also failed: {copy_error}"
                    )
                })?;
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    copy_tree_preserving_metadata(source, destination)?;

    if let Err(error) = (|| {
        verify_tree_checkpoint(source, destination)?;
        verify_sqlite_databases(destination)?;
        sync_tree_root(destination)
    })() {
        let _ = remove_tree_if_present(destination);
        return Err(error);
    }
    Ok(STORAGE_FULL_COPY.to_string())
}

pub(crate) fn copy_tree_checkpoint(source: &Path, destination: &Path) -> Result<(), String> {
    let _ = create_tree_checkpoint(source, destination)?;
    Ok(())
}

pub(crate) fn verify_tree_checkpoint(source: &Path, destination: &Path) -> Result<(), String> {
    let source_entries = inventory_tree(source)?;
    let destination_entries = inventory_tree(destination)?;
    if source_entries != destination_entries {
        return Err(format!(
            "checkpoint verification failed: {} does not exactly match {}",
            display_path(destination),
            display_path(source)
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InventoryEntry {
    kind: &'static str,
    len: Option<u64>,
    mode: u32,
    link_target: Option<PathBuf>,
    sha256: Option<[u8; 32]>,
}

fn inventory_tree(root: &Path) -> Result<BTreeMap<PathBuf, InventoryEntry>, String> {
    let mut out = BTreeMap::new();
    inventory_path(root, root, &mut out)?;
    Ok(out)
}

fn inventory_path(
    root: &Path,
    path: &Path,
    out: &mut BTreeMap<PathBuf, InventoryEntry>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect {}: {error}", display_path(path)))?;
    let relative = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    let file_type = metadata.file_type();
    let (kind, link_target, sha256) = if file_type.is_symlink() {
        (
            "symlink",
            Some(fs::read_link(path).map_err(|error| error.to_string())?),
            None,
        )
    } else if file_type.is_dir() {
        ("directory", None, None)
    } else if file_type.is_file() {
        ("file", None, Some(hash_file(path)?))
    } else {
        ("special", None, None)
    };
    out.insert(
        relative,
        InventoryEntry {
            kind,
            len: file_type.is_file().then_some(metadata.len()),
            mode: metadata_mode(&metadata),
            link_target,
            sha256,
        },
    );
    if file_type.is_dir() {
        let mut entries = fs::read_dir(path)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            inventory_path(root, &entry.path(), out)?;
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<[u8; 32], String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn verify_sqlite_databases(root: &Path) -> Result<(), String> {
    for path in regular_files(root)? {
        let mut file = fs::File::open(&path).map_err(|error| error.to_string())?;
        let mut magic = [0_u8; 16];
        if file.read_exact(&mut magic).is_err() || &magic != b"SQLite format 3\0" {
            continue;
        }
        drop(file);
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|error| {
            format!(
                "failed to open checkpoint SQLite database {}: {error}",
                display_path(&path)
            )
        })?;
        let result: String = connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|error| {
                format!(
                    "failed to verify checkpoint SQLite database {}: {error}",
                    display_path(&path)
                )
            })?;
        if result != "ok" {
            return Err(format!(
                "checkpoint SQLite integrity check failed for {}: {result}",
                display_path(&path)
            ));
        }
    }
    Ok(())
}

fn regular_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    collect_regular_files(root, &mut out)?;
    Ok(out)
}

fn collect_regular_files(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
            collect_regular_files(&entry.map_err(|error| error.to_string())?.path(), out)?;
        }
    }
    Ok(())
}

fn sync_tree_root(root: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let _ = root;
        return Ok(());
    }
    #[cfg(not(windows))]
    fs::File::open(root)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("failed to sync checkpoint {}: {error}", display_path(root)))
}

pub(crate) fn remove_tree_if_present(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            make_tree_removable(path)?;
            fs::remove_dir_all(path).map_err(|error| error.to_string())
        }
        Ok(_) => fs::remove_file(path).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn make_tree_removable(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.is_dir() {
            let mut permissions = metadata.permissions();
            permissions.set_mode(permissions.mode() | 0o700);
            fs::set_permissions(path, permissions).map_err(|error| error.to_string())?;
        }
    }
    #[cfg(windows)]
    {
        let mut permissions = metadata.permissions();
        if permissions.readonly() {
            permissions.set_readonly(false);
            fs::set_permissions(path, permissions).map_err(|error| error.to_string())?;
        }
    }

    if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
            make_tree_removable(&entry.map_err(|error| error.to_string())?.path())?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn copyfile_tree(source: &Path, destination: &Path, clone: bool) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    const COPYFILE_ALL: u32 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3);
    const COPYFILE_RECURSIVE: u32 = 1 << 15;
    const COPYFILE_CLONE: u32 = 1 << 24;

    unsafe extern "C" {
        fn copyfile(
            from: *const libc::c_char,
            to: *const libc::c_char,
            state: *mut libc::c_void,
            flags: u32,
        ) -> libc::c_int;
    }

    if clone {
        verify_clone_capability(source, destination)?;
    }
    let source_c =
        CString::new(source.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    let destination_c =
        CString::new(destination.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    let mut flags = COPYFILE_ALL | COPYFILE_RECURSIVE;
    if clone {
        flags |= COPYFILE_CLONE;
    }
    let result = unsafe {
        copyfile(
            source_c.as_ptr(),
            destination_c.as_ptr(),
            std::ptr::null_mut(),
            flags,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

#[cfg(target_os = "macos")]
fn verify_clone_capability(source: &Path, destination: &Path) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    unsafe extern "C" {
        fn clonefile(
            source: *const libc::c_char,
            destination: *const libc::c_char,
            flags: libc::c_int,
        ) -> libc::c_int;
    }

    let destination_parent = destination
        .parent()
        .ok_or_else(|| "checkpoint destination has no parent".to_string())?;
    ensure_dir(destination_parent)?;
    if fs::metadata(source)
        .map_err(|error| error.to_string())?
        .dev()
        != fs::metadata(destination_parent)
            .map_err(|error| error.to_string())?
            .dev()
    {
        return Err("source and checkpoint destination are on different filesystems".to_string());
    }

    let nonce = format!(
        ".ocm-clone-probe-{}-{}",
        std::process::id(),
        time::OffsetDateTime::now_utc().unix_timestamp_nanos()
    );
    let probe_source = destination_parent.join(format!("{nonce}.source"));
    let probe_clone = destination_parent.join(format!("{nonce}.clone"));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe_source)
        .map_err(|error| error.to_string())?;
    let source_c =
        CString::new(probe_source.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    let clone_c =
        CString::new(probe_clone.as_os_str().as_bytes()).map_err(|error| error.to_string())?;
    let result = unsafe { clonefile(source_c.as_ptr(), clone_c.as_ptr(), 0) };
    let clone_error = (result != 0).then(std::io::Error::last_os_error);
    let _ = fs::remove_file(&probe_clone);
    let _ = fs::remove_file(&probe_source);
    match clone_error {
        Some(error) => Err(format!("filesystem clone probe failed: {error}")),
        None => Ok(()),
    }
}

#[cfg(not(target_os = "macos"))]
fn copy_tree_preserving_metadata(source: &Path, destination: &Path) -> Result<(), String> {
    copy_path_preserving_metadata(source, destination)
}

#[cfg(not(target_os = "macos"))]
fn copy_path_preserving_metadata(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source).map_err(|error| error.to_string())?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        let target = fs::read_link(source).map_err(|error| error.to_string())?;
        if let Some(parent) = destination.parent() {
            ensure_dir(parent)?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, destination).map_err(|error| error.to_string())?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, destination)
            .or_else(|_| std::os::windows::fs::symlink_dir(target, destination))
            .map_err(|error| error.to_string())?;
        preserve_metadata(source, destination, &metadata, true)?;
        return Ok(());
    }
    if file_type.is_dir() {
        ensure_dir(destination)?;
        let entries = fs::read_dir(source)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        for entry in entries {
            copy_path_preserving_metadata(&entry.path(), &destination.join(entry.file_name()))?;
        }
        preserve_metadata(source, destination, &metadata, false)?;
        return Ok(());
    }
    if file_type.is_file() {
        if let Some(parent) = destination.parent() {
            ensure_dir(parent)?;
        }
        let mut input = fs::File::open(source).map_err(|error| error.to_string())?;
        let mut output = fs::File::create(destination).map_err(|error| error.to_string())?;
        std::io::copy(&mut input, &mut output).map_err(|error| error.to_string())?;
        output.flush().map_err(|error| error.to_string())?;
        output.sync_all().map_err(|error| error.to_string())?;
        preserve_metadata(source, destination, &metadata, false)?;
        return Ok(());
    }
    Err(format!(
        "unsupported special file in checkpoint: {}",
        display_path(source)
    ))
}

#[cfg(not(target_os = "macos"))]
fn preserve_metadata(
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    symlink: bool,
) -> Result<(), String> {
    if !symlink {
        fs::set_permissions(destination, metadata.permissions())
            .map_err(|error| error.to_string())?;
    }
    let accessed = FileTime::from_last_access_time(metadata);
    let modified = FileTime::from_last_modification_time(metadata);
    if symlink {
        set_symlink_file_times(destination, accessed, modified)
            .map_err(|error| error.to_string())?;
    } else {
        set_file_times(destination, accessed, modified).map_err(|error| error.to_string())?;
    }
    #[cfg(unix)]
    for name in xattr::list(source).map_err(|error| error.to_string())? {
        if let Some(value) = xattr::get(source, &name).map_err(|error| error.to_string())? {
            xattr::set(destination, &name, &value).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn metadata_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    metadata.mode()
}

#[cfg(not(unix))]
fn metadata_mode(metadata: &fs::Metadata) -> u32 {
    u32::from(metadata.permissions().readonly())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::inventory_tree;

    #[test]
    fn checkpoint_inventory_ignores_directory_allocation_lengths() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested/value.txt"), "value\n").unwrap();

        let inventory = inventory_tree(root.path()).unwrap();
        assert_eq!(inventory[Path::new("")].len, None);
        assert_eq!(inventory[Path::new("nested")].len, None);
        assert_eq!(inventory[Path::new("nested/value.txt")].len, Some(6));
    }
}
