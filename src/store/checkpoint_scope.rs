use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::dev_sources::path_identity;
use super::layout::display_path;
use super::openclaw_state::with_owner_write_permission;

/// The declared paths are opaque to checkpoints. In particular, checking their
/// contents to decide whether to omit them would repeat the original scope bug.
pub(crate) fn validate_independent_paths(paths: &[PathBuf]) -> Result<(), String> {
    for (index, path) in paths.iter().enumerate() {
        if path.as_os_str().is_empty()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(
                "independent paths must be nonempty environment-relative paths without . or .."
                    .to_string(),
            );
        }
        if paths[..index]
            .iter()
            .any(|other| path.starts_with(other) || other.starts_with(path))
        {
            return Err("independent paths must not overlap or repeat".to_string());
        }
    }
    Ok(())
}

pub(crate) fn validate_ancestors(root: &Path, paths: &[PathBuf]) -> Result<(), String> {
    validate_independent_paths(paths)?;
    for relative in paths {
        let mut ancestor = root.to_path_buf();
        require_directory(&ancestor)?;
        let parts = relative.components().collect::<Vec<_>>();
        for part in &parts[..parts.len() - 1] {
            ancestor.push(part);
            require_directory(&ancestor)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_scoped_artifact(root: &Path, paths: &[PathBuf]) -> Result<(), String> {
    validate_ancestors(root, paths)?;
    for relative in paths {
        if entry_exists(&root.join(relative))? {
            return Err(format!(
                "upgrade checkpoint unexpectedly contains independent path: {}",
                display_path(relative)
            ));
        }
    }
    Ok(())
}

// Scoped publication moves directory entries, irrespective of their targets.
// In particular, an earlier move can make a staged relative link dangling.
fn entry_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("failed to inspect {}: {error}", display_path(path))),
    }
}

fn require_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        _ => Err(format!(
            "independent path ancestor must remain a real directory: {}",
            display_path(path)
        )),
    }
}

// Resolve a scope component without following its final link or opening its
// contents. Exact entries stay distinct even when they are hard links.
fn scope_entry_name(
    directory: &Path,
    names: &BTreeSet<OsString>,
    name: &OsStr,
) -> Result<Option<OsString>, String> {
    if let Some(actual) = names.get(name) {
        return Ok(Some(actual.clone()));
    }
    let selected = directory.join(name);
    if !entry_exists(&selected)? {
        return Ok(None);
    }
    let identity = path_identity(&selected)?;
    let mut alias = None;
    for actual in names {
        if path_identity(&directory.join(actual))? == identity {
            // The spelling selects an entry, but several hard links can share
            // its identity. Refuse before moving anything if it is ambiguous.
            if alias.is_some() {
                return Err(format!(
                    "independent path has ambiguous directory entries: {}",
                    display_path(&selected)
                ));
            }
            alias = Some(actual.clone());
        }
    }
    alias.map(Some).ok_or_else(|| {
        format!(
            "independent path changed while inspecting directory entries: {}",
            display_path(&selected)
        )
    })
}

