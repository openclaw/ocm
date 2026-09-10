use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::env::{EnvDevMeta, EnvMeta};

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
pub(crate) fn path_identity(path: &Path) -> Result<(u64, u64), String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
pub(crate) fn path_identity(path: &Path) -> Result<(u64, u64), String> {
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
pub(crate) fn path_identity(_path: &Path) -> Result<(u64, u64), String> {
    Err("cannot establish dev source identity on this platform".to_string())
}

pub(crate) fn contains_existing(root: &Path, path: &Path) -> Result<bool, String> {
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

fn existing_cleanup_path(path: &Path) -> Result<Option<PathBuf>, String> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "failed to resolve cleanup path {}: {error}",
            display_path(path)
        )),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SourceFootprint {
    pub(crate) entries: BTreeSet<PathBuf>,
    pub(crate) content_roots: BTreeSet<PathBuf>,
}

fn record_path_entries(
    path: &Path,
    entries: &mut BTreeSet<PathBuf>,
    links: &mut BTreeSet<PathBuf>,
) -> Result<PathBuf, String> {
    let resolved = fs::canonicalize(path).map_err(|error| {
        format!(
            "cannot resolve source identity path {}: {error}",
            display_path(path)
        )
    })?;
    entries.insert(resolved.clone());
    for ancestor in path.ancestors() {
        if !fs::symlink_metadata(ancestor)
            .map_err(|error| error.to_string())?
            .is_symlink()
        {
            continue;
        }
        let parent = ancestor.parent().ok_or("source symlink has no parent")?;
        let entry = fs::canonicalize(parent)
            .map_err(|error| error.to_string())?
            .join(ancestor.file_name().ok_or("source symlink has no name")?);
        entries.insert(entry.clone());
        if links.insert(entry) {
            let target = fs::read_link(ancestor).map_err(|error| error.to_string())?;
            let target = if target.is_absolute() {
                target
            } else {
                parent.join(target)
            };
            record_path_entries(&target, entries, links)?;
        }
    }
    Ok(resolved)
}

pub(crate) fn existing_source_path_entries(path: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut entries = BTreeSet::new();
    record_path_entries(path, &mut entries, &mut BTreeSet::new())?;
    Ok(entries)
}

fn append_git_footprint(
    footprint: &mut SourceFootprint,
    git: crate::openclaw_repo::GitIdentityPaths,
    include_worktree_entry: bool,
    links: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    for path in git.entries {
        record_path_entries(&path, &mut footprint.entries, links)?;
    }
    if include_worktree_entry && let Some(path) = git.worktree_entry {
        record_path_entries(&path, &mut footprint.entries, links)?;
    }
    for path in [git.private_dir, git.common_dir] {
        let resolved = record_path_entries(&path, &mut footprint.entries, links)?;
        footprint.content_roots.insert(resolved);
    }
    Ok(())
}

// This primitive describes existing paths, not source eligibility. Registration
// may retain absent/non-Git tuples; callers own that policy and path reservations.
pub(crate) fn inspect_source_footprint(root: &Path) -> Result<SourceFootprint, String> {
    let mut footprint = SourceFootprint::default();
    if existing_cleanup_path(root)?.is_none() {
        return Ok(footprint);
    }
    footprint.entries = existing_source_path_entries(root)?;
    let mut links = BTreeSet::new();
    if let Some(git) = crate::openclaw_repo::git_identity_paths(root)? {
        let registrations = match git.worktree_entry.as_ref().and_then(|path| path.parent()) {
            Some(worktree) => {
                crate::openclaw_repo::git_registration_entries(&git.common_dir, worktree)?
            }
            None => Vec::new(),
        };
        append_git_footprint(&mut footprint, git, true, &mut links)?;
        for path in registrations {
            append_git_footprint(
                &mut footprint,
                crate::openclaw_repo::git_registration_paths(&path)?,
                true,
                &mut links,
            )?;
        }
    }
    Ok(footprint)
}

pub(crate) fn inspect_dev_source_metadata(dev: &EnvDevMeta) -> Result<SourceFootprint, String> {
    let mut footprint = inspect_source_footprint(Path::new(&dev.repo_root))?;
    let mut links = BTreeSet::new();
    for path in crate::openclaw_repo::worktree_registration_entries(
        Path::new(&dev.repo_root),
        Path::new(&dev.worktree_root),
    )? {
        // Missing working files do not discard the slot's recoverable history.
        // Preserve existing metadata, without reserving the absent .git entry.
        append_git_footprint(
            &mut footprint,
            crate::openclaw_repo::git_registration_paths(&path)?,
            false,
            &mut links,
        )?;
    }
    Ok(footprint)
}

pub(crate) fn inspect_dev_source_footprint(dev: &EnvDevMeta) -> Result<SourceFootprint, String> {
    let mut footprint = inspect_dev_source_metadata(dev)?;
    let worktree = inspect_source_footprint(Path::new(&dev.worktree_root))?;
    footprint.entries.extend(worktree.entries);
    footprint.content_roots.extend(worktree.content_roots);
    if let Some(source) = existing_cleanup_path(Path::new(&dev.worktree_root))? {
        footprint.content_roots.insert(source);
    }
    Ok(footprint)
}

