use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::env::{
    CreateEnvSnapshotOptions, EnvMeta, EnvSnapshotRemoveSummary, EnvSnapshotRestoreSummary,
    EnvSnapshotSummary, RemoveEnvSnapshotOptions, RestoreEnvSnapshotOptions,
    UpgradeCheckpointScope, default_service_enabled, default_service_running,
};
use crate::infra::archive::{EnvArchiveMetadata, extract_env_archive};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::checkpoints::{
    CheckpointCleanup, PreparedTreeCheckpoint, STORAGE_TAR_ARCHIVE, copy_tree_checkpoint,
    create_tree_checkpoint_from_preparation, default_snapshot_storage_kind,
    prepare_scoped_tree_checkpoint_in, remove_tree_if_present,
};
use super::common::{copy_path_recursive, load_json_files, path_exists, read_json, write_json};
use super::dev_sources::{
    contains_existing, path_identity, projected_path_contains, projected_path_relative,
    registered_dev_source_footprint,
};
use super::layout::{
    derive_env_paths, display_path, snapshot_archive_path, snapshot_checkpoint_path,
    snapshot_env_dir, snapshot_meta_path, validate_name,
};
use super::{
    OpenClawWorkspaceRuntime, audit_openclaw_state, clear_nonportable_runtime_state,
    get_environment, list_environments, now_utc, remove_upgrade_recovery_for_snapshot,
    rewrite_openclaw_config_for_target, save_environment,
};

const UPGRADE_CHECKPOINT_KIND: &str = "ocm-env-upgrade-checkpoint-v1";

static NEXT_REMOVAL_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvSnapshotMeta {
    pub kind: String,
    pub id: String,
    pub env_name: String,
    #[serde(default)]
    pub label: Option<String>,
    pub archive_path: String,
    #[serde(default = "default_snapshot_storage_kind")]
    pub storage_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade_scope: Option<UpgradeCheckpointScope>,
    pub source_root: String,
    pub gateway_port: Option<u32>,
    #[serde(default)]
    pub gateway_port_auto_assigned: bool,
    #[serde(default = "default_service_enabled")]
    pub service_enabled: bool,
    #[serde(default = "default_service_running")]
    pub service_running: bool,
    pub default_runtime: Option<String>,
    pub default_launcher: Option<String>,
    pub protected: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug)]
pub(crate) struct EnvSnapshotRestoreTransaction {
    pub(crate) summary: EnvSnapshotRestoreSummary,
    original: EnvMeta,
    operation_root: PathBuf,
    displaced_root: Option<PathBuf>,
    rejected_root: PathBuf,
    restored_root: PathBuf,
    independent: Vec<PathBuf>,
}

impl EnvSnapshotRestoreTransaction {
    pub(crate) fn retained_operation_note(&self) -> String {
        format!(
            "restore operation retained at {}",
            display_path(&self.operation_root)
        )
    }
}

#[derive(Debug)]
pub(crate) struct PreparedEnvSnapshotCapture {
    env_name: String,
    source_root: PathBuf,
    checkpoint: PreparedTreeCheckpoint,
    upgrade_scope: Option<UpgradeCheckpointScope>,
}

impl PreparedEnvSnapshotCapture {
    pub(crate) fn cleanup_guard(&self) -> CheckpointCleanup {
        self.checkpoint.cleanup_guard()
    }
}

#[derive(Debug)]
struct RestoreOperationNamespace {
    root: PathBuf,
    candidate_root: PathBuf,
    backup_root: PathBuf,
    legacy_staging_root: PathBuf,
    rejected_root: PathBuf,
}