fn collect_owned_entries(
    current: &Path,
    candidate: &Path,
    relative: &Path,
    independent: &[PathBuf],
    entries: &mut Vec<PathBuf>,
    ancestors: &mut Vec<PathBuf>,
) -> Result<(), String> {
    // Exclusions are relative to this level, so case-equivalent ancestor
    // spellings can contribute their children to the same preserved directory.
    if independent.iter().any(|path| path.as_os_str().is_empty()) {
        return Ok(());
    }
    if independent.is_empty() {
        entries.push(relative.to_path_buf());
        return Ok(());
    }
    ancestors.push(relative.to_path_buf());
    let current_dir = current.join(relative);
    let candidate_dir = candidate.join(relative);
    let read_names = |dir: &Path| -> Result<BTreeSet<OsString>, String> {
        require_directory(dir)?;
        fs::read_dir(dir)
            .map_err(|error| error.to_string())?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<Result<_, _>>()
            .map_err(|error| error.to_string())
    };
    let current_names = read_names(&current_dir)?;
    let candidate_names = read_names(&candidate_dir)?;
    let mut children: BTreeMap<OsString, Vec<PathBuf>> = BTreeMap::new();
    let mut aliases = BTreeMap::new();
    for path in independent {
        let mut parts = path.components();
        let name = parts.next().expect("nonempty exclusion").as_os_str();
        let live = scope_entry_name(&current_dir, &current_names, name)?;
        let staged = scope_entry_name(&candidate_dir, &candidate_names, name)?;
        let selected = live
            .as_ref()
            .or(staged.as_ref())
            .cloned()
            .unwrap_or_else(|| name.to_os_string());
        for actual in [live, staged].into_iter().flatten() {
            aliases.insert(actual, selected.clone());
        }
        children.entry(selected).or_default().push(parts.collect());
    }
    let names = current_names
        .into_iter()
        .chain(candidate_names)
        .map(|name| aliases.get(&name).cloned().unwrap_or(name))
        .collect::<BTreeSet<_>>();
    for name in names {
        collect_owned_entries(
            current,
            candidate,
            &relative.join(&name),
            children.get(&name).map(Vec::as_slice).unwrap_or_default(),
            entries,
            ancestors,
        )?;
    }
    Ok(())
}

#[derive(Default)]
struct Replacement {
    relative: PathBuf,
    displaced: bool,
    installed: bool,
}

