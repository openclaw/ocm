use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::env::{EnvDevMeta, EnvMeta};
use crate::openclaw_repo::{
    default_worktree_root, ensure_openclaw_worktree, remove_openclaw_worktree,
};

use super::dev_sources::{
    DevSourceRemoval, SourceFootprint, ensure_dev_worktree_removal_preserves_dev_sources,
    existing_source_path_entries, inspect_dev_source_footprint, inspect_source_footprint,
    path_identity,
};
use super::envs::{environment_operation_lock_path, try_lock_environment_operation};
use super::{
    EnvironmentOperationLock, display_path, list_environments, validate_name,
    with_locked_environments,
};

#[derive(PartialEq, Eq)]
struct SourceIdentity {
    repo: SourceFootprint,
    footprint: SourceFootprint,
    paths: BTreeMap<PathBuf, (u64, u64)>,
    locations: Vec<PathBuf>,
    preparation_paths: BTreeSet<PathBuf>,
}

pub(super) fn registration_path(path: &Path) -> Result<(PathBuf, BTreeSet<PathBuf>), String> {
    let mut pending = path.to_path_buf();
    let mut entries = BTreeSet::new();
    loop {
        let mut existing = pending.as_path();
        loop {
            match fs::canonicalize(existing) {
                Ok(resolved) => {
                    entries.extend(existing_source_path_entries(existing)?);
                    let suffix = pending
                        .strip_prefix(existing)
                        .map_err(|error| error.to_string())?;
                    return Ok((resolved.join(suffix), entries));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
            // A registered symlink root may outlive its target during restore.
            // Follow only this known link, retaining its own entry as a dependency.
            match fs::read_link(existing) {
                Ok(target) => {
                    let parent = existing.parent().ok_or("source link has no parent")?;
                    entries.extend(existing_source_path_entries(parent)?);
                    let entry = fs::canonicalize(parent)
                        .map_err(|error| error.to_string())?
                        .join(existing.file_name().ok_or("source link has no name")?);
                    if !entries.insert(entry) {
                        return Err("dev source contains a symlink cycle".to_string());
                    }
                    let target = if target.is_absolute() {
                        target
                    } else {
                        parent.join(target)
                    };
                    pending = target.join(
                        pending
                            .strip_prefix(existing)
                            .map_err(|error| error.to_string())?,
                    );
                    break;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
                    ) =>
                {
                    existing = existing
                        .parent()
                        .ok_or("dev source has no existing ancestor")?;
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    }
}

fn source_identity(dev: &EnvDevMeta) -> Result<SourceIdentity, String> {
    let repo = inspect_source_footprint(Path::new(dev.repo_root()))?;
    let mut footprint = inspect_dev_source_footprint(dev)?;
    let mut preparation_paths = repo.entries.clone();
    preparation_paths.extend(repo.content_roots.iter().cloned());
    let mut locations = Vec::new();
    // Before worktree creation, retain the existing path through .worktrees,
    // including symlinks into another environment. This reserves no new paths.
    for root in [dev.repo_root(), dev.source_root()] {
        let (resolved, entries) = registration_path(Path::new(root))?;
        locations.push(resolved);
        preparation_paths.extend(entries.iter().cloned());
        footprint.entries.extend(entries);
    }
    let paths = footprint
        .entries
        .iter()
        .chain(&footprint.content_roots)
        .map(|path| Ok((path.clone(), path_identity(path)?)))
        .collect::<Result<_, String>>()?;
    Ok(SourceIdentity {
        repo,
        footprint,
        paths,
        locations,
        preparation_paths,
    })
}

fn containing_environments(
    identity: &SourceIdentity,
    envs: &[EnvMeta],
) -> Result<BTreeSet<String>, String> {
    let mut owners = BTreeSet::new();
    for meta in envs {
        if DevSourceRemoval::environment(meta)?
            .affected_path(&identity.footprint)?
            .is_some()
        {
            owners.insert(meta.name.clone());
            continue;
        }
        // Displaced roots still belong to the operation that will restore them.
        for root in std::iter::once(meta.root.as_str()).chain(
            meta.dev
                .as_ref()
                .and_then(|dev| dev.owned_worktree())
                .map(|(_, root)| root),
        ) {
            let (root, _) = registration_path(Path::new(root))?;
            if !root.exists()
                && identity
                    .locations
                    .iter()
                    .any(|path| path.starts_with(&root))
            {
                owners.insert(meta.name.clone());
                break;
            }
        }
    }
    Ok(owners)
}

fn source_changed(name: &str, dev: Option<&EnvDevMeta>, envs: &[EnvMeta]) -> bool {
    dev.is_some()
        && dev
            != envs
                .iter()
                .find(|meta| meta.name == name)
                .and_then(|meta| meta.dev.as_ref())
}

#[derive(Default)]
pub(crate) struct DevSourceRegistration {
    source: Option<(EnvDevMeta, SourceIdentity)>,
    owners: BTreeSet<String>,
    _locks: Vec<EnvironmentOperationLock>,
    held_files: BTreeSet<(u64, u64)>,
    _daemon_lifecycle: Option<super::ExclusiveFileLock>,
}

impl DevSourceRegistration {
    pub(crate) fn acquire(
        name: &str,
        dev: Option<&EnvDevMeta>,
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> Result<Self, String> {
        let name = validate_name(name, "Environment name")?;
        let Some(dev) = dev else {
            return Ok(Self::default());
        };
        let envs = list_environments(env, cwd)?;
        if !source_changed(&name, Some(dev), &envs) {
            return Ok(Self::default());
        }
        Self::lock_source(dev, &envs, env, cwd)
    }

    fn lock_source(
        dev: &EnvDevMeta,
        envs: &[EnvMeta],
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> Result<Self, String> {
        if dev.borrowed_source_root().is_some() {
            dev.execution_source_root()?;
        }
        let identity = source_identity(dev)?;
        let mut registration = Self {
            source: Some((dev.clone(), identity)),
            ..Self::default()
        };
        registration.lock_owners(envs, env, cwd)?;
        if dev.borrowed_source_root().is_some() {
            // The containing owner's operation may have completed since inspection.
            dev.execution_source_root()?;
            registration._daemon_lifecycle = crate::supervisor::SupervisorService::new(env, cwd)
                .lock_borrowed_source_publication()?;
        }
        Ok(registration)
    }

    fn lock_owners(
        &mut self,
        envs: &[EnvMeta],
        env: &BTreeMap<String, String>,
        cwd: &Path,
    ) -> Result<(), String> {
        let Some((_, identity)) = &self.source else {
            return Err(Self::changed());
        };
        let owners = containing_environments(identity, envs)?;
        for owner in owners.difference(&self.owners).cloned().collect::<Vec<_>>() {
            let path = environment_operation_lock_path(&owner, env, cwd)?;
            // Distinct registered names can share a lock on case-insensitive filesystems.
            if !path_identity(&path).is_ok_and(|identity| self.held_files.contains(&identity)) {
                let lock = try_lock_environment_operation(&owner, env, cwd)?.ok_or_else(|| format!(
                    "cannot register dev source while environment {owner} has an operation in progress; retry after it finishes"
                ))?;
                self.held_files.insert(path_identity(&path)?);
                self._locks.push(lock);
            }
            self.owners.insert(owner);
        }
        Ok(())
    }

    pub(crate) fn recheck(
        &self,
        name: &str,
        dev: Option<&EnvDevMeta>,
        envs: &[EnvMeta],
    ) -> Result<(), String> {
        // Recovery writes can run with the target operation lock already held,
        // including while its source is displaced. Recheck the tuple under registry.
        if !source_changed(name.trim(), dev, envs) {
            return Ok(());
        }
        self.recheck_source(dev.ok_or("missing dev source")?, envs)
    }

    fn recheck_source(&self, dev: &EnvDevMeta, envs: &[EnvMeta]) -> Result<(), String> {
        if dev.borrowed_source_root().is_some() {
            dev.execution_source_root()?;
        }
        let Some((expected, identity)) = &self.source else {
            return Err(Self::changed());
        };
        if expected != dev || source_identity(dev)? != *identity {
            return Err(Self::changed());
        }
        if !containing_environments(identity, envs)?.is_subset(&self.owners) {
            return Err(Self::changed());
        }
        Ok(())
    }

    fn prepared(&mut self, dev: &EnvDevMeta, created: bool) -> Result<(), String> {
        let Some((expected, before)) = &self.source else {
            return Err(Self::changed());
        };
        let after = source_identity(dev)?;
        if expected != dev
            || (!created && *before != after)
            || before.repo != after.repo
            || before.preparation_paths.iter().any(|path| {
                !path_identity(path).is_ok_and(|current| before.paths.get(path) == Some(&current))
            })
        {
            return Err(Self::changed());
        }
        self.source = Some((dev.clone(), after));
        Ok(())
    }

    fn changed() -> String {
        "dev source or containing environment changed during registration; retry the command"
            .to_string()
    }
}

pub(crate) fn with_prepared_dev_source<T>(
    repo: &Path,
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
    publish: impl FnOnce(EnvDevMeta, &DevSourceRegistration) -> Result<T, String>,
) -> Result<T, String> {
    let name = validate_name(name, "Environment name")?;
    let dev = EnvDevMeta::Owned {
        repo_root: display_path(repo),
        worktree_root: display_path(&default_worktree_root(repo, &name)),
    };
    let mut registration =
        DevSourceRegistration::lock_source(&dev, &list_environments(env, cwd)?, env, cwd)?;
    with_locked_environments(env, cwd, |envs| registration.recheck_source(&dev, envs))?;
    let worktree = ensure_openclaw_worktree(repo, &name)?;
    let result = registration
        .prepared(&dev, worktree.created)
        .and_then(|()| publish(dev.clone(), &registration));
    if let Err(error) = result {
        if worktree.created {
            // Publication locks have dropped. A newly registered owner may now
            // need its operation lock for cleanup, acquired before registry.
            let cleanup = list_environments(env, cwd)
                .and_then(|envs| registration.lock_owners(&envs, env, cwd))
                .and_then(|()| {
                    with_locked_environments(env, cwd, |envs| {
                        registration.recheck_source(&dev, envs)?;
                        ensure_dev_worktree_removal_preserves_dev_sources(&name, &dev, envs)?;
                        remove_openclaw_worktree(repo, &worktree.root)
                    })
                });
            if let Err(cleanup) = cleanup {
                return Err(format!(
                    "{error}; prepared worktree retained at {}: {cleanup}",
                    display_path(&worktree.root)
                ));
            }
        }
        return Err(error);
    }
    result
}

#[cfg(test)]
#[path = "dev_registration_tests.rs"]
mod tests;
