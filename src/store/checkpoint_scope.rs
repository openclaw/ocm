use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::common::path_exists;
use super::layout::display_path;

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
        if path_exists(&root.join(relative)) {
            return Err(format!(
                "upgrade checkpoint unexpectedly contains independent path: {}",
                display_path(relative)
            ));
        }
    }
    Ok(())
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

fn collect_owned_entries(
    current: &Path,
    candidate: &Path,
    relative: &Path,
    independent: &[PathBuf],
    entries: &mut Vec<PathBuf>,
    ancestors: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if independent.iter().any(|path| path == relative) {
        return Ok(());
    }
    if !independent.iter().any(|path| path.starts_with(relative)) {
        entries.push(relative.to_path_buf());
        return Ok(());
    }
    ancestors.push(relative.to_path_buf());
    let mut names = BTreeSet::new();
    for root in [current, candidate] {
        let dir = root.join(relative);
        require_directory(&dir)?;
        for entry in fs::read_dir(dir).map_err(|error| error.to_string())? {
            names.insert(entry.map_err(|error| error.to_string())?.file_name());
        }
    }
    for name in names {
        collect_owned_entries(
            current,
            candidate,
            &relative.join(name),
            independent,
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
        if path_exists(&displaced.join(relative)) {
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
            if path_exists(&live) {
                fs::rename(&live, displaced.join(&relative)).map_err(|error| error.to_string())?;
                replacement.displaced = true;
            }
            let staged = candidate.join(&relative);
            if path_exists(&staged) {
                fs::rename(&staged, &live).map_err(|error| error.to_string())?;
                replacement.installed = true;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        for replacement in completed.iter().rev() {
            let live = current.join(&replacement.relative);
            let revert = (|| {
                if replacement.installed {
                    fs::rename(&live, candidate.join(&replacement.relative))?;
                }
                if replacement.displaced {
                    fs::rename(displaced.join(&replacement.relative), &live)?;
                }
                Ok::<_, std::io::Error>(())
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
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

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
        fs::create_dir(current.join("z-parent/project")).unwrap();
        fs::write(current.join("z-parent/project/file"), "developer").unwrap();
        let before = fs::metadata(current.join("z-parent/project/file")).unwrap();
        fs::set_permissions(current.join("z-parent"), fs::Permissions::from_mode(0o500)).unwrap();
        let result = replace_owned_entries(
            &current,
            &candidate,
            &backup,
            &[PathBuf::from("z-parent/project")],
        );
        fs::set_permissions(current.join("z-parent"), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
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
        let paths = [PathBuf::from("workspace/project")];
        replace_owned_entries(&current, &candidate, &backup, &paths).unwrap();
        fs::write(current.join("workspace/project/new"), "new work").unwrap();
        fs::write(current.join("new-runtime-state"), "new runtime").unwrap();
        replace_owned_entries(&current, &backup, &rejected, &paths).unwrap();
        assert_eq!(fs::read(current.join("owned")).unwrap(), b"original");
        assert_eq!(
            fs::read(current.join("workspace/project/new")).unwrap(),
            b"new work"
        );
        assert!(!current.join("new-runtime-state").exists());
    }
}