/// Replace owned entries, leaving independent paths and all their ancestors in
/// place. The displaced owned entries are retained until service acceptance.
/// On any publication failure, reverse completed moves before returning.
pub(crate) fn replace_owned_entries(
    current: &Path,
    candidate: &Path,
    displaced: &Path,
    independent: &[PathBuf],
) -> Result<(), super::snapshots::SnapshotRestoreError> {
    use super::snapshots::SnapshotRestoreError;
    validate_ancestors(current, independent).map_err(SnapshotRestoreError::unchanged)?;
    validate_ancestors(candidate, independent).map_err(SnapshotRestoreError::unchanged)?;
    let mut entries = Vec::new();
    let mut ancestors = Vec::new();
    collect_owned_entries(
        current,
        candidate,
        Path::new(""),
        independent,
        &mut entries,
        &mut ancestors,
    )
    .map_err(SnapshotRestoreError::unchanged)?;
    // The sparse backup needs the same directory skeleton for compensation and
    // later service-acceptance rollback, even when an ancestor has no owned child.
    for relative in ancestors {
        fs::create_dir_all(displaced.join(relative))
            .map_err(|error| error.to_string())
            .map_err(SnapshotRestoreError::unchanged)?;
    }
    for relative in &entries {
        if entry_exists(&displaced.join(relative)).map_err(SnapshotRestoreError::unchanged)? {
            return Err(SnapshotRestoreError::unchanged(format!(
                "scoped restore destination already exists: {}",
                display_path(&displaced.join(relative))
            )));
        }
    }
    let mut completed: Vec<Replacement> = Vec::new();
    let result = (|| {
        for relative in entries {
            completed.push(Replacement {
                relative: relative.clone(),
                ..Default::default()
            });
            let replacement = completed.last_mut().expect("replacement just inserted");
            let live = current.join(&relative);
            if entry_exists(&live)? {
                fs::rename(&live, displaced.join(&relative)).map_err(|error| error.to_string())?;
                replacement.displaced = true;
            }
            let staged = candidate.join(&relative);
            if entry_exists(&staged)? {
                // Shared ancestors stay in place, so their captured modes do
                // not need to permit removing children from our staging tree.
                // Never relax the corresponding live ancestor's permissions.
                with_owner_write_permission(
                    staged.parent().expect("owned entry has a parent"),
                    || {
                        fs::rename(&staged, &live).map_err(|error| error.to_string())?;
                        // Permission restoration can fail after the move; retain
                        // that move in the ledger so compensation still undoes it.
                        replacement.installed = true;
                        Ok(())
                    },
                )?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        for replacement in completed.iter().rev() {
            let live = current.join(&replacement.relative);
            let revert = (|| {
                if replacement.installed {
                    let staged = candidate.join(&replacement.relative);
                    with_owner_write_permission(
                        staged.parent().expect("owned entry has a parent"),
                        || fs::rename(&live, &staged).map_err(|error| error.to_string()),
                    )?;
                }
                if replacement.displaced {
                    fs::rename(displaced.join(&replacement.relative), &live)
                        .map_err(|error| error.to_string())?;
                }
                Ok::<_, String>(())
            })();
            if let Err(revert_error) = revert {
                return Err(SnapshotRestoreError {
                    message: format!(
                        "scoped restore failed ({error}); compensation failed ({revert_error}); recovery entries retained at {}",
                        display_path(displaced)
                    ),
                    prior_state_restored: false,
                });
            }
        }
        return Err(SnapshotRestoreError::unchanged(error));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    #[test]
    fn independent_and_owned_hardlinks_remain_distinct_entries() {
        let temp = tempfile::tempdir().unwrap();
        let current = temp.path().join("current");
        let candidate = temp.path().join("candidate");
        let backup = temp.path().join("backup");
        for root in [&current, &candidate] {
            fs::create_dir(root).unwrap();
        }
        fs::write(current.join("project"), "independent").unwrap();
        fs::hard_link(current.join("project"), current.join("owned")).unwrap();
        fs::write(candidate.join("owned"), "saved").unwrap();
        let identity = path_identity(&current.join("project")).unwrap();
        let paths = [PathBuf::from("project")];
        replace_owned_entries(&current, &candidate, &backup, &paths).unwrap();
        assert_eq!(fs::read(current.join("project")).unwrap(), b"independent");
        assert_eq!(path_identity(&current.join("project")).unwrap(), identity);
        assert_eq!(fs::read(current.join("owned")).unwrap(), b"saved");

        fs::rename(current.join("project"), current.join("Project")).unwrap();
        if !current.join("project").exists() {
            return; // The ambiguous native lookup needs a case-insensitive volume.
        }
        fs::hard_link(current.join("Project"), current.join("second-link")).unwrap();
        let error = replace_owned_entries(&current, &backup, &candidate, &paths).unwrap_err();
        assert!(error.prior_state_restored);
        assert!(error.message.contains("ambiguous directory entries"));
        assert_eq!(fs::read(current.join("owned")).unwrap(), b"saved");
        assert_eq!(path_identity(&current.join("Project")).unwrap(), identity);
    }

    #[test]
    fn partial_publication_failure_reinstates_owned_entries_and_leaves_project_in_place() {
        // Permission denial requires an unprivileged test process.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let current = temp.path().join("current");
        let candidate = temp.path().join("candidate");
        let backup = temp.path().join("backup");
        for root in [&current, &candidate] {
            fs::create_dir_all(root.join("z-parent")).unwrap();
            fs::write(
                root.join("a-first"),
                root.file_name().unwrap().as_encoded_bytes(),
            )
            .unwrap();
            fs::write(root.join("z-parent/owned"), "runtime").unwrap();
        }
        symlink("missing-live", current.join("b-link")).unwrap();
        symlink("missing-staged", candidate.join("b-link")).unwrap();
        fs::create_dir(current.join("z-parent/project")).unwrap();
        fs::write(current.join("z-parent/project/file"), "developer").unwrap();
        let before = fs::metadata(current.join("z-parent/project/file")).unwrap();
        fs::set_permissions(current.join("z-parent"), fs::Permissions::from_mode(0o500)).unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o555)).unwrap();
        let result = replace_owned_entries(
            &current,
            &candidate,
            &backup,
            &[PathBuf::from("z-parent/project")],
        );
        let live_mode = fs::metadata(current.join("z-parent")).unwrap().mode() & 0o777;
        let staged_mode = fs::metadata(&candidate).unwrap().mode() & 0o777;
        fs::set_permissions(current.join("z-parent"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.unwrap_err().prior_state_restored);
        assert_eq!(live_mode, 0o500);
        assert_eq!(staged_mode, 0o555);
        assert_eq!(
            fs::read_link(current.join("b-link")).unwrap(),
            Path::new("missing-live")
        );
        assert_eq!(
            fs::read_link(candidate.join("b-link")).unwrap(),
            Path::new("missing-staged")
        );
        assert_eq!(fs::read(current.join("a-first")).unwrap(), b"current");
        assert_eq!(fs::read(candidate.join("a-first")).unwrap(), b"candidate");
        assert_eq!(
            fs::metadata(current.join("z-parent/project/file"))
                .unwrap()
                .ino(),
            before.ino()
        );
        assert_eq!(
            fs::read(current.join("z-parent/project/file")).unwrap(),
            b"developer"
        );
    }

    #[test]
    fn acceptance_rollback_preserves_new_independent_work_and_removes_new_owned_state() {
        let temp = tempfile::tempdir().unwrap();
        let current = temp.path().join("current");
        let candidate = temp.path().join("candidate");
        let backup = temp.path().join("backup");
        let rejected = temp.path().join("rejected");
        for root in [&current, &candidate] {
            fs::create_dir_all(root.join("workspace")).unwrap();
        }
        fs::create_dir(current.join("workspace/project")).unwrap();
        fs::write(current.join("owned"), "original").unwrap();
        fs::write(candidate.join("owned"), "restored").unwrap();
        symlink("missing-original", current.join("link")).unwrap();
        symlink("missing-restored", candidate.join("link")).unwrap();
        let paths = [PathBuf::from("workspace/project")];
        replace_owned_entries(&current, &candidate, &backup, &paths).unwrap();
        fs::write(current.join("workspace/project/new"), "new work").unwrap();
        fs::write(current.join("new-runtime-state"), "new runtime").unwrap();
        symlink("missing-new", current.join("new-link")).unwrap();
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o555)).unwrap();
        replace_owned_entries(&current, &backup, &rejected, &paths).unwrap();
        assert_eq!(fs::metadata(&backup).unwrap().mode() & 0o777, 0o555);
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(fs::read(current.join("owned")).unwrap(), b"original");
        assert_eq!(
            fs::read(current.join("workspace/project/new")).unwrap(),
            b"new work"
        );
        assert!(!current.join("new-runtime-state").exists());
        assert_eq!(
            fs::read_link(current.join("link")).unwrap(),
            Path::new("missing-original")
        );
        assert!(fs::symlink_metadata(current.join("new-link")).is_err());
    }
    #[test]
    fn dangling_entries_do_not_bypass_scope_or_displacement_guards() {
        let temp = tempfile::tempdir().unwrap();
        let current = temp.path().join("current");
        let candidate = temp.path().join("candidate");
        let backup = temp.path().join("backup");
        for root in [&current, &candidate, &backup] {
            fs::create_dir(root).unwrap();
        }
        let paths = [PathBuf::from("project")];
        symlink("missing-project", candidate.join("project")).unwrap();
        assert!(validate_scoped_artifact(&candidate, &paths).is_err());
        fs::remove_file(candidate.join("project")).unwrap();
        validate_scoped_artifact(&candidate, &paths).unwrap();

        fs::write(current.join("owned"), "live").unwrap();
        fs::write(candidate.join("owned"), "saved").unwrap();
        symlink("missing-backup", backup.join("owned")).unwrap();
        let error = replace_owned_entries(&current, &candidate, &backup, &paths).unwrap_err();
        assert!(error.prior_state_restored);
        assert!(error.message.contains("destination already exists"));
        assert_eq!(fs::read(current.join("owned")).unwrap(), b"live");
        assert_eq!(fs::read(candidate.join("owned")).unwrap(), b"saved");
        assert_eq!(
            fs::read_link(backup.join("owned")).unwrap(),
            Path::new("missing-backup")
        );
    }
}