/// Resolve existing aliases while retaining absent descendants. An unresolved
/// symlink is ambiguous ownership, not evidence that a workspace is unrelated.
fn resolve_scope_path(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match fs::canonicalize(existing) {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::symlink_metadata(existing) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => {
                        return Err(format!(
                            "cannot resolve workspace ownership: {}",
                            display_path(path)
                        ));
                    }
                }
                missing.push(
                    existing
                        .file_name()
                        .ok_or("missing workspace path name")?
                        .to_os_string(),
                );
                existing = existing.parent().ok_or("missing workspace path parent")?;
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

pub(crate) fn validate_upgrade_independent_paths(
    meta: &EnvMeta,
    independent: &[PathBuf],
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    if independent.is_empty() {
        return Ok(());
    }
    let paths = derive_env_paths(Path::new(&meta.root));
    super::checkpoint_scope::validate_ancestors(&paths.root, independent)?;
    let workspaces = super::resolve_env_openclaw_workspaces(
        &paths,
        env,
        OpenClawWorkspaceRuntime::for_env(&meta.name, meta.gateway_port),
    )?;
    let workspace_targets = workspaces
        .workspace_roots()
        .map(|workspace| resolve_scope_path(workspace))
        .collect::<Result<Vec<_>, _>>()?;
    let mut configuration =
        super::openclaw_config_include_paths(&paths.config_path, &paths.state_dir)?;
    configuration.push(paths.config_path.clone());
    for config in configuration.clone() {
        match fs::canonicalize(&config) {
            Ok(target) => configuration.push(target),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    for relative in independent {
        let absolute = paths.root.join(relative);
        // A directory boundary keeps a database together with adjacent WAL and
        // journal files. Never infer ownership from extensions or inspect the
        // declared directory's contents.
        match fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.is_dir() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("independent paths must name directories (which may not yet exist); declare a parent directory for files or symlinks".to_string()),
        }
        let resolved =
            fs::canonicalize(absolute.parent().ok_or("missing independent path parent")?)
                .map_err(|error| error.to_string())?
                .join(
                    absolute
                        .file_name()
                        .ok_or("missing independent path name")?,
                );
        if configuration
            .iter()
            .any(|config| config.starts_with(&absolute) || config.starts_with(&resolved))
        {
            return Err(format!(
                "independent path includes rollback-owned configuration: {}",
                display_path(relative)
            ));
        }
        if workspaces
            .workspace_roots()
            .any(|workspace| workspace.starts_with(&absolute))
            || workspace_targets
                .iter()
                .any(|workspace| workspace.starts_with(&resolved))
        {
            return Err(format!(
                "a configured workspace can contain migration-owned state; declare only independent content beneath it: {}",
                display_path(relative)
            ));
        }
        // Preserve established workspace eligibility before considering new
        // locations: unrelated hidden aliases must not invalidate saved policy.
        if workspaces
            .workspace_roots()
            .any(|workspace| absolute.starts_with(workspace))
            || workspace_targets
                .iter()
                .any(|workspace| resolved.starts_with(workspace))
        {
            continue;
        }
        // OpenClaw keeps managed checkout contents beside its workspace, while
        // their registry remains in the owned state database. Operators may
        // also keep projects elsewhere in the environment's home. These are
        // eligible locations, not automatic exclusions: every boundary still
        // requires an explicit declaration and all checks above.
        let mut managed_worktree_content = relative.starts_with(".openclaw/worktrees");
        // Keep other hidden home/state namespaces closed, including unknown
        // future state directories and tool credential/configuration homes.
        let mut home_content = relative
            .components()
            .next()
            .and_then(|part| part.as_os_str().to_str())
            .is_some_and(|name| !name.starts_with('.'));
        if home_content || managed_worktree_content {
            // A hidden state/tool home may alias an ordinary directory. Check
            // only top-level aliases, never the declared project's contents.
            for entry in fs::read_dir(&paths.root).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                if managed_worktree_content && entry.file_name() == ".openclaw" {
                    continue;
                }
                if entry.file_name().to_string_lossy().starts_with('.')
                    && entry
                        .file_type()
                        .map_err(|error| error.to_string())?
                        .is_symlink()
                {
                    let target = resolve_scope_path(&entry.path())?;
                    if resolved.starts_with(&target) || target.starts_with(&resolved) {
                        home_content = false;
                        managed_worktree_content = false;
                        break;
                    }
                }
            }
        }
        if !managed_worktree_content && !home_content {
            return Err(format!(
                "independent paths must be beneath a configured workspace, in .openclaw/worktrees, or in a non-hidden environment-home directory: {}",
                display_path(relative)
            ));
        }
    }
    Ok(())
}

pub fn create_env_snapshot(
    options: CreateEnvSnapshotOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvSnapshotMeta, String> {
    create_env_snapshot_with_service_state(options, None, env, cwd)
}

pub(crate) fn create_env_snapshot_with_service_state(
    options: CreateEnvSnapshotOptions,
    service_state: Option<(bool, bool)>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvSnapshotMeta, String> {
    let prepared = prepare_env_snapshot_capture(&options.env_name, env, cwd)?;
    create_env_snapshot_from_preparation(options, service_state, prepared, env, cwd)
}

pub(crate) fn prepare_env_snapshot_capture(
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PreparedEnvSnapshotCapture, String> {
    prepare_snapshot_capture(env_name, false, env, cwd)
}

pub(crate) fn prepare_upgrade_checkpoint_capture(
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PreparedEnvSnapshotCapture, String> {
    prepare_snapshot_capture(env_name, true, env, cwd)
}

fn prepare_snapshot_capture(
    env_name: &str,
    upgrade: bool,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PreparedEnvSnapshotCapture, String> {
    let env_name = validate_name(env_name, "Environment name")?;
    let meta = get_environment(&env_name, env, cwd)?;
    let env_paths = derive_env_paths(Path::new(&meta.root));
    if !path_exists(&env_paths.root) {
        return Err(format!(
            "environment root does not exist: {}",
            display_path(&env_paths.root)
        ));
    }
    let upgrade_scope = if upgrade {
        validate_upgrade_independent_paths(&meta, &meta.upgrade_independent_paths, env)?;
        Some(UpgradeCheckpointScope {
            independent_paths: meta.upgrade_independent_paths.clone(),
        })
    } else {
        None
    };
    let independent = upgrade_scope
        .as_ref()
        .map(|scope| scope.independent_paths.as_slice())
        .unwrap_or_default();
    let checkpoint = prepare_scoped_tree_checkpoint_in(
        &env_paths.root,
        &snapshot_env_dir(&env_name, env, cwd)?,
        independent,
    )?;
    Ok(PreparedEnvSnapshotCapture {
        env_name,
        source_root: env_paths.root,
        checkpoint,
        upgrade_scope,
    })
}

pub(crate) fn create_env_snapshot_from_preparation(
    options: CreateEnvSnapshotOptions,
    service_state: Option<(bool, bool)>,
    prepared: PreparedEnvSnapshotCapture,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvSnapshotMeta, String> {
    let env_name = validate_name(&options.env_name, "Environment name")?;
    if prepared.env_name != env_name {
        return Err(format!(
            "prepared snapshot belongs to environment \"{}\", not \"{env_name}\"",
            prepared.env_name
        ));
    }
    let meta = get_environment(&env_name, env, cwd)?;
    let (service_enabled, service_running) =
        service_state.unwrap_or((meta.service_enabled, meta.service_running));
    let env_paths = derive_env_paths(Path::new(&meta.root));
    if env_paths.root != prepared.source_root {
        return Err(format!(
            "environment root changed after snapshot preparation: expected {}, found {}",
            display_path(&prepared.source_root),
            display_path(&env_paths.root)
        ));
    }
    if !path_exists(&env_paths.root) {
        return Err(format!(
            "environment root does not exist: {}",
            display_path(&env_paths.root)
        ));
    }

    if let Some(scope) = &prepared.upgrade_scope {
        if scope.independent_paths != meta.upgrade_independent_paths {
            return Err(
                "independent upgrade paths changed after checkpoint preparation".to_string(),
            );
        }
        validate_upgrade_independent_paths(&meta, &scope.independent_paths, env)?;
    }

    let created_at = now_utc();
    let snapshot_id = format!(
        "{}-{:09}",
        created_at.unix_timestamp(),
        created_at.nanosecond()
    );
    let checkpoint_path = snapshot_checkpoint_path(&env_name, &snapshot_id, env, cwd)?;
    let meta_path = snapshot_meta_path(&env_name, &snapshot_id, env, cwd)?;

    let cleanup = prepared.cleanup_guard();
    let result = (|| {
        let storage_kind =
            create_tree_checkpoint_from_preparation(prepared.checkpoint, &checkpoint_path)?;
        if let Some(scope) = &prepared.upgrade_scope {
            super::checkpoint_scope::validate_scoped_artifact(
                &checkpoint_path,
                &scope.independent_paths,
            )?;
        }
        let snapshot = EnvSnapshotMeta {
            kind: if prepared.upgrade_scope.is_some() {
                UPGRADE_CHECKPOINT_KIND
            } else {
                "ocm-env-snapshot"
            }
            .to_string(),
            upgrade_scope: prepared.upgrade_scope,
            id: snapshot_id,
            env_name: meta.name.clone(),
            label: options.label,
            archive_path: display_path(&checkpoint_path),
            storage_kind,
            source_root: meta.root.clone(),
            gateway_port: meta.gateway_port,
            gateway_port_auto_assigned: meta.gateway_port_auto_assigned,
            service_enabled,
            service_running,
            default_runtime: meta.default_runtime.clone(),
            default_launcher: meta.default_launcher.clone(),
            protected: meta.protected,
            created_at,
        };
        write_json(&meta_path, &snapshot)?;
        Ok(snapshot)
    })();

    if let Err(error) = result {
        let cleanup_result = cleanup.retire(&checkpoint_path);
        let _ = fs::remove_file(&meta_path);
        return Err(match cleanup_result {
            Ok(()) => error,
            Err(cleanup_error) => format!("{error}; {cleanup_error}"),
        });
    }

    result
}

pub fn get_env_snapshot(
    env_name: &str,
    snapshot_id: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvSnapshotMeta, String> {
    let safe_env_name = validate_name(env_name, "Environment name")?;
    let safe_snapshot_id = validate_name(snapshot_id, "Snapshot id")?;

    let path = snapshot_meta_path(&safe_env_name, &safe_snapshot_id, env, cwd)?;
    if !path_exists(&path) {
        return Err(format!(
            "snapshot \"{}\" does not exist for environment \"{}\"",
            safe_snapshot_id, safe_env_name
        ));
    }
    let snapshot = read_json(&path)?;
    validate_env_snapshot_identity(&snapshot, &safe_env_name, &safe_snapshot_id, env, cwd)?;
    Ok(snapshot)
}

pub fn summarize_snapshot(meta: &EnvSnapshotMeta) -> EnvSnapshotSummary {
    EnvSnapshotSummary {
        id: meta.id.clone(),
        env_name: meta.env_name.clone(),
        label: meta.label.clone(),
        archive_path: meta.archive_path.clone(),
        storage_kind: meta.storage_kind.clone(),
        upgrade_scope: meta.upgrade_scope.clone(),
        source_root: meta.source_root.clone(),
        gateway_port: meta.gateway_port,
        service_enabled: meta.service_enabled,
        service_running: meta.service_running,
        default_runtime: meta.default_runtime.clone(),
        default_launcher: meta.default_launcher.clone(),
        protected: meta.protected,
        created_at: meta.created_at,
    }
}

pub(crate) fn ensure_restore_preserves_dev_sources(
    target: &EnvMeta,
    independent: &[PathBuf],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    super::with_locked_environments(env, cwd, |envs| {
        let protected = |meta: &EnvMeta| {
            meta.dev
                .as_ref()
                .is_some_and(|dev| meta.name != target.name || dev.borrowed_source_root().is_some())
        };
        if !envs.iter().any(protected) {
            return Ok(());
        }
        let target_root = Path::new(&target.root);
        if !independent.is_empty() {
            super::checkpoint_scope::validate_ancestors(target_root, independent)?;
        }
        // A full restore renames the root entry itself, including a final
        // symlink. Resolve its parent without following that final entry.
        let parent = fs::canonicalize(target_root.parent().unwrap_or(target_root))
            .map_err(|error| error.to_string())?;
        let root = parent.join(target_root.file_name().unwrap_or_default());
        let root_identity = match fs::symlink_metadata(&root) {
            Ok(_) => Some(path_identity(&root)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let relative_to_root = |path: &Path| -> Result<Option<PathBuf>, String> {
            if let Ok(relative) = path.strip_prefix(&root) {
                return Ok(Some(relative.to_path_buf()));
            }
            // Compare known path ancestors for filesystem aliases, including
            // macOS firmlinks. Never resolve or inspect an exclusion's contents.
            if let Some(identity) = root_identity {
                for ancestor in path.ancestors() {
                    if path_identity(ancestor)? == identity {
                        return Ok(Some(path.strip_prefix(ancestor).unwrap().to_path_buf()));
                    }
                }
            }
            Ok(None)
        };
        for meta in envs.iter().filter(|meta| protected(meta)) {
            let Some(footprint) = registered_dev_source_footprint(meta, envs).map_err(|error| {
                format!("cannot protect dev source for env {}: {error}", meta.name)
            })?
            else {
                continue;
            };
            let replacement_error = |path: &Path| {
                format!(
                    "restoring env {} would replace registered dev source or Git metadata for env {} at {}; choose a checkpoint that preserves the complete source and Git metadata",
                    target.name,
                    meta.name,
                    display_path(path)
                )
            };
            for (path, contents) in footprint
                .entries
                .iter()
                .map(|path| (path, false))
                .chain(footprint.content_roots.iter().map(|path| (path, true)))
            {
                let affected = if let Some(relative) = relative_to_root(path)? {
                    !independent.iter().any(|excluded| {
                        relative.starts_with(excluded)
                            || (!contents && excluded.starts_with(&relative))
                    })
                } else {
                    contents
                        && contains_existing(
                            path,
                            if root_identity.is_some() {
                                &root
                            } else {
                                &parent
                            },
                        )?
                };
                if affected {
                    return Err(replacement_error(path));
                }
            }
            for path in &footprint.reserved_roots {
                let affected = if let Some(relative) = projected_path_relative(&root, path)? {
                    !independent
                        .iter()
                        .any(|excluded| relative.starts_with(excluded))
                } else {
                    projected_path_contains(path, &root)?
                };
                if affected {
                    return Err(replacement_error(path));
                }
            }
        }
        Ok(())
    })
}

pub fn restore_env_snapshot(
    options: RestoreEnvSnapshotOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvSnapshotRestoreSummary, String> {
    let _operation_lock = super::lock_environment_operation(&options.env_name, env, cwd)?;
    let transaction = prepare_env_snapshot_restore(options, env, cwd)?;
    Ok(commit_env_snapshot_restore(transaction))
}

pub(crate) fn prepare_env_snapshot_restore(
    options: RestoreEnvSnapshotOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvSnapshotRestoreTransaction, String> {
    prepare_env_snapshot_restore_with_binding(options, true, env, cwd)
}

pub(crate) fn prepare_upgrade_snapshot_restore(
    options: RestoreEnvSnapshotOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvSnapshotRestoreTransaction, String> {
    prepare_env_snapshot_restore_with_binding(options, false, env, cwd)
}

fn prepare_env_snapshot_restore_with_binding(
    options: RestoreEnvSnapshotOptions,
    preserve_current_dev_execution: bool,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvSnapshotRestoreTransaction, String> {
    let env_name = validate_name(&options.env_name, "Environment name")?;
    let snapshot = get_env_snapshot(&env_name, &options.snapshot_id, env, cwd)?;
    crate::env::EnvironmentService::new(env, cwd)
        .ensure_source_watch_allows_state_mutation_locked(&env_name)?;
    let current = get_environment(&env_name, env, cwd)?;
    let current_paths = derive_env_paths(Path::new(&current.root));
    let root_exists = path_exists(&current_paths.root);
    let independent = snapshot
        .upgrade_scope
        .as_ref()
        .map(|scope| scope.independent_paths.clone())
        .unwrap_or_default();
    ensure_restore_preserves_dev_sources(&current, &independent, env, cwd)?;
    if !independent.is_empty() {
        super::checkpoint_scope::validate_ancestors(&current_paths.root, &independent)?;
        super::checkpoint_scope::validate_scoped_artifact(
            Path::new(&snapshot.archive_path),
            &independent,
        )?;
    }

    let operation = create_restore_operation_namespace(&current_paths.root)?;
    let staging_dir = operation.legacy_staging_root.clone();
    let candidate_root = operation.candidate_root.clone();
    let backup_root = operation.backup_root.clone();

    let result = (|| {
        let (mut restored, legacy_archive) = if snapshot.storage_kind == STORAGE_TAR_ARCHIVE {
            let extracted = extract_env_archive::<EnvArchiveMetadata>(
                Path::new(&snapshot.archive_path),
                &staging_dir,
            )?;
            if extracted.metadata.kind != "ocm-env-archive" {
                return Err(format!(
                    "unsupported archive kind: {}",
                    extracted.metadata.kind
                ));
            }
            if extracted.metadata.format_version != 1 {
                return Err(format!(
                    "unsupported archive format version: {}",
                    extracted.metadata.format_version
                ));
            }
            copy_path_recursive(&extracted.root_dir, &candidate_root)?;
            let archived = extracted.metadata.env;
            (
                EnvMeta {
                    upgrade_independent_paths: current.upgrade_independent_paths.clone(),
                    kind: "ocm-env".to_string(),
                    name: current.name.clone(),
                    root: current.root.clone(),
                    gateway_port: archived.gateway_port,
                    gateway_port_auto_assigned: archived.gateway_port_auto_assigned,
                    service_enabled: archived.service_enabled,
                    service_running: archived.service_running,
                    default_runtime: archived.default_runtime,
                    default_launcher: archived.default_launcher,
                    dev: current.dev.clone(),
                    dev_ui_port: current.dev_ui_port,
                    protected: archived.protected,
                    created_at: current.created_at,
                    updated_at: current.updated_at,
                    last_used_at: current.last_used_at,
                },
                true,
            )
        } else {
            copy_tree_checkpoint(Path::new(&snapshot.archive_path), &candidate_root)?;
            clear_snapshot_runtime_residue(&candidate_root)?;
            (
                EnvMeta {
                    upgrade_independent_paths: current.upgrade_independent_paths.clone(),
                    kind: "ocm-env".to_string(),
                    name: current.name.clone(),
                    root: current.root.clone(),
                    gateway_port: snapshot.gateway_port,
                    gateway_port_auto_assigned: snapshot.gateway_port_auto_assigned,
                    service_enabled: snapshot.service_enabled,
                    service_running: snapshot.service_running,
                    default_runtime: snapshot.default_runtime.clone(),
                    default_launcher: snapshot.default_launcher.clone(),
                    dev: current.dev.clone(),
                    dev_ui_port: current.dev_ui_port,
                    protected: snapshot.protected,
                    created_at: current.created_at,
                    updated_at: current.updated_at,
                    last_used_at: current.last_used_at,
                },
                false,
            )
        };

        if preserve_current_dev_execution && current.dev.is_some() {
            restored.default_runtime = current.default_runtime.clone();
            restored.default_launcher = current.default_launcher.clone();
        }

        let mut renamed = false;
        if !independent.is_empty() {
            super::checkpoint_scope::replace_owned_entries(
                &current_paths.root,
                &candidate_root,
                &backup_root,
                &independent,
            )?;
            renamed = true;
        } else if root_exists {
            fs::rename(&current_paths.root, &backup_root).map_err(|error| error.to_string())?;
            renamed = true;
        }

        let restore_result = (|| {
            if independent.is_empty() {
                fs::rename(&candidate_root, &current_paths.root)
                    .map_err(|error| error.to_string())?;
            }
            if legacy_archive && !restore_uses_directory_link(&current_paths.root)? {
                rewrite_openclaw_config_for_target(
                    &current_paths,
                    Some(Path::new(&snapshot.source_root)),
                    restored.gateway_port,
                )?;
                let known_envs = list_environments(env, cwd)?;
                let audit = audit_openclaw_state(&restored, &known_envs, env);
                if audit.repair_runtime_state {
                    clear_nonportable_runtime_state(
                        &current_paths,
                        env,
                        OpenClawWorkspaceRuntime::for_env(&restored.name, restored.gateway_port),
                    )?;
                }
                if renamed {
                    preserve_current_excluded_openclaw_state(&backup_root, &current_paths.root)?;
                }
            }
            restored = save_environment(restored, env, cwd)?;
            Ok(restored.clone())
        })();

        match restore_result {
            Ok(meta) => Ok(EnvSnapshotRestoreTransaction {
                summary: EnvSnapshotRestoreSummary {
                    env_name: meta.name.clone(),
                    snapshot_id: snapshot.id,
                    label: snapshot.label,
                    root: meta.root.clone(),
                    archive_path: snapshot.archive_path,
                    storage_kind: snapshot.storage_kind,
                    default_runtime: meta.default_runtime.clone(),
                    default_launcher: meta.default_launcher.clone(),
                    protected: meta.protected,
                    warnings: Vec::new(),
                },
                original: current.clone(),
                operation_root: operation.root.clone(),
                displaced_root: renamed.then_some(backup_root.clone()),
                rejected_root: operation.rejected_root.clone(),
                restored_root: current_paths.root.clone(),
                independent: independent.clone(),
            }),
            Err(error) => {
                match reinstate_displaced_environment_root(
                    &current_paths.root,
                    renamed.then_some(backup_root.as_path()),
                    &operation.rejected_root,
                    current.clone(),
                    &independent,
                    env,
                    cwd,
                ) {
                    Ok(()) => match remove_tree_if_present(&operation.root) {
                        Ok(()) => Err(error),
                        Err(cleanup_error) => Err(format!(
                            "{error}; the displaced environment root was restored, but rejected restore cleanup failed at {}: {cleanup_error}",
                            display_path(&operation.root)
                        )),
                    },
                    Err(rollback_error) => Err(format!(
                        "{error}; restoring the displaced environment root also failed: {rollback_error}"
                    )),
                }
            }
        }
    })();

    if result.is_err() && !path_exists(&backup_root) {
        let _ = remove_tree_if_present(&operation.root);
    }
    result
}

pub(crate) fn commit_env_snapshot_restore(
    mut transaction: EnvSnapshotRestoreTransaction,
) -> EnvSnapshotRestoreSummary {
    // Acceptance is complete. Discarding displaced bytes cannot undo it or
    // prevent the caller from recovering the restored service.
    if let Err(error) = remove_tree_if_present(&transaction.operation_root) {
        transaction.summary.warnings.push(format!(
            "restored snapshot was accepted; cleanup requires attention at {}: {error}",
            display_path(&transaction.operation_root)
        ));
    }
    transaction.summary
}

pub(crate) fn rollback_env_snapshot_restore(
    transaction: EnvSnapshotRestoreTransaction,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Option<String>, String> {
    reinstate_displaced_environment_root(
        &transaction.restored_root,
        transaction.displaced_root.as_deref(),
        &transaction.rejected_root,
        transaction.original,
        &transaction.independent,
        env,
        cwd,
    )?;
    Ok(
        remove_tree_if_present(&transaction.operation_root)
            .err()
            .map(|error| {
                format!(
                    "displaced environment root was restored, but its operation namespace {} could not be removed: {error}",
                    display_path(&transaction.operation_root)
                )
            }),
    )
}

fn reinstate_displaced_environment_root(
    restored_root: &Path,
    displaced_root: Option<&Path>,
    rejected_root: &Path,
    original: EnvMeta,
    independent: &[PathBuf],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    if !independent.is_empty() {
        let displaced =
            displaced_root.ok_or("scoped restore is missing displaced owned entries")?;
        super::checkpoint_scope::replace_owned_entries(
            restored_root,
            displaced,
            rejected_root,
            independent,
        )?;
        if let Err(error) = save_environment(original, env, cwd) {
            super::checkpoint_scope::replace_owned_entries(
                restored_root,
                rejected_root,
                displaced,
                independent,
            )
            .map_err(|compensation| {
                format!("{error}; scoped restore compensation failed: {compensation}")
            })?;
            return Err(error);
        }
        return Ok(());
    }
    fs::rename(restored_root, rejected_root).map_err(|error| {
        format!(
            "failed to retain rejected restored root {} at {}: {error}",
            display_path(restored_root),
            display_path(rejected_root)
        )
    })?;

    if let Some(displaced_root) = displaced_root
        && let Err(error) = fs::rename(displaced_root, restored_root)
    {
        let compensation = fs::rename(rejected_root, restored_root)
            .err()
            .map(|compensation_error| {
                format!(
                    "; restoring the rejected root after that failure also failed: {compensation_error}"
                )
            })
            .unwrap_or_default();
        return Err(format!(
            "failed to atomically restore displaced environment root {} from {}: {error}{compensation}",
            display_path(restored_root),
            display_path(displaced_root)
        ));
    }

    if let Err(error) = save_environment(original, env, cwd) {
        let mut compensation_errors = Vec::new();
        if let Some(displaced_root) = displaced_root
            && let Err(compensation_error) = fs::rename(restored_root, displaced_root)
        {
            compensation_errors.push(format!(
                "failed to move the displaced root back to {}: {compensation_error}",
                display_path(displaced_root)
            ));
        }
        if !path_exists(restored_root)
            && let Err(compensation_error) = fs::rename(rejected_root, restored_root)
        {
            compensation_errors.push(format!(
                "failed to restore the rejected root at {}: {compensation_error}",
                display_path(restored_root)
            ));
        }
        return Err(if compensation_errors.is_empty() {
            format!(
                "failed to restore the environment registry entry; the restored root was reinstated: {error}"
            )
        } else {
            format!(
                "failed to restore the environment registry entry: {error}; rollback compensation also failed: {}",
                compensation_errors.join("; ")
            )
        });
    }
    Ok(())
}

fn preserve_current_excluded_openclaw_state(
    backup_root: &Path,
    restored_root: &Path,
) -> Result<(), String> {
    let source_root = backup_root.join(".openclaw");
    let entries = match fs::read_dir(&source_root) {
        Ok(entries) => entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };

    let destination_root = restored_root.join(".openclaw");
    for entry in entries {
        let name = entry.file_name();
        let source = entry.path();
        let destination = destination_root.join(&name);
        let metadata = fs::symlink_metadata(&source).map_err(|error| error.to_string())?;
        if should_discard_current_openclaw_restore_entry(&name, metadata.is_dir()) {
            continue;
        }

        let current_state_wins = matches!(name.to_str(), Some("secrets") | Some("browser"));
        if !current_state_wins && path_exists(&destination) {
            continue;
        }

        remove_path_if_present(&destination)?;
        copy_path_recursive(&source, &destination).map_err(|error| {
            format!(
                "failed to preserve current excluded OpenClaw state at {} while restoring snapshot: {error}",
                display_path(&source)
            )
        })?;
    }
    Ok(())
}

fn should_discard_current_openclaw_restore_entry(name: &OsStr, is_dir: bool) -> bool {
    if is_dir
        && matches!(
            name.to_str(),
            Some("run") | Some("tmp") | Some("temp") | Some("locks")
        )
    {
        return true;
    }

    matches!(
        Path::new(name).extension().and_then(OsStr::to_str),
        Some("pid") | Some("lock") | Some("sock") | Some("socket")
    ) || matches!(
        name.to_str(),
        Some("pid")
            | Some("lock")
            | Some("sock")
            | Some("socket")
            | Some("gateway-supervisor-restart-handoff.json")
    )
}

fn restore_uses_directory_link(root: &Path) -> Result<bool, String> {
    // Captured links do not grant ownership of their live destinations.
    for path in [root.to_path_buf(), root.join(".openclaw")] {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(false)
}

fn clear_snapshot_runtime_residue(root: &Path) -> Result<(), String> {
    let openclaw_root = root.join(".openclaw");
    if restore_uses_directory_link(root)? || !path_exists(&openclaw_root) {
        return Ok(());
    }
    let entries = match fs::read_dir(&openclaw_root) {
        Ok(entries) => entries
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).map_err(|error| error.to_string())?;
        if should_discard_current_openclaw_restore_entry(&entry.file_name(), metadata.is_dir()) {
            remove_path_if_present(&entry_path)?;
        }
    }
    Ok(())
}

fn remove_path_if_present(path: &Path) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };

    if metadata.is_dir() {
        fs::remove_dir_all(path).map_err(|error| error.to_string())
    } else {
        fs::remove_file(path).map_err(|error| error.to_string())
    }
}

pub fn remove_env_snapshot(
    options: RemoveEnvSnapshotOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvSnapshotRemoveSummary, String> {
    let snapshot = get_env_snapshot(&options.env_name, &options.snapshot_id, env, cwd)?;
    let meta_path = snapshot_meta_path(&snapshot.env_name, &snapshot.id, env, cwd)?;
    let artifact_path = PathBuf::from(&snapshot.archive_path);
    let staging_dir = snapshot_removal_staging_dir(&snapshot.env_name, &snapshot.id, env, cwd)?;
    let staged_meta_path = staging_dir.join("snapshot.json");
    let staged_artifact_path = staging_dir.join("artifact");

    fs::create_dir(&staging_dir).map_err(|error| {
        format!(
            "failed to create snapshot removal staging directory {}: {error}",
            display_path(&staging_dir)
        )
    })?;

    if let Err(error) = fs::rename(&meta_path, &staged_meta_path) {
        let _ = fs::remove_dir(&staging_dir);
        return Err(format!(
            "failed to stage snapshot metadata {} for removal: {error}",
            display_path(&meta_path),
        ));
    }

    if path_exists(&artifact_path)
        && let Err(error) = fs::rename(&artifact_path, &staged_artifact_path)
    {
        let restore_error = fs::rename(&staged_meta_path, &meta_path).err();
        if restore_error.is_none() {
            let _ = fs::remove_dir(&staging_dir);
        }
        return Err(format!(
            "failed to stage snapshot artifact {} for removal: {error}{}",
            display_path(&artifact_path),
            restore_error
                .map(|restore_error| {
                    format!("; restoring the staged metadata also failed: {restore_error}")
                })
                .unwrap_or_default()
        ));
    }

    let mut warnings = Vec::new();
    if let Err(error) =
        remove_upgrade_recovery_for_snapshot(&snapshot.env_name, &snapshot.id, env, cwd)
    {
        warnings.push(format!(
            "linked upgrade recovery cleanup failed after snapshot removal: {error}"
        ));
    }
    if let Err(error) = fs::remove_dir_all(&staging_dir) {
        warnings.push(format!(
            "staged snapshot artifact cleanup failed at {}: {error}",
            display_path(&staging_dir)
        ));
    }
    if let Err(error) = remove_snapshot_parent_if_empty(&snapshot.env_name, env, cwd) {
        warnings.push(format!(
            "snapshot directory cleanup failed after removal: {error}"
        ));
    }

    Ok(EnvSnapshotRemoveSummary {
        env_name: snapshot.env_name,
        snapshot_id: snapshot.id,
        label: snapshot.label,
        archive_path: snapshot.archive_path,
        storage_kind: snapshot.storage_kind,
        warnings,
    })
}

pub fn list_env_snapshots(
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Vec<EnvSnapshotMeta>, String> {
    let safe_env_name = validate_name(env_name, "Environment name")?;
    let dir = snapshot_env_dir(&safe_env_name, env, cwd)?;
    let files = load_json_files(&dir)?;
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let snapshot_id = file
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("snapshot path has an invalid id: {}", display_path(&file)))?;
        let snapshot = read_json(&file)?;
        validate_env_snapshot_identity(&snapshot, &safe_env_name, snapshot_id, env, cwd)?;
        out.push(snapshot);
    }
    sort_snapshots(&mut out);
    Ok(out)
}

pub fn list_all_env_snapshots(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Vec<EnvSnapshotMeta>, String> {
    let stores = super::ensure_store(env, cwd)?;
    let mut out = Vec::new();
    let entries = fs::read_dir(&stores.snapshots_dir).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let env_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                format!(
                    "snapshot directory has an invalid environment name: {}",
                    display_path(&path)
                )
            })?;
        let env_name = validate_name(env_name, "Environment name")?;
        let files = load_json_files(&path)?;
        for file in files {
            let snapshot_id = file
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    format!("snapshot path has an invalid id: {}", display_path(&file))
                })?;
            let snapshot = read_json(&file)?;
            validate_env_snapshot_identity(&snapshot, &env_name, snapshot_id, env, cwd)?;
            out.push(snapshot);
        }
    }
    sort_snapshots(&mut out);
    Ok(out)
}

fn validate_env_snapshot_identity(
    snapshot: &EnvSnapshotMeta,
    env_name: &str,
    snapshot_id: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    match (snapshot.kind.as_str(), &snapshot.upgrade_scope) {
        ("ocm-env-snapshot", None) => {}
        (UPGRADE_CHECKPOINT_KIND, Some(scope)) if snapshot.storage_kind != STORAGE_TAR_ARCHIVE => {
            super::checkpoint_scope::validate_independent_paths(&scope.independent_paths)?;
        }
        _ => {
            return Err(format!(
                "unsupported snapshot kind or scope: {}",
                snapshot.kind
            ));
        }
    }
    if snapshot.env_name != env_name {
        return Err(format!(
            "snapshot \"{snapshot_id}\" belongs to environment \"{}\", expected \"{env_name}\"",
            snapshot.env_name
        ));
    }
    if snapshot.id != snapshot_id {
        return Err(format!(
            "snapshot entry \"{snapshot_id}\" contains snapshot id \"{}\"",
            snapshot.id
        ));
    }
    let expected_artifact = if snapshot.storage_kind == STORAGE_TAR_ARCHIVE {
        snapshot_archive_path(env_name, snapshot_id, env, cwd)?
    } else if matches!(
        snapshot.storage_kind.as_str(),
        super::checkpoints::STORAGE_APFS_CLONE | super::checkpoints::STORAGE_FULL_COPY
    ) {
        snapshot_checkpoint_path(env_name, snapshot_id, env, cwd)?
    } else {
        return Err(format!(
            "unsupported snapshot storage kind: {}",
            snapshot.storage_kind
        ));
    };
    if Path::new(&snapshot.archive_path) != expected_artifact {
        return Err(format!(
            "snapshot \"{snapshot_id}\" artifact path is {}, expected {}",
            snapshot.archive_path,
            display_path(&expected_artifact)
        ));
    }
    Ok(())
}

fn sort_snapshots(snapshots: &mut [EnvSnapshotMeta]) {
    snapshots.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.id.cmp(&left.id))
    });
}

fn create_restore_operation_namespace(root: &Path) -> Result<RestoreOperationNamespace, String> {
    let parent = root.parent().unwrap_or_else(|| Path::new("."));
    let env_name = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("env");
    let prefix = format!(".{env_name}-ocm-restore-");
    let operation_root = tempfile::Builder::new()
        .prefix(&prefix)
        .tempdir_in(parent)
        .map_err(|error| {
            format!(
                "failed to create an exclusive restore namespace beside {}: {error}",
                display_path(root)
            )
        })?
        .keep();
    Ok(RestoreOperationNamespace {
        candidate_root: operation_root.join("candidate"),
        backup_root: operation_root.join("displaced"),
        legacy_staging_root: operation_root.join("legacy"),
        rejected_root: operation_root.join("rejected"),
        root: operation_root,
    })
}

fn snapshot_removal_staging_dir(
    env_name: &str,
    snapshot_id: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let id = NEXT_REMOVAL_ID.fetch_add(1, Ordering::Relaxed);
    Ok(snapshot_env_dir(env_name, env, cwd)?
        .join(format!(".remove-{snapshot_id}-{}-{id}", std::process::id())))
}

fn remove_snapshot_parent_if_empty(
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    let dir = snapshot_env_dir(env_name, env, cwd)?;
    if !path_exists(&dir) {
        return Ok(());
    }

    let mut entries = fs::read_dir(&dir).map_err(|error| error.to_string())?;
    if entries.next().is_none() {
        fs::remove_dir(&dir).map_err(|error| error.to_string())?;
    }
    Ok(())
}
