use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::env::{
    CloneEnvironmentOptions, CloneEnvironmentResult, CreateEnvironmentOptions, EnvExportSummary,
    EnvImportSummary, EnvMeta, ExportEnvironmentOptions, ImportEnvironmentOptions,
    ImportEnvironmentResult,
};
use crate::infra::archive::{
    ArchivedEnvMeta, EnvArchiveMetadata, extract_env_archive, write_env_archive_with_options,
};
use crate::openclaw_repo::remove_openclaw_worktree;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;

use super::common::{
    copy_dir_recursive, copy_path, ensure_dir, path_exists, read_json, write_json,
};
use super::gateway_ports::{
    DEFAULT_GATEWAY_PORT, choose_available_gateway_port, resolve_effective_gateway_ports,
    resolve_env_gateway_port,
};
use super::layout::{
    clean_path, default_env_root, derive_env_paths, display_path, env_registry_path,
    resolve_absolute_path, resolve_store_paths, validate_name,
};
use super::now_utc;
use super::{
    OpenClawWorkspaceRuntime, clear_nonportable_runtime_state,
    normalize_new_environment_sandbox_origin, openclaw_config_include_paths,
    openclaw_config_uses_includes, openclaw_env_archive_options,
    reject_include_owned_agent_workspaces, reject_include_owned_sandbox_origin,
    resolve_env_openclaw_workspaces, rewrite_identity_bound_workspace_paths_for_target,
    rewrite_openclaw_config_for_new_environment, rewrite_openclaw_config_for_simulation,
    rewrite_openclaw_config_includes_for_target,
};

static NEXT_IMPORT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvRegistry {
    kind: String,
    envs: Vec<EnvMeta>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    service_policy_revisions: BTreeMap<String, u64>,
}

pub(crate) struct EnvironmentServicePolicyChange {
    pub(crate) applied: EnvMeta,
    applied_revision: u64,
    previous_service_enabled: bool,
    previous_service_running: bool,
}

fn empty_env_registry() -> EnvRegistry {
    EnvRegistry {
        kind: "ocm-env-registry".to_string(),
        envs: Vec::new(),
        service_policy_revisions: BTreeMap::new(),
    }
}

fn load_env_registry(env: &BTreeMap<String, String>, cwd: &Path) -> Result<EnvRegistry, String> {
    let path = env_registry_path(env, cwd)?;
    if !path_exists(&path) {
        return Ok(empty_env_registry());
    }

    let mut registry: EnvRegistry = read_json(&path)?;
    registry.kind = "ocm-env-registry".to_string();
    registry
        .envs
        .sort_by(|left, right| left.name.cmp(&right.name));
    Ok(registry)
}

fn write_env_registry(
    registry: &mut EnvRegistry,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    registry.kind = "ocm-env-registry".to_string();
    registry
        .envs
        .sort_by(|left, right| left.name.cmp(&right.name));
    let path = env_registry_path(env, cwd)?;
    write_json(&path, &registry)
}

pub(crate) struct EnvRegistryLock {
    file: File,
}

pub(crate) struct EnvironmentOperationLock {
    file: File,
}

impl Drop for EnvRegistryLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

impl Drop for EnvironmentOperationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub(crate) fn lock_environment_operation(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvironmentOperationLock, String> {
    // Every env binding, service, snapshot, and guarded-destroy mutation shares
    // this lock. Bypassing it can make an accepted destroy token stale.
    let safe_name = validate_name(name, "Environment name")?;
    let lock_dir = resolve_store_paths(env, cwd)?
        .home
        .join("locks")
        .join("environments");
    ensure_dir(&lock_dir)?;
    let lock_path = lock_dir.join(format!("{safe_name}.lock"));
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| {
            format!(
                "failed to open environment operation lock {}: {error}",
                display_path(&lock_path)
            )
        })?;
    file.lock_exclusive().map_err(|error| {
        format!(
            "failed to lock environment operation {}: {error}",
            display_path(&lock_path)
        )
    })?;
    Ok(EnvironmentOperationLock { file })
}

pub(crate) fn lock_env_registry(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvRegistryLock, String> {
    // Keep load/allocate/write under one cross-process lock. Locking only the
    // final rename loses concurrent entries and can assign duplicate ports.
    let registry_path = env_registry_path(env, cwd)?;
    let parent = registry_path
        .parent()
        .ok_or_else(|| "environment registry has no parent directory".to_string())?;
    ensure_dir(parent)?;
    let lock_path = registry_path.with_extension("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| {
            format!(
                "failed to open environment registry lock {}: {error}",
                display_path(&lock_path)
            )
        })?;
    file.lock_exclusive().map_err(|error| {
        format!(
            "failed to lock environment registry {}: {error}",
            display_path(&lock_path)
        )
    })?;
    Ok(EnvRegistryLock { file })
}

fn normalize_environment(mut meta: EnvMeta) -> Result<EnvMeta, String> {
    meta.name = validate_name(&meta.name, "Environment name")?;
    meta.kind = "ocm-env".to_string();
    meta.root = display_path(&clean_path(Path::new(&meta.root)));
    meta.updated_at = now_utc();
    Ok(meta)
}

