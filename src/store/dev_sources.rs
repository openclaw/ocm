use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use caseless::Caseless;
use icu_properties::{CodePointSetData, props::DefaultIgnorableCodePoint};

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

// Inputs are locations projected by registration_path, or a restore's actual
// root entry. Preserve a final symlink's identity instead of following it here.
pub(crate) fn projected_path_contains(root: &Path, path: &Path) -> Result<bool, String> {
    Ok(projected_path_relative(root, path)?.is_some())
}

pub(crate) fn projected_path_relative(root: &Path, path: &Path) -> Result<Option<PathBuf>, String> {
    fn split(path: &Path) -> Result<(&Path, &Path), String> {
        let mut existing = path;
        loop {
            match fs::symlink_metadata(existing) {
                Ok(_) => {
                    let suffix = path
                        .strip_prefix(existing)
                        .map_err(|error| error.to_string())?;
                    if suffix
                        .components()
                        .any(|part| !matches!(part, std::path::Component::Normal(_)))
                    {
                        return Err(format!(
                            "cannot resolve missing source location {}",
                            display_path(path)
                        ));
                    }
                    return Ok((existing, suffix));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    existing = existing
                        .parent()
                        .ok_or("source location has no existing ancestor")?;
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }
    let (root, root_suffix) = split(root)?;
    let (path, path_suffix) = split(path)?;
    if root_suffix.as_os_str().is_empty() {
        if let Ok(relative) = path.strip_prefix(root) {
            return Ok(Some(relative.join(path_suffix)));
        }
        let identity = path_identity(root)?;
        for ancestor in path.ancestors() {
            if path_identity(ancestor)? == identity {
                return Ok(Some(path.strip_prefix(ancestor).unwrap().join(path_suffix)));
            }
        }
        return Ok(None);
    }
    if path_suffix.as_os_str().is_empty() || path_identity(root)? != path_identity(path)? {
        return Ok(None);
    }
    let mut actual = path_suffix.components();
    for expected in root_suffix.components() {
        let Some(component) = actual.next() else {
            return Ok(None);
        };
        if component == expected {
            continue;
        }
        let component = component.as_os_str().to_str();
        let expected = expected.as_os_str().to_str();
        let (Some(component), Some(expected)) = (component, expected) else {
            return Err("cannot distinguish differently encoded missing source names; restore the source before retrying".to_string());
        };
        #[cfg(windows)]
        let (component, expected) = (
            component.trim_end_matches(&['.', ' '][..]),
            expected.trim_end_matches(&['.', ' '][..]),
        );
        // Missing names lack filesystem identity. Reserve possible Unicode
        // aliases without changing comparisons between existing entries.
        if component.eq_ignore_ascii_case(expected)
            || ((!component.is_ascii() || !expected.is_ascii())
                && missing_name_characters(component)
                    .compatibility_caseless_match(missing_name_characters(expected)))
        {
            continue;
        }
        #[cfg(windows)]
        if windows_case_alias(component, expected)? {
            continue;
        }
        #[cfg(windows)]
        if (possible_short_name(component) || possible_short_name(expected))
            && !(plain_short_name(component) && plain_short_name(expected))
        {
            return Err("cannot distinguish missing Windows short-name aliases; restore the source before retrying".to_string());
        }
        return Ok(None);
    }
    Ok(Some(actual.collect()))
}

fn missing_name_characters(name: &str) -> impl Iterator<Item = char> + '_ {
    let ignorables = CodePointSetData::new::<DefaultIgnorableCodePoint>();
    name.chars()
        // HFS+ ignores formatting characters in filename comparisons. This is
        // only a conservative reservation key; never rewrite a stored path.
        .filter(move |character| !ignorables.contains(*character))
        .map(|character| match character {
            // HFS+ folds Georgian Asomtavruli to Mkhedruli, whereas modern
            // Unicode folds it to Nuskhuri. Preserve both equivalences before
            // applying standard Unicode compatibility caseless matching.
            // https://developer.apple.com/library/archive/technotes/tn/tn1150.html
            '\u{10a0}'..='\u{10c5}' => char::from_u32(character as u32 + 0x30).unwrap(),
            '\u{2d00}'..='\u{2d25}' => char::from_u32(character as u32 - 0x1c30).unwrap(),
            _ => character,
        })
}

#[cfg(windows)]
fn windows_case_alias(left: &str, right: &str) -> Result<bool, String> {
    use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};

    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();
    let left_len = i32::try_from(left.len()).map_err(|error| error.to_string())?;
    let right_len = i32::try_from(right.len()).map_err(|error| error.to_string())?;
    // Both buffers remain live for their explicit UTF-16 lengths. Use the OS
    // uppercase table as well as Unicode folding.
    let comparison =
        unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) };
    if comparison == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(comparison == CSTR_EQUAL)
}

