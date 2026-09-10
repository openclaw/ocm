use std::fs;
use std::path::{Path, PathBuf};

use crate::env::EnvMeta;

use super::display_path;

// An absent destination cannot contain an existing source. Resolve its nearest
// existing ancestor instead of guessing how the filesystem compares new names.
fn destination_ancestor(path: &Path) -> Result<(PathBuf, bool), String> {
    let mut existing = path;
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => {
                let resolved = fs::canonicalize(existing).map_err(|error| {
                    format!(
                        "failed to resolve environment root {}: {error}",
                        display_path(existing)
                    )
                })?;
                return Ok((resolved, existing != path));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                existing = existing.parent().ok_or_else(|| {
                    format!("failed to resolve environment root {}", display_path(path))
                })?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[cfg(unix)]
fn path_identity(path: &Path) -> Result<(u64, u64), String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn path_identity(path: &Path) -> Result<(u64, u64), String> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        GetFileInformationByHandle,
    };

    let file = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| error.to_string())?;
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    // The handle and output pointer remain valid for the duration of the call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok((
        u64::from(information.dwVolumeSerialNumber),
        ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64,
    ))
}

#[cfg(not(any(unix, windows)))]
fn path_identity(_path: &Path) -> Result<(u64, u64), String> {
    Err("cannot establish dev source identity on this platform".to_string())
}

fn contains_existing(root: &Path, path: &Path) -> Result<bool, String> {
    if path.starts_with(root) {
        return Ok(true);
    }
    let root_identity = path_identity(root)?;
    for ancestor in path.ancestors() {
        if path_identity(ancestor)? == root_identity {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn ensure_root_outside_dev_sources(
    name: &str,
    root: &Path,
    envs: &[EnvMeta],
) -> Result<(), String> {
    if !envs.iter().any(|meta| meta.dev.is_some()) {
        return Ok(());
    }
    let (target, missing) = destination_ancestor(root)?;
    // Clone/import rollback removes a final symlink itself. Its parent can
    // belong to a source even when the destination contents resolve elsewhere.
    let entry_parent = if fs::symlink_metadata(root).is_ok_and(|meta| meta.is_symlink()) {
        let parent = root.parent().ok_or("environment root has no parent")?;
        Some(fs::canonicalize(parent).map_err(|error| error.to_string())?)
    } else {
        None
    };
    for meta in envs {
        let Some(dev) = &meta.dev else {
            continue;
        };
        let source = match fs::canonicalize(&dev.worktree_root) {
            Ok(source) => source,
            // Missing source reservations belong to source registration. Its
            // existing cleanup verifies Git identity before removing a path
            // that has since been recreated by another environment.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "failed to resolve dev source {} for env {}: {error}",
                    dev.worktree_root, meta.name
                ));
            }
        };
        let removes_source_entry = entry_parent
            .as_ref()
            .map(|parent| contains_existing(&source, parent))
            .transpose()?
            .unwrap_or(false);
        if contains_existing(&source, &target)?
            || (!missing && contains_existing(&target, &source)?)
            || removes_source_entry
        {
            return Err(format!(
                "environment {name} root {} overlaps dev source {} for env {}; choose a separate environment root",
                display_path(root),
                display_path(&source),
                meta.name
            ));
        }
    }
    Ok(())
}