pub(crate) fn registered_dev_source_replaced(
    meta: &EnvMeta,
    envs: &[EnvMeta],
) -> Result<bool, String> {
    let Some(dev) = &meta.dev else {
        return Ok(false);
    };
    if !fs::symlink_metadata(Path::new(&dev.worktree_root).join(".git"))
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(false);
    }
    let Some(source) = existing_cleanup_path(Path::new(&dev.worktree_root))? else {
        return Ok(false);
    };
    // Creation can reuse a missing worktree path for another env's state,
    // regardless of that env's binding or the current cleanup target.
    let source_identity = path_identity(&source)?;
    for owner in envs.iter().filter(|owner| owner.name != meta.name) {
        if let Some(root) = existing_cleanup_path(Path::new(&owner.root))?
            && path_identity(&root)? == source_identity
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn registered_dev_source_footprint(
    meta: &EnvMeta,
    envs: &[EnvMeta],
) -> Result<Option<SourceFootprint>, String> {
    let Some(dev) = &meta.dev else {
        return Ok(None);
    };
    if registered_dev_source_replaced(meta, envs)? {
        // Keep the old source's recoverable Git data, not the new state.
        let footprint = inspect_dev_source_metadata(dev)?;
        return Ok((!footprint.entries.is_empty()).then_some(footprint));
    }
    if existing_cleanup_path(Path::new(&dev.worktree_root))?.is_some()
        && !crate::openclaw_repo::has_expected_worktree_identity(
            Path::new(&dev.repo_root),
            Path::new(&dev.worktree_root),
        )
    {
        return Err(format!(
            "cannot verify Git identity for registered dev source {}",
            dev.worktree_root
        ));
    }
    let footprint = inspect_dev_source_footprint(dev)?;
    Ok((!footprint.entries.is_empty()).then_some(footprint))
}

pub(crate) fn ensure_environment_removal_preserves_dev_sources(
    target: &EnvMeta,
    envs: &[EnvMeta],
) -> Result<(), String> {
    let mut footprints = Vec::new();
    for meta in envs.iter().filter(|meta| meta.name != target.name) {
        if let Some(footprint) = registered_dev_source_footprint(meta, envs)? {
            footprints.push((meta.name.as_str(), footprint));
        }
    }
    if footprints.is_empty() {
        return Ok(());
    }
    let root_path = Path::new(&target.root);
    let root = existing_cleanup_path(root_path)?;
    let worktree = target
        .dev
        .as_ref()
        .map(|dev| existing_cleanup_path(Path::new(&dev.worktree_root)))
        .transpose()?
        .flatten();
    // Removing a final symlink also changes its containing source, even when
    // the link resolves outside that source (or its destination is missing).
    let entry_parent = if fs::symlink_metadata(root_path).is_ok_and(|meta| meta.is_symlink()) {
        root_path
            .parent()
            .map(existing_cleanup_path)
            .transpose()?
            .flatten()
    } else {
        None
    };
    let mut registrations = Vec::new();
    let mut registration_links = Vec::new();
    if let Some(dev) = &target.dev {
        for path in crate::openclaw_repo::worktree_registration_entries(
            Path::new(&dev.repo_root),
            Path::new(&dev.worktree_root),
        )? {
            if fs::symlink_metadata(&path)
                .map_err(|error| error.to_string())?
                .is_symlink()
            {
                // Git unlinks the administrative slot itself, not its referent.
                let parent =
                    fs::canonicalize(path.parent().unwrap()).map_err(|error| error.to_string())?;
                registration_links.push(parent.join(path.file_name().unwrap()));
            } else {
                registrations.push(fs::canonicalize(path).map_err(|error| error.to_string())?);
            }
        }
    }
    if root.is_none()
        && worktree.is_none()
        && entry_parent.is_none()
        && registrations.is_empty()
        && registration_links.is_empty()
    {
        return Ok(());
    }
    let preserve = |path: &Path, owner: &str, contents: bool| -> Result<(), String> {
        let mut affected = false;
        // An owned child worktree may be removed from inside another source.
        // Refuse only when that worktree itself contains a protected path.
        for (root, symmetric) in [(root.as_ref(), true), (worktree.as_ref(), false)] {
            if let Some(root) = root {
                affected |= contains_existing(root, path)?
                    || (symmetric && contents && contains_existing(path, root)?);
            }
        }
        if let Some(parent) = &entry_parent
            && contents
        {
            affected |= contains_existing(path, parent)?;
        }
        for root in &registrations {
            affected |= contains_existing(root, path)?;
        }
        for entry in &registration_links {
            affected |= path_identity(entry)? == path_identity(path)?;
        }
        if affected {
            return Err(format!(
                "cleanup for env {} would affect registered dev source for env {owner} at {}; remove the dependent dev env first",
                target.name,
                display_path(path)
            ));
        }
        Ok(())
    };
    for (owner, footprint) in footprints {
        for path in &footprint.entries {
            preserve(path, owner, false)?;
        }
        for path in &footprint.content_roots {
            preserve(path, owner, true)?;
        }
    }
    Ok(())
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