fn canonicalize_launcher_binding(
    mut meta: EnvMeta,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    if let Some(launcher_name) = meta.default_launcher.as_deref() {
        let launcher = super::launchers::get_launcher(launcher_name, env, cwd)?;
        meta.default_launcher = Some(launcher.name);
    }
    Ok(meta)
}

fn upsert_environment(registry: &mut EnvRegistry, meta: EnvMeta) -> Result<EnvMeta, String> {
    let meta = normalize_environment(meta)?;
    registry.envs.retain(|entry| entry.name != meta.name);
    registry.envs.push(meta.clone());
    Ok(meta)
}

fn find_environment(registry: &EnvRegistry, name: &str) -> Option<EnvMeta> {
    registry.envs.iter().find(|meta| meta.name == name).cloned()
}

fn bump_service_policy_revision(registry: &mut EnvRegistry, name: &str) -> u64 {
    let revision = registry
        .service_policy_revisions
        .entry(name.to_string())
        .or_default();
    *revision = revision.saturating_add(1);
    *revision
}

pub fn list_environments(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Vec<EnvMeta>, String> {
    Ok(load_env_registry(env, cwd)?.envs)
}

pub fn get_environment(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let safe_name = validate_name(name, "Environment name")?;
    load_env_registry(env, cwd)?
        .envs
        .into_iter()
        .find(|meta| meta.name == safe_name)
        .ok_or_else(|| format!("environment \"{safe_name}\" does not exist"))
}

pub fn save_environment(
    mut meta: EnvMeta,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let service = crate::env::EnvironmentService::new(env, cwd);
    let _admission_lock = service.lock_gateway_admission(&meta.name)?;
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    let policy_changed = find_environment(&registry, &meta.name).is_some_and(|current| {
        current.service_enabled != meta.service_enabled
            || current.service_running != meta.service_running
    });
    if policy_changed && meta.service_enabled && meta.service_running {
        service.ensure_source_watch_allows_service(&meta.name)?;
    }
    meta = upsert_environment(&mut registry, meta)?;
    if policy_changed {
        bump_service_policy_revision(&mut registry, &meta.name);
    }
    write_env_registry(&mut registry, env, cwd)?;

    Ok(meta)
}

pub(crate) fn save_environment_with_validated_launcher(
    mut meta: EnvMeta,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    meta = canonicalize_launcher_binding(meta, env, cwd)?;
    meta = upsert_environment(&mut registry, meta)?;
    write_env_registry(&mut registry, env, cwd)?;

    Ok(meta)
}

pub(crate) fn save_environment_with_validated_runtime(
    mut meta: EnvMeta,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    if let Some(runtime_name) = meta.default_runtime.as_deref() {
        meta.default_runtime =
            Some(super::runtimes::get_runtime_verified(runtime_name, env, cwd)?.name);
    }
    meta = upsert_environment(&mut registry, meta)?;
    write_env_registry(&mut registry, env, cwd)?;

    Ok(meta)
}

pub(crate) fn set_environment_service_policy(
    name: &str,
    service_enabled: Option<bool>,
    service_running: Option<bool>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvironmentServicePolicyChange, String> {
    let safe_name = validate_name(name, "Environment name")?;
    let service = crate::env::EnvironmentService::new(env, cwd);
    let _admission_lock = service.lock_gateway_admission(&safe_name)?;
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    let (applied, previous_service_enabled, previous_service_running) = {
        let meta = registry
            .envs
            .iter_mut()
            .find(|meta| meta.name == safe_name)
            .ok_or_else(|| format!("environment \"{safe_name}\" does not exist"))?;
        let previous_service_enabled = meta.service_enabled;
        let previous_service_running = meta.service_running;
        if let Some(service_enabled) = service_enabled {
            meta.service_enabled = service_enabled;
        }
        if let Some(service_running) = service_running {
            meta.service_running = service_running;
        }
        if meta.service_enabled && meta.service_running {
            service.ensure_source_watch_allows_service(&safe_name)?;
        }
        meta.updated_at = now_utc();
        (
            meta.clone(),
            previous_service_enabled,
            previous_service_running,
        )
    };
    let applied_revision = bump_service_policy_revision(&mut registry, &safe_name);
    write_env_registry(&mut registry, env, cwd)?;
    Ok(EnvironmentServicePolicyChange {
        applied,
        applied_revision,
        previous_service_enabled,
        previous_service_running,
    })
}

pub(crate) fn restore_environment_service_policy(
    change: &EnvironmentServicePolicyChange,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<bool, String> {
    let service = crate::env::EnvironmentService::new(env, cwd);
    let _admission_lock = service.lock_gateway_admission(&change.applied.name)?;
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    let current_revision = registry
        .service_policy_revisions
        .get(&change.applied.name)
        .copied()
        .unwrap_or_default();
    if current_revision != change.applied_revision {
        return Ok(false);
    }
    let meta = registry
        .envs
        .iter_mut()
        .find(|meta| meta.name == change.applied.name)
        .ok_or_else(|| format!("environment \"{}\" does not exist", change.applied.name))?;
    meta.service_enabled = change.previous_service_enabled;
    meta.service_running = change.previous_service_running;
    meta.updated_at = now_utc();
    bump_service_policy_revision(&mut registry, &change.applied.name);
    write_env_registry(&mut registry, env, cwd)?;
    Ok(true)
}

pub fn create_environment(
    options: CreateEnvironmentOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    create_environment_with_runtime_validation(options, false, env, cwd)
}

pub(crate) fn create_environment_with_validated_runtime(
    options: CreateEnvironmentOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    create_environment_with_runtime_validation(options, true, env, cwd)
}

fn create_environment_with_runtime_validation(
    options: CreateEnvironmentOptions,
    validate_runtime: bool,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let name = validate_name(&options.name, "Environment name")?;
    let service = crate::env::EnvironmentService::new(env, cwd);
    let _admission_lock = service.lock_gateway_admission(&name)?;
    if options.service_enabled && options.service_running {
        service.ensure_source_watch_allows_service(&name)?;
    }
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    if find_environment(&registry, &name).is_some() {
        return Err(format!("environment \"{name}\" already exists"));
    }
    let default_runtime = if validate_runtime {
        options
            .default_runtime
            .as_deref()
            .map(|runtime_name| super::runtimes::get_runtime_verified(runtime_name, env, cwd))
            .transpose()?
            .map(|runtime| runtime.name)
    } else {
        options.default_runtime
    };
    let default_launcher = options
        .default_launcher
        .as_deref()
        .map(|launcher_name| super::launchers::get_launcher(launcher_name, env, cwd))
        .transpose()?
        .map(|launcher| launcher.name);

    let root = if let Some(root) = options.root.as_deref() {
        resolve_absolute_path(root, env, cwd)?
    } else {
        default_env_root(&name, env, cwd)?
    };

    let paths = derive_env_paths(&root);
    if path_exists(&paths.root) {
        let mut entries = fs::read_dir(&paths.root).map_err(|error| error.to_string())?;
        if entries.next().is_some() {
            return Err(format!(
                "root already exists and is not empty: {}",
                display_path(&paths.root)
            ));
        }
    }

    ensure_dir(&paths.root)?;
    ensure_dir(&paths.state_dir)?;
    ensure_dir(&paths.workspace_dir)?;

    let gateway_port_auto_assigned = options.gateway_port.is_none();
    let gateway_port = options.gateway_port.or_else(|| {
        Some(choose_available_gateway_port(
            DEFAULT_GATEWAY_PORT,
            &registry.envs,
            env,
        ))
    });
    let created_at = now_utc();
    let meta = EnvMeta {
        upgrade_independent_paths: Vec::new(),
        kind: "ocm-env".to_string(),
        name,
        root: display_path(&paths.root),
        gateway_port,
        gateway_port_auto_assigned,
        service_enabled: options.service_enabled,
        service_running: options.service_running,
        default_runtime,
        default_launcher,
        dev: options.dev,
        protected: options.protected,
        created_at,
        updated_at: created_at,
        last_used_at: None,
    };
    let meta = upsert_environment(&mut registry, meta)?;
    write_env_registry(&mut registry, env, cwd)?;
    Ok(meta)
}

pub fn clone_environment(
    options: CloneEnvironmentOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    Ok(
        clone_environment_with_policy(options, None, CloneEnvironmentPolicy::Standard, env, cwd)?
            .meta,
    )
}

pub(crate) fn clone_environment_with_sandbox_origin(
    options: CloneEnvironmentOptions,
    sandbox_origin: Option<&str>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<CloneEnvironmentResult, String> {
    clone_environment_with_policy(
        options,
        sandbox_origin,
        CloneEnvironmentPolicy::Standard,
        env,
        cwd,
    )
}

pub(crate) fn clone_environment_for_simulation(
    options: CloneEnvironmentOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    Ok(
        clone_environment_with_policy(options, None, CloneEnvironmentPolicy::Simulation, env, cwd)?
            .meta,
    )
}

#[derive(Clone, Copy)]
enum CloneEnvironmentPolicy {
    Standard,
    Simulation,
}

fn clone_environment_with_policy(
    options: CloneEnvironmentOptions,
    sandbox_origin: Option<&str>,
    policy: CloneEnvironmentPolicy,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<CloneEnvironmentResult, String> {
    let sandbox_origin = normalize_new_environment_sandbox_origin(sandbox_origin)?;
    let source_name = validate_name(&options.source_name, "Environment name")?;
    let name = validate_name(&options.name, "Environment name")?;
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    let source = find_environment(&registry, &source_name)
        .ok_or_else(|| format!("environment \"{source_name}\" does not exist"))?;
    if find_environment(&registry, &name).is_some() {
        return Err(format!("environment \"{name}\" already exists"));
    }

    let root = if let Some(root) = options.root.as_deref() {
        resolve_absolute_path(root, env, cwd)?
    } else {
        default_env_root(&name, env, cwd)?
    };
    let target_paths = derive_env_paths(&root);
    if path_exists(&target_paths.root) {
        let mut entries = fs::read_dir(&target_paths.root).map_err(|error| error.to_string())?;
        if entries.next().is_some() {
            return Err(format!(
                "root already exists and is not empty: {}",
                display_path(&target_paths.root)
            ));
        }
    }

    let source_paths = derive_env_paths(Path::new(&source.root));
    if !path_exists(&source_paths.root) {
        return Err(format!(
            "environment root does not exist: {}",
            display_path(&source_paths.root)
        ));
    }
    if matches!(policy, CloneEnvironmentPolicy::Standard) {
        reject_include_owned_sandbox_origin(&source_paths.config_path)?;
        reject_include_owned_agent_workspaces(&source_paths.config_path)?;
    }
    let source_workspaces = resolve_env_openclaw_workspaces(
        &source_paths,
        env,
        OpenClawWorkspaceRuntime::for_env(&source.name, source.gateway_port),
    )?;
    source_workspaces.archive_relative_roots(&source_paths.root)?;
    let result = (|| {
        copy_dir_recursive(&source_paths.root, &target_paths.root)?;
        let created_at = now_utc();
        let gateway_port = choose_cloned_gateway_port(&source, &registry.envs, env);
        let target_runtime = OpenClawWorkspaceRuntime::for_env(&name, Some(gateway_port));
        let config_rewrite = match policy {
            CloneEnvironmentPolicy::Standard => {
                reject_include_owned_sandbox_origin(&target_paths.config_path)?;
                let outcome = rewrite_openclaw_config_for_new_environment(
                    &target_paths,
                    Some(&source_paths.root),
                    Some(gateway_port),
                    sandbox_origin.as_deref(),
                )?;
                rewrite_identity_bound_workspace_paths_for_target(
                    &target_paths.config_path,
                    &source_workspaces,
                    &source_paths.root,
                    &target_paths.root,
                )?;
                verify_copied_workspaces_remain_preserved(
                    &source_workspaces,
                    &source_paths.root,
                    &target_paths,
                    env,
                    target_runtime,
                )?;
                clear_nonportable_runtime_state(&target_paths, env, target_runtime)?;
                outcome
            }
            CloneEnvironmentPolicy::Simulation
                if openclaw_config_uses_includes(&target_paths.config_path)? =>
            {
                rewrite_openclaw_config_for_simulation(
                    &target_paths,
                    Some(&source_paths.root),
                    gateway_port,
                )?;
                rewrite_openclaw_config_includes_for_target(
                    &target_paths.config_path,
                    &target_paths.state_dir,
                    &source_paths.root,
                    &target_paths.root,
                )?;
                verify_copied_workspaces_remain_preserved(
                    &source_workspaces,
                    &source_paths.root,
                    &target_paths,
                    env,
                    target_runtime,
                )?;
                clear_simulation_runtime_state_preserving_includes(
                    &target_paths,
                    env,
                    target_runtime,
                )?;
                Default::default()
            }
            CloneEnvironmentPolicy::Simulation => {
                let outcome = rewrite_openclaw_config_for_new_environment(
                    &target_paths,
                    Some(&source_paths.root),
                    Some(gateway_port),
                    None,
                )?;
                rewrite_identity_bound_workspace_paths_for_target(
                    &target_paths.config_path,
                    &source_workspaces,
                    &source_paths.root,
                    &target_paths.root,
                )?;
                verify_copied_workspaces_remain_preserved(
                    &source_workspaces,
                    &source_paths.root,
                    &target_paths,
                    env,
                    target_runtime,
                )?;
                clear_nonportable_runtime_state(&target_paths, env, target_runtime)?;
                outcome
            }
        };

        let meta = EnvMeta {
            upgrade_independent_paths: Vec::new(),
            kind: "ocm-env".to_string(),
            name,
            root: display_path(&target_paths.root),
            gateway_port: Some(gateway_port),
            gateway_port_auto_assigned: false,
            service_enabled: false,
            service_running: false,
            default_runtime: source.default_runtime,
            default_launcher: source.default_launcher,
            dev: None,
            protected: source.protected,
            created_at,
            updated_at: created_at,
            last_used_at: None,
        };
        let meta = upsert_environment(&mut registry, meta)?;
        write_env_registry(&mut registry, env, cwd)?;
        Ok(CloneEnvironmentResult {
            meta,
            cleared_sandbox_origin: config_rewrite.cleared_sandbox_origin,
            sandbox_port: config_rewrite.sandbox_port,
        })
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&target_paths.root);
    }

    result
}

fn clear_simulation_runtime_state_preserving_includes(
    target_paths: &super::layout::EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<(), String> {
    let include_paths =
        openclaw_config_include_paths(&target_paths.config_path, &target_paths.state_dir)?;
    if include_paths.is_empty() {
        clear_nonportable_runtime_state(target_paths, env, runtime)?;
        return Ok(());
    }

    let staging_root = create_private_simulation_staging_dir(&target_paths.root)?;
    let result = (|| {
        for include_path in &include_paths {
            let relative = include_path
                .strip_prefix(&target_paths.state_dir)
                .map_err(|error| error.to_string())?;
            copy_path(include_path, &staging_root.join(relative))?;
        }
        clear_nonportable_runtime_state(target_paths, env, runtime)?;
        for include_path in &include_paths {
            let relative = include_path
                .strip_prefix(&target_paths.state_dir)
                .map_err(|error| error.to_string())?;
            match fs::symlink_metadata(include_path) {
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
            copy_path(&staging_root.join(relative), include_path)?;
        }
        Ok(())
    })();
    let cleanup = fs::remove_dir_all(&staging_root);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error.to_string())
        }
        (Ok(()), _) => Ok(()),
    }
}

fn verify_copied_workspaces_remain_preserved(
    source_workspaces: &super::openclaw_workspaces::OpenClawWorkspaceInventory,
    source_root: &Path,
    target_paths: &super::layout::EnvPaths,
    env: &BTreeMap<String, String>,
    target_runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<(), String> {
    let target_workspaces = resolve_env_openclaw_workspaces(target_paths, env, target_runtime)?;
    for source_workspace in source_workspaces.workspace_roots() {
        let Ok(relative) = source_workspace.strip_prefix(source_root) else {
            continue;
        };
        let copied_workspace = clean_path(&target_paths.root.join(relative));
        if path_exists(&copied_workspace) && !target_workspaces.contains(&copied_workspace) {
            return Err(format!(
                "rewritten OpenClaw workspace configuration would stop preserving copied workspace data at {}; flatten or make the workspace path independent of changing environment identity",
                display_path(&copied_workspace)
            ));
        }
    }
    Ok(())
}

fn create_private_simulation_staging_dir(parent: &Path) -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    for attempt in 0..16 {
        let path = parent.join(format!(
            ".ocm-simulation-includes-{}-{nonce}-{}-{attempt}",
            std::process::id(),
            NEXT_IMPORT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let result = {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(&path)
        };
        match result {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("failed to create private simulation include staging directory".to_string())
}

fn choose_cloned_gateway_port(
    source: &EnvMeta,
    envs: &[EnvMeta],
    env: &BTreeMap<String, String>,
) -> u32 {
    let effective_ports = resolve_effective_gateway_ports(envs, env);
    let preferred = effective_ports
        .get(&source.name)
        .copied()
        .or_else(|| resolve_env_gateway_port(source))
        .unwrap_or(DEFAULT_GATEWAY_PORT);

    choose_available_gateway_port(preferred, envs, env)
}

pub fn export_environment(
    options: ExportEnvironmentOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvExportSummary, String> {
    let meta = get_environment(&options.name, env, cwd)?;
    let env_paths = derive_env_paths(Path::new(&meta.root));
    if !path_exists(&env_paths.root) {
        return Err(format!(
            "environment root does not exist: {}",
            display_path(&env_paths.root)
        ));
    }

    let output_path = if let Some(output) = options.output.as_deref() {
        resolve_absolute_path(output, env, cwd)?
    } else {
        clean_path(&cwd.join(format!("{}.ocm-env.tar", meta.name)))
    };
    if path_exists(&output_path) {
        return Err(format!(
            "export archive already exists: {}",
            display_path(&output_path)
        ));
    }

    let metadata = EnvArchiveMetadata {
        kind: "ocm-env-archive".to_string(),
        format_version: 1,
        exported_at: now_utc(),
        env: ArchivedEnvMeta {
            name: meta.name.clone(),
            source_root: Some(meta.root.clone()),
            gateway_port: meta.gateway_port,
            gateway_port_auto_assigned: meta.gateway_port_auto_assigned,
            service_enabled: meta.service_enabled,
            service_running: meta.service_running,
            default_runtime: meta.default_runtime.clone(),
            default_launcher: meta.default_launcher.clone(),
            protected: meta.protected,
            created_at: meta.created_at,
            updated_at: meta.updated_at,
            last_used_at: meta.last_used_at,
        },
    };

    let result = write_env_archive_with_options(
        &metadata,
        &env_paths.root,
        &output_path,
        openclaw_env_archive_options(
            &env_paths,
            env,
            OpenClawWorkspaceRuntime::for_env(&meta.name, meta.gateway_port),
        )?,
    );
    if result.is_err() {
        let _ = fs::remove_file(&output_path);
    }
    result?;

    Ok(EnvExportSummary {
        name: meta.name,
        root: meta.root,
        archive_path: display_path(&output_path),
        default_runtime: meta.default_runtime,
        default_launcher: meta.default_launcher,
        protected: meta.protected,
    })
}

pub fn import_environment(
    options: ImportEnvironmentOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvImportSummary, String> {
    Ok(import_environment_with_sandbox_origin(options, None, env, cwd)?.summary)
}

pub(crate) fn import_environment_with_sandbox_origin(
    options: ImportEnvironmentOptions,
    sandbox_origin: Option<&str>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<ImportEnvironmentResult, String> {
    let sandbox_origin = normalize_new_environment_sandbox_origin(sandbox_origin)?;
    let archive_path = resolve_absolute_path(&options.archive, env, cwd)?;
    let staging_dir = import_staging_dir();
    if path_exists(&staging_dir) {
        let _ = fs::remove_dir_all(&staging_dir);
    }

    let result = (|| {
        let extracted = extract_env_archive::<EnvArchiveMetadata>(&archive_path, &staging_dir)?;
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

        let source_name = extracted.metadata.env.name.clone();
        let name = if let Some(name) = options.name.as_deref() {
            validate_name(name, "Environment name")?
        } else {
            validate_name(&source_name, "Environment name")?
        };
        let _lock = lock_env_registry(env, cwd)?;
        let mut registry = load_env_registry(env, cwd)?;
        if find_environment(&registry, &name).is_some() {
            return Err(format!("environment \"{name}\" already exists"));
        }

        let root = if let Some(root) = options.root.as_deref() {
            resolve_absolute_path(root, env, cwd)?
        } else {
            default_env_root(&name, env, cwd)?
        };
        let target_paths = derive_env_paths(&root);
        if path_exists(&target_paths.root) {
            let mut entries =
                fs::read_dir(&target_paths.root).map_err(|error| error.to_string())?;
            if entries.next().is_some() {
                return Err(format!(
                    "root already exists and is not empty: {}",
                    display_path(&target_paths.root)
                ));
            }
        }

        if !path_exists(&extracted.root_dir) {
            return Err("archive is missing root/".to_string());
        }
        let extracted_paths = derive_env_paths(&extracted.root_dir);
        reject_include_owned_sandbox_origin(&extracted_paths.config_path)?;
        reject_include_owned_agent_workspaces(&extracted_paths.config_path)?;
        let extracted_workspaces = resolve_env_openclaw_workspaces(
            &extracted_paths,
            env,
            OpenClawWorkspaceRuntime::for_env(&source_name, extracted.metadata.env.gateway_port),
        )?;
        let imported = (|| {
            let preferred_gateway_port = extracted
                .metadata
                .env
                .gateway_port
                .unwrap_or(DEFAULT_GATEWAY_PORT);
            let gateway_port =
                choose_available_gateway_port(preferred_gateway_port, &registry.envs, env);
            let target_runtime = OpenClawWorkspaceRuntime::for_env(&name, Some(gateway_port));
            copy_dir_recursive(&extracted.root_dir, &target_paths.root)?;
            reject_include_owned_sandbox_origin(&target_paths.config_path)?;
            let config_rewrite = rewrite_openclaw_config_for_new_environment(
                &target_paths,
                extracted.metadata.env.source_root.as_deref().map(Path::new),
                Some(gateway_port),
                sandbox_origin.as_deref(),
            )?;
            rewrite_identity_bound_workspace_paths_for_target(
                &target_paths.config_path,
                &extracted_workspaces,
                &extracted.root_dir,
                &target_paths.root,
            )?;
            verify_copied_workspaces_remain_preserved(
                &extracted_workspaces,
                &extracted.root_dir,
                &target_paths,
                env,
                target_runtime,
            )?;
            clear_nonportable_runtime_state(&target_paths, env, target_runtime)?;

            let created_at = now_utc();
            let meta = EnvMeta {
                upgrade_independent_paths: Vec::new(),
                kind: "ocm-env".to_string(),
                name: name.clone(),
                root: display_path(&target_paths.root),
                gateway_port: Some(gateway_port),
                gateway_port_auto_assigned: false,
                service_enabled: false,
                service_running: false,
                default_runtime: extracted.metadata.env.default_runtime.clone(),
                default_launcher: extracted.metadata.env.default_launcher.clone(),
                dev: None,
                protected: extracted.metadata.env.protected,
                created_at,
                updated_at: created_at,
                last_used_at: None,
            };
            let meta = upsert_environment(&mut registry, meta)?;
            write_env_registry(&mut registry, env, cwd)?;
            Ok((meta, config_rewrite))
        })();

        match imported {
            Ok((meta, config_rewrite)) => Ok(ImportEnvironmentResult {
                summary: EnvImportSummary {
                    name: meta.name.clone(),
                    source_name,
                    root: meta.root.clone(),
                    archive_path: display_path(&archive_path),
                    default_runtime: meta.default_runtime.clone(),
                    default_launcher: meta.default_launcher.clone(),
                    protected: meta.protected,
                },
                cleared_sandbox_origin: config_rewrite.cleared_sandbox_origin,
                sandbox_port: config_rewrite.sandbox_port,
            }),
            Err(error) => {
                let _ = fs::remove_dir_all(&target_paths.root);
                Err(error)
            }
        }
    })();

    let _ = fs::remove_dir_all(&staging_dir);
    result
}

pub fn remove_environment(
    name: &str,
    force: bool,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let _operation = lock_environment_operation(name, env, cwd)?;
    remove_environment_locked(name, force, env, cwd)
}

pub(crate) fn remove_environment_locked(
    name: &str,
    force: bool,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let safe_name = validate_name(name, "Environment name")?;
    let source_service = crate::env::EnvironmentService::new(env, cwd);
    let _admission = source_service.lock_gateway_admission(&safe_name)?;
    source_service.ensure_source_watch_stopped(&safe_name)?;
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    let meta = find_environment(&registry, &safe_name)
        .ok_or_else(|| format!("environment \"{safe_name}\" does not exist"))?;
    if meta.protected && !force {
        return Err(format!(
            "environment \"{}\" is protected; re-run with --force",
            meta.name
        ));
    }

    let paths = derive_env_paths(Path::new(&meta.root));

    if let Some(dev) = meta.dev.as_ref() {
        remove_openclaw_worktree(Path::new(&dev.repo_root), Path::new(&dev.worktree_root))?;
    }

    if path_exists(&paths.root) {
        fs::remove_dir_all(&paths.root).map_err(|error| error.to_string())?;
    }

    source_service.remove_stopped_source_watch_state_locked(&safe_name)?;
    registry.envs.retain(|entry| entry.name != meta.name);
    // Keep a monotonic tombstone so a stale rollback token cannot target a
    // later environment that reuses the same name.
    bump_service_policy_revision(&mut registry, &meta.name);
    write_env_registry(&mut registry, env, cwd)?;

    Ok(meta)
}

pub(crate) fn with_locked_environments<T>(
    env: &BTreeMap<String, String>,
    cwd: &Path,
    action: impl FnOnce(&[EnvMeta]) -> Result<T, String>,
) -> Result<T, String> {
    let _lock = lock_env_registry(env, cwd)?;
    let registry = load_env_registry(env, cwd)?;
    action(&registry.envs)
}

fn import_staging_dir() -> PathBuf {
    let id = NEXT_IMPORT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join("ocm-env-imports")
        .join(format!("{}-{id}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use crate::env::CreateEnvironmentOptions;
    use serde_json::{Value, json};

    use super::{
        CloneEnvironmentOptions, NEXT_IMPORT_ID, OpenClawWorkspaceRuntime, Ordering,
        clear_simulation_runtime_state_preserving_includes, clone_environment_for_simulation,
        create_environment, create_private_simulation_staging_dir, derive_env_paths,
        get_environment, remove_environment, restore_environment_service_policy, save_environment,
        set_environment_service_policy,
    };

    #[test]
    fn service_policy_rollback_requires_the_applied_snapshot_to_still_be_current() {
        let id = NEXT_IMPORT_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join("ocm-env-policy-tests")
            .join(format!("{}-{id}", std::process::id()));
        let cwd = root.join("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let env = BTreeMap::from([
            ("HOME".to_string(), root.join("home").display().to_string()),
            (
                "OCM_HOME".to_string(),
                root.join("ocm-home").display().to_string(),
            ),
        ]);
        create_environment(
            CreateEnvironmentOptions {
                name: "demo".to_string(),
                root: None,
                gateway_port: None,
                service_enabled: false,
                service_running: false,
                default_runtime: None,
                default_launcher: None,
                dev: None,
                protected: false,
            },
            &env,
            &cwd,
        )
        .unwrap();

        let change =
            set_environment_service_policy("demo", Some(true), Some(true), &env, &cwd).unwrap();
        let mut concurrent = get_environment("demo", &env, &cwd).unwrap();
        concurrent.default_runtime = Some("stable".to_string());
        save_environment(concurrent, &env, &cwd).unwrap();

        assert!(restore_environment_service_policy(&change, &env, &cwd).unwrap());
        let current = get_environment("demo", &env, &cwd).unwrap();
        assert!(!current.service_enabled);
        assert!(!current.service_running);
        assert_eq!(current.default_runtime.as_deref(), Some("stable"));

        let change =
            set_environment_service_policy("demo", Some(true), Some(true), &env, &cwd).unwrap();
        let same_policy =
            set_environment_service_policy("demo", Some(true), Some(true), &env, &cwd).unwrap();
        assert!(!restore_environment_service_policy(&change, &env, &cwd).unwrap());
        let current = get_environment("demo", &env, &cwd).unwrap();
        assert!(current.service_enabled);
        assert!(current.service_running);

        remove_environment("demo", false, &env, &cwd).unwrap();
        create_environment(
            CreateEnvironmentOptions {
                name: "demo".to_string(),
                root: None,
                gateway_port: None,
                service_enabled: false,
                service_running: false,
                default_runtime: None,
                default_launcher: None,
                dev: None,
                protected: false,
            },
            &env,
            &cwd,
        )
        .unwrap();
        assert!(!restore_environment_service_policy(&same_policy, &env, &cwd).unwrap());
        let recreated = get_environment("demo", &env, &cwd).unwrap();
        assert!(!recreated.service_enabled);
        assert!(!recreated.service_running);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn simulation_clone_overlays_identity_and_preserves_included_config_files() {
        let id = NEXT_IMPORT_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join("ocm-env-simulation-tests")
            .join(format!("{}-{id}", std::process::id()));
        let cwd = root.join("workspace");
        fs::create_dir_all(&cwd).unwrap();
        let env = BTreeMap::from([
            ("HOME".to_string(), root.join("home").display().to_string()),
            (
                "OCM_HOME".to_string(),
                root.join("ocm-home").display().to_string(),
            ),
        ]);
        let source = create_environment(
            CreateEnvironmentOptions {
                name: "source".to_string(),
                root: None,
                gateway_port: Some(19_789),
                service_enabled: false,
                service_running: false,
                default_runtime: None,
                default_launcher: None,
                dev: None,
                protected: false,
            },
            &env,
            &cwd,
        )
        .unwrap();
        let source_paths = derive_env_paths(Path::new(&source.root));
        let include_dir = source_paths.state_dir.join("config");
        fs::create_dir_all(&include_dir).unwrap();
        fs::write(
            &source_paths.config_path,
            serde_json::to_string_pretty(&json!({"$include": "./config/base.json"})).unwrap()
                + "\n",
        )
        .unwrap();
        fs::write(
            include_dir.join("base.json"),
            format!(
                r#"{{
                  // OpenClaw include files support JSON5.
                  gateway: {{ port: 19789 }},
                  agents: {{
                    defaults: {{ workspace: "{}" }},
                    list: [{{ id: "main", default: true, workspace: "{}" }}],
                  }},
                  mcp: {{ apps: {{
                    sandboxPort: 19790,
                    sandboxOrigin: "https://source.example.test",
                  }} }},
                  nested: {{ $include: "./nested.json5" }},
                }}
"#,
                source_paths.workspace_dir.display(),
                source_paths.workspace_dir.display()
            ),
        )
        .unwrap();
        fs::write(
            include_dir.join("nested.json5"),
            "{\n  // Nested JSON5 includes must be preserved.\n  enabled: true,\n}\n",
        )
        .unwrap();
        fs::create_dir_all(source_paths.state_dir.join("run")).unwrap();
        fs::write(source_paths.state_dir.join("run/gateway.pid"), "123\n").unwrap();
        fs::create_dir_all(source_paths.state_dir.join("agents/main/sessions")).unwrap();
        fs::write(
            source_paths
                .state_dir
                .join("agents/main/sessions/main.jsonl"),
            "{}\n",
        )
        .unwrap();
        fs::create_dir_all(source_paths.state_dir.join("logs")).unwrap();
        fs::write(source_paths.state_dir.join("logs/gateway.log"), "source\n").unwrap();
        fs::write(source_paths.state_dir.join("openclaw.json.bak"), "{}\n").unwrap();

        let simulation = clone_environment_for_simulation(
            CloneEnvironmentOptions {
                source_name: "source".to_string(),
                name: "simulation".to_string(),
                root: None,
            },
            &env,
            &cwd,
        )
        .unwrap();

        let target_paths = derive_env_paths(Path::new(&simulation.root));
        let config: Value =
            serde_json::from_str(&fs::read_to_string(&target_paths.config_path).unwrap()).unwrap();
        let gateway_port = simulation.gateway_port.unwrap();
        assert_eq!(config["$include"], "./config/base.json");
        assert_eq!(config["gateway"]["port"], gateway_port);
        assert_eq!(
            config["agents"]["defaults"]["workspace"],
            target_paths.workspace_dir.display().to_string()
        );
        assert_eq!(config["mcp"]["apps"]["sandboxPort"], gateway_port + 1);
        assert_eq!(
            config["mcp"]["apps"]["sandboxOrigin"],
            format!("http://127.0.0.1:{}", gateway_port + 1)
        );
        assert!(target_paths.state_dir.join("config/base.json").exists());
        assert!(target_paths.state_dir.join("config/nested.json5").exists());
        let included: Value = serde_json::from_str(
            &fs::read_to_string(target_paths.state_dir.join("config/base.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            included["agents"]["list"][0]["workspace"],
            target_paths.workspace_dir.display().to_string()
        );
        assert!(!target_paths.state_dir.join("run").exists());
        assert!(!target_paths.state_dir.join("agents/main/sessions").exists());
        assert!(!target_paths.state_dir.join("logs").exists());
        assert!(!target_paths.state_dir.join("openclaw.json.bak").exists());
        assert!(source_paths.state_dir.join("config/base.json").exists());
        assert!(
            fs::read_to_string(source_paths.state_dir.join("config/base.json"))
                .unwrap()
                .contains(&source_paths.workspace_dir.display().to_string())
        );

        remove_environment("simulation", false, &env, &cwd).unwrap();
        remove_environment("source", false, &env, &cwd).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn simulation_cleanup_rejects_escaping_include_symlinks_without_mutation() {
        let id = NEXT_IMPORT_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join("ocm-env-simulation-symlink-tests")
            .join(format!("{}-{id}", std::process::id()));
        let paths = derive_env_paths(root.join("target"));
        fs::create_dir_all(&paths.workspace_dir).unwrap();
        fs::create_dir_all(paths.state_dir.join("logs")).unwrap();
        fs::write(paths.state_dir.join("logs/gateway.log"), "remove\n").unwrap();
        let external = root.join("external/include.json");
        fs::create_dir_all(external.parent().unwrap()).unwrap();
        fs::write(&external, "{\"shared\": true}\n").unwrap();
        std::os::unix::fs::symlink(&external, paths.workspace_dir.join("include.json")).unwrap();
        fs::write(
            &paths.config_path,
            "{\"$include\":\"./workspace/include.json\"}\n",
        )
        .unwrap();

        let error = clear_simulation_runtime_state_preserving_includes(
            &paths,
            &BTreeMap::new(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap_err();
        assert!(
            error.contains("OpenClaw $include path resolves outside the config directory"),
            "{error}"
        );

        assert_eq!(
            fs::read_to_string(&external).unwrap(),
            "{\"shared\": true}\n"
        );
        assert!(
            fs::symlink_metadata(paths.workspace_dir.join("include.json"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(paths.state_dir.join("logs/gateway.log").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn simulation_include_staging_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let id = NEXT_IMPORT_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir()
            .join("ocm-env-simulation-staging-tests")
            .join(format!("{}-{id}", std::process::id()));
        fs::create_dir_all(&root).unwrap();

        let staging = create_private_simulation_staging_dir(&root).unwrap();

        assert_eq!(
            fs::metadata(&staging).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir_all(root).unwrap();
    }
}