#[cfg(windows)]
fn possible_short_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let base = parts.next().unwrap_or_default();
    let extension = parts.next().unwrap_or_default();
    !base.is_empty()
        && base.encode_utf16().count() <= 8
        && extension.encode_utf16().count() <= 3
        && parts.next().is_none()
}

#[cfg(windows)]
fn plain_short_name(name: &str) -> bool {
    possible_short_name(name)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-~.".contains(&byte))
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
    // In-memory locations only. Existing-path identity checks must not stat an
    // absent borrowed checkout, and legacy Owned paths remain unreserved.
    pub(crate) reserved_roots: BTreeSet<PathBuf>,
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

fn reserve_path(path: &Path, footprint: &mut SourceFootprint) -> Result<(), String> {
    let (reserved, entries) = super::dev_registration::registration_path(path)?;
    footprint.entries.extend(entries);
    footprint.reserved_roots.insert(reserved);
    Ok(())
}

fn record_git_path(
    path: &Path,
    footprint: &mut SourceFootprint,
    reserve_missing: bool,
    links: &mut BTreeSet<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    if reserve_missing && existing_cleanup_path(path)?.is_none() {
        reserve_path(path, footprint)?;
        return Ok(None);
    }
    record_path_entries(path, &mut footprint.entries, links).map(Some)
}

fn append_git_footprint(
    footprint: &mut SourceFootprint,
    git: crate::openclaw_repo::GitIdentityPaths,
    include_worktree_entry: bool,
    reserve_missing: bool,
    links: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    for path in git.entries {
        record_git_path(&path, footprint, reserve_missing, links)?;
    }
    if include_worktree_entry && let Some(path) = git.worktree_entry {
        record_git_path(&path, footprint, reserve_missing, links)?;
    }
    for path in [git.private_dir, git.common_dir] {
        if let Some(resolved) = record_git_path(&path, footprint, reserve_missing, links)? {
            footprint.content_roots.insert(resolved);
        }
    }
    Ok(())
}

// This primitive describes existing paths, not source eligibility. Registration
// may retain absent/non-Git tuples; callers own that policy and path reservations.
pub(crate) fn inspect_source_footprint(root: &Path) -> Result<SourceFootprint, String> {
    inspect_source_footprint_paths(root, false)
}

fn inspect_source_footprint_paths(
    root: &Path,
    reserve_missing: bool,
) -> Result<SourceFootprint, String> {
    let mut footprint = SourceFootprint::default();
    if existing_cleanup_path(root)?.is_none() {
        return Ok(footprint);
    }
    footprint.entries = existing_source_path_entries(root)?;
    // Borrowed cleanup can preserve a missing Git target without accepting it
    // for execution. Read only the known .git entry and its surviving aliases.
    let git_entry = root.join(".git");
    if reserve_missing && existing_cleanup_path(&git_entry)?.is_none() {
        reserve_path(&git_entry, &mut footprint)?;
        return Ok(footprint);
    }
    let mut links = BTreeSet::new();
    if let Some(git) = crate::openclaw_repo::git_identity_paths(root)? {
        let registrations = if reserve_missing && existing_cleanup_path(&git.common_dir)?.is_none()
        {
            Vec::new()
        } else {
            match git.worktree_entry.as_ref().and_then(|path| path.parent()) {
                Some(worktree) => {
                    crate::openclaw_repo::git_registration_entries(&git.common_dir, worktree)?
                }
                None => Vec::new(),
            }
        };
        append_git_footprint(&mut footprint, git, true, reserve_missing, &mut links)?;
        for path in registrations {
            append_git_footprint(
                &mut footprint,
                crate::openclaw_repo::git_registration_paths(&path)?,
                true,
                reserve_missing,
                &mut links,
            )?;
        }
    }
    Ok(footprint)
}

pub(crate) fn inspect_dev_source_metadata(dev: &EnvDevMeta) -> Result<SourceFootprint, String> {
    let Some((repo_root, worktree_root)) = dev.owned_worktree() else {
        return inspect_source_footprint_paths(Path::new(dev.source_root()), true);
    };
    let mut footprint = inspect_source_footprint(Path::new(repo_root))?;
    let mut links = BTreeSet::new();
    for path in crate::openclaw_repo::worktree_registration_entries(
        Path::new(repo_root),
        Path::new(worktree_root),
    )? {
        // Missing working files do not discard the slot's recoverable history.
        // Preserve existing metadata, without reserving the absent .git entry.
        append_git_footprint(
            &mut footprint,
            crate::openclaw_repo::git_registration_paths(&path)?,
            false,
            false,
            &mut links,
        )?;
    }
    Ok(footprint)
}

pub(crate) fn inspect_dev_source_footprint(dev: &EnvDevMeta) -> Result<SourceFootprint, String> {
    if let Some(source) = dev.borrowed_source_root() {
        return borrowed_source_footprint(Path::new(source));
    }
    let mut footprint = inspect_dev_source_metadata(dev)?;
    let worktree = inspect_source_footprint(Path::new(dev.source_root()))?;
    footprint.entries.extend(worktree.entries);
    footprint.content_roots.extend(worktree.content_roots);
    if let Some(source) = existing_cleanup_path(Path::new(dev.source_root()))? {
        footprint.content_roots.insert(source);
    }
    Ok(footprint)
}

pub(crate) fn registered_dev_source_replaced(
    meta: &EnvMeta,
    envs: &[EnvMeta],
) -> Result<bool, String> {
    let Some((_, worktree)) = meta.dev.as_ref().and_then(|dev| dev.owned_worktree()) else {
        return Ok(false);
    };
    if !fs::symlink_metadata(Path::new(worktree).join(".git"))
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(false);
    }
    let Some(source) = existing_cleanup_path(Path::new(worktree))? else {
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
    if let Some((repo_root, worktree_root)) = dev.owned_worktree()
        && existing_cleanup_path(Path::new(worktree_root))?.is_some()
        && !crate::openclaw_repo::has_expected_worktree_identity(
            Path::new(repo_root),
            Path::new(worktree_root),
        )
    {
        return Err(format!(
            "cannot verify Git identity for registered dev source {worktree_root}"
        ));
    }
    let footprint = inspect_dev_source_footprint(dev)?;
    Ok(
        (!footprint.entries.is_empty() || !footprint.reserved_roots.is_empty())
            .then_some(footprint),
    )
}

fn borrowed_source_footprint(source: &Path) -> Result<SourceFootprint, String> {
    let mut footprint = inspect_source_footprint_paths(source, true)?;
    if let Some(source) = existing_cleanup_path(source)? {
        footprint.content_roots.insert(source);
    }
    reserve_path(source, &mut footprint)?;
    Ok(footprint)
}

pub(crate) fn ensure_environment_removal_preserves_dev_sources(
    target: &EnvMeta,
    envs: &[EnvMeta],
) -> Result<(), String> {
    let mut footprints = Vec::new();
    for meta in envs.iter().filter(|meta| {
        meta.name != target.name
            || meta
                .dev
                .as_ref()
                .is_some_and(|dev| dev.borrowed_source_root().is_some())
    }) {
        if let Some(footprint) = registered_dev_source_footprint(meta, envs)? {
            footprints.push((meta.name.as_str(), footprint));
        }
    }
    if footprints.is_empty() {
        return Ok(());
    }
    let removal = DevSourceRemoval::environment(target)?;
    for (owner, footprint) in footprints {
        if let Some(path) = removal.affected_path(&footprint)? {
            return Err(source_removal_error(&target.name, owner, path));
        }
    }
    Ok(())
}

pub(crate) fn ensure_dev_worktree_removal_preserves_dev_sources(
    name: &str,
    dev: &EnvDevMeta,
    envs: &[EnvMeta],
) -> Result<(), String> {
    let removal = DevSourceRemoval::worktree(dev)?;
    for meta in envs {
        if let Some(footprint) = registered_dev_source_footprint(meta, envs)?
            && let Some(path) = removal.affected_path(&footprint)?
        {
            return Err(source_removal_error(name, &meta.name, path));
        }
    }
    Ok(())
}

fn source_removal_error(name: &str, owner: &str, path: &Path) -> String {
    format!(
        "cleanup for env {name} would affect registered dev source for env {owner} at {}; remove the dependent dev env first",
        display_path(path)
    )
}

// Both removal and registration use the actual deletion boundaries. In
// particular, an owned child worktree does not own its shared common directory.
pub(crate) struct DevSourceRemoval {
    root: Option<PathBuf>,
    worktree: Option<PathBuf>,
    reserved_worktree: Option<PathBuf>,
    entry_parent: Option<PathBuf>,
    root_link: Option<PathBuf>,
    registrations: Vec<PathBuf>,
    registration_links: Vec<PathBuf>,
}

impl DevSourceRemoval {
    pub(crate) fn environment(target: &EnvMeta) -> Result<Self, String> {
        Self::new(Some(Path::new(&target.root)), target.dev.as_ref())
    }

    fn worktree(dev: &EnvDevMeta) -> Result<Self, String> {
        Self::new(None, Some(dev))
    }

    fn new(root_path: Option<&Path>, dev: Option<&EnvDevMeta>) -> Result<Self, String> {
        let root = root_path.map(existing_cleanup_path).transpose()?.flatten();
        let owned = dev.and_then(|dev| dev.owned_worktree());
        let worktree = owned
            .map(|(_, worktree)| existing_cleanup_path(Path::new(worktree)))
            .transpose()?
            .flatten();
        // Removing a final symlink also changes its containing source, even when
        // the link resolves outside that source (or its destination is missing).
        let entry_parent = match root_path {
            Some(path) if fs::symlink_metadata(path).is_ok_and(|meta| meta.is_symlink()) => path
                .parent()
                .map(existing_cleanup_path)
                .transpose()?
                .flatten(),
            _ => None,
        };
        let root_link = root_path
            .zip(entry_parent.as_ref())
            .and_then(|(path, parent)| path.file_name().map(|name| parent.join(name)));
        let mut registrations = Vec::new();
        let mut registration_links = Vec::new();
        if let Some((repo_root, worktree_root)) = owned {
            for path in crate::openclaw_repo::worktree_registration_entries(
                Path::new(repo_root),
                Path::new(worktree_root),
            )? {
                if fs::symlink_metadata(&path)
                    .map_err(|error| error.to_string())?
                    .is_symlink()
                {
                    // Git unlinks the administrative slot itself, not its referent.
                    let parent = fs::canonicalize(path.parent().unwrap())
                        .map_err(|error| error.to_string())?;
                    registration_links.push(parent.join(path.file_name().unwrap()));
                } else {
                    registrations.push(fs::canonicalize(path).map_err(|error| error.to_string())?);
                }
            }
        }
        // Missing working files can still leave a Git slot that removal deletes.
        // Without either deletion, an Owned record reserves no missing location.
        let reserved_worktree =
            if worktree.is_some() || !registrations.is_empty() || !registration_links.is_empty() {
                owned
                    .map(|(_, worktree)| {
                        super::dev_registration::registration_path(Path::new(worktree))
                            .map(|(path, _)| path)
                    })
                    .transpose()?
            } else {
                None
            };
        Ok(Self {
            root,
            worktree,
            reserved_worktree,
            entry_parent,
            root_link,
            registrations,
            registration_links,
        })
    }

    pub(crate) fn affected_path<'a>(
        &self,
        footprint: &'a SourceFootprint,
    ) -> Result<Option<&'a Path>, String> {
        for (path, contents) in footprint
            .entries
            .iter()
            .map(|path| (path, false))
            .chain(footprint.content_roots.iter().map(|path| (path, true)))
        {
            let mut affected = false;
            // An owned child worktree may be removed from inside another source.
            // Refuse only when that worktree itself contains a protected path.
            for (root, symmetric) in [(self.root.as_ref(), true), (self.worktree.as_ref(), false)] {
                if let Some(root) = root {
                    affected |= contains_existing(root, path)?
                        || (symmetric && contents && contains_existing(path, root)?);
                }
            }
            if let Some(parent) = &self.entry_parent
                && contents
            {
                affected |= contains_existing(path, parent)?;
            }
            for root in &self.registrations {
                affected |= contains_existing(root, path)?;
            }
            for entry in self.registration_links.iter().chain(self.root_link.iter()) {
                affected |= path_identity(entry)? == path_identity(path)?;
            }
            if affected {
                return Ok(Some(path));
            }
        }
        for reserved in &footprint.reserved_roots {
            for (path, symmetric) in [
                (self.root.as_ref(), true),
                (self.reserved_worktree.as_ref(), false),
            ] {
                let Some(path) = path else { continue };
                if projected_path_contains(path, reserved)?
                    || (symmetric && projected_path_contains(reserved, path)?)
                {
                    return Ok(Some(reserved));
                }
            }
        }
        Ok(None)
    }
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
        if let Some(source) = dev.borrowed_source_root() {
            ensure_root_outside_borrowed_source(name, root, &meta.name, Path::new(source))?;
            continue;
        }
        let source = match fs::canonicalize(dev.source_root()) {
            Ok(source) => source,
            // Missing source reservations belong to source registration. Its
            // existing cleanup verifies Git identity before removing a path
            // that has since been recreated by another environment.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "failed to resolve dev source {} for env {}: {error}",
                    dev.source_root(),
                    meta.name
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

pub(super) fn ensure_root_outside_borrowed_source(
    name: &str,
    root: &Path,
    source_name: &str,
    source: &Path,
) -> Result<(), String> {
    let footprint = borrowed_source_footprint(source)?;
    let (target, _) = super::dev_registration::registration_path(root)?;
    let entry_parent = if fs::symlink_metadata(root).is_ok_and(|meta| meta.is_symlink()) {
        let parent = root.parent().ok_or("environment root has no parent")?;
        Some(super::dev_registration::registration_path(parent)?.0)
    } else {
        None
    };
    for (path, contents) in footprint
        .entries
        .iter()
        .map(|path| (path, false))
        .chain(footprint.content_roots.iter().map(|path| (path, true)))
        .chain(footprint.reserved_roots.iter().map(|path| (path, true)))
    {
        if projected_path_contains(&target, path)?
            || (contents && projected_path_contains(path, &target)?)
            || (contents
                && entry_parent
                    .as_ref()
                    .map(|parent| projected_path_contains(path, parent))
                    .transpose()?
                    .unwrap_or(false))
        {
            return Err(format!(
                "environment {name} root {} overlaps borrowed source or Git metadata for env {source_name} at {}; choose a separate environment root",
                display_path(root),
                display_path(path)
            ));
        }
    }
    Ok(())
}

pub(crate) fn ensure_borrowed_source_isolation(
    name: &str,
    root: &Path,
    source_root: Option<&str>,
    envs: &[EnvMeta],
) -> Result<(), String> {
    for meta in envs {
        if let Some(source) = meta.dev.as_ref().and_then(|dev| dev.borrowed_source_root()) {
            ensure_root_outside_borrowed_source(name, root, &meta.name, Path::new(source))?;
        }
    }
    if let Some(source) = source_root {
        ensure_root_outside_borrowed_source(name, root, name, Path::new(source))?;
    }
    Ok(())
}
