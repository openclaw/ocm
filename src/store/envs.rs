use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::env::{
    CloneEnvironmentOptions, CloneEnvironmentResult, CreateEnvironmentOptions, EnvExportSummary,
    EnvImportSummary, EnvMeta, ExportEnvironmentOptions, ImportEnvironmentOptions,
    ImportEnvironmentResult,
};
use crate::infra::archive::{
    ArchivedEnvMeta, EnvArchiveMetadata, extract_env_archive, write_env_archive_from_resolved_root,
};
use crate::openclaw_repo::remove_openclaw_worktree;
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;

use super::common::{
    copy_dir_recursive, copy_dir_with_sqlite_snapshots, copy_path, ensure_dir, path_exists,
    read_json, write_json,
};
use super::dev_sources::{DevSourceRemoval, ensure_root_outside_dev_sources};
use super::gateway_ports::{
    DEFAULT_GATEWAY_PORT, choose_available_gateway_port, resolve_effective_gateway_ports,
    resolve_env_gateway_port,
};
use super::layout::{
    clean_path, default_env_root, derive_env_paths, display_path, env_registry_path,
    resolve_absolute_path, resolve_store_paths, validate_name,
};
use super::now_utc;
use super::openclaw_state::{
    openclaw_env_state_archive_options, validate_nonportable_runtime_state_paths,
};
use super::{
    DevSourceRegistration, OpenClawWorkspaceRuntime, normalize_new_environment_sandbox_origin,
    openclaw_config_include_paths, openclaw_config_uses_includes, openclaw_env_archive_options,
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
    let mut registry: EnvRegistry = if path_exists(&path) {
        read_json(&path)?
    } else {
        empty_env_registry()
    };
    super::dev_ui_ports::hydrate(&mut registry.envs, env, cwd)?;
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

pub(crate) type EnvRegistryLock = super::common::ExclusiveFileLock;
pub(crate) type EnvironmentOperationLock = super::common::ExclusiveFileLock;

pub(super) fn environment_operation_lock_path(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    // Every env binding, service, snapshot, and guarded-destroy mutation shares
    // this lock. Bypassing it can make an accepted destroy token stale.
    let safe_name = validate_name(name, "Environment name")?;
    Ok(resolve_store_paths(env, cwd)?
        .home
        .join("locks")
        .join("environments")
        .join(format!("{safe_name}.lock")))
}

pub(crate) fn lock_environment_operation(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvironmentOperationLock, String> {
    super::lock_file(
        &environment_operation_lock_path(name, env, cwd)?,
        "environment operation",
    )
}

pub(crate) fn try_lock_environment_operation(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Option<EnvironmentOperationLock>, String> {
    super::try_lock_file(
        &environment_operation_lock_path(name, env, cwd)?,
        "environment operation",
    )
}

pub(crate) fn lock_env_registry(
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvRegistryLock, String> {
    // Keep load/allocate/write under one cross-process lock. Locking only the
    // final rename loses concurrent entries and can assign duplicate ports.
    let registry_path = env_registry_path(env, cwd)?;
    super::lock_file(
        &registry_path.with_extension("lock"),
        "environment registry",
    )
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

fn upsert_environment(
    registry: &mut EnvRegistry,
    meta: EnvMeta,
    runtime_guard: Option<&super::RuntimeMutationGuard>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(EnvMeta, Option<super::RuntimeBindingGuard>), String> {
    let admission = admit_runtime_binding(
        registry,
        &meta.name,
        meta.default_runtime.as_deref(),
        runtime_guard,
        env,
        cwd,
    )?;
    let meta = normalize_environment(meta)?;
    if registry
        .envs
        .iter()
        .find(|current| current.name == meta.name)
        .is_none_or(|current| current.root != meta.root || current.dev != meta.dev)
    {
        // Creation may have initialized a new root at an absent Owned path.
        // Its pre-creation guard already checked Owned sources.
        super::dev_sources::ensure_borrowed_source_isolation(
            &meta.name,
            Path::new(&meta.root),
            meta.dev.as_ref().and_then(|dev| dev.borrowed_source_root()),
            &registry.envs,
        )?;
    }
    registry.envs.retain(|entry| entry.name != meta.name);
    registry.envs.push(meta.clone());
    Ok((meta, admission))
}

fn admit_runtime_binding(
    registry: &EnvRegistry,
    name: &str,
    runtime: Option<&str>,
    held: Option<&super::RuntimeMutationGuard>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Option<super::RuntimeBindingGuard>, String> {
    let Some(runtime) = runtime else {
        return Ok(None);
    };
    if registry
        .envs
        .iter()
        .any(|current| current.name == name && current.default_runtime.as_deref() == Some(runtime))
    {
        return Ok(None);
    }
    // Never wait on a runtime while holding the registry: its owner needs the
    // registry to publish its own binding or complete recovery.
    super::runtimes::try_lock_runtime_binding_with_guard(runtime, held, env, cwd).map(Some)
}

pub(crate) fn lock_environment_runtime_binding(
    meta: &EnvMeta,
    held: Option<&super::RuntimeMutationGuard>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Option<super::RuntimeBindingGuard>, String> {
    let _registry = lock_env_registry(env, cwd)?;
    admit_runtime_binding(
        &load_env_registry(env, cwd)?,
        &meta.name,
        meta.default_runtime.as_deref(),
        held,
        env,
        cwd,
    )
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

pub(crate) fn environment_service_policy_revision(
    name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<u64, String> {
    let name = validate_name(name, "Environment name")?;
    Ok(load_env_registry(env, cwd)?
        .service_policy_revisions
        .get(&name)
        .copied()
        .unwrap_or_default())
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
    meta: EnvMeta,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    save_environment_with_runtime_guard(meta, None, env, cwd)
}

pub(crate) fn save_environment_with_runtime_guard(
    meta: EnvMeta,
    runtime_guard: Option<&super::RuntimeMutationGuard>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let registration = DevSourceRegistration::acquire(&meta.name, meta.dev.as_ref(), env, cwd)?;
    save_environment_registered(meta, &registration, runtime_guard, env, cwd)
}

pub(crate) fn save_environment_with_dev_registration(
    meta: EnvMeta,
    registration: &DevSourceRegistration,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    save_environment_registered(meta, registration, None, env, cwd)
}

fn save_environment_registered(
    meta: EnvMeta,
    registration: &DevSourceRegistration,
    runtime_guard: Option<&super::RuntimeMutationGuard>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let service = crate::env::EnvironmentService::new(env, cwd);
    let _admission_lock = service.lock_gateway_admission(&meta.name)?;
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    registration.recheck(&meta.name, meta.dev.as_ref(), &registry.envs)?;
    let policy_changed = find_environment(&registry, &meta.name).is_some_and(|current| {
        current.service_enabled != meta.service_enabled
            || current.service_running != meta.service_running
    });
    if policy_changed && meta.service_enabled && meta.service_running {
        service.ensure_source_watch_allows_service(&meta.name)?;
    }
    let (meta, _admission) = upsert_environment(&mut registry, meta, runtime_guard, env, cwd)?;
    if policy_changed {
        bump_service_policy_revision(&mut registry, &meta.name);
    }
    write_env_registry(&mut registry, env, cwd)?;

    Ok(meta)
}

/// The caller retains the environment operation and Gateway admission locks.
pub(crate) fn rebind_environment_dev(
    expected: &EnvMeta,
    dev: crate::env::EnvDevMeta,
    registration: &DevSourceRegistration,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    let current = find_environment(&registry, &expected.name)
        .filter(|current| current == expected)
        .ok_or("environment changed during dev rebind; retry the command")?;
    // Even a repeated target must still be the checkout inspected before admission.
    registration.recheck_source(&dev, &registry.envs)?;
    if current.dev.as_ref() == Some(&dev) {
        return Ok(current);
    }
    let mut changed = current;
    changed.dev = Some(dev);
    let (changed, _admission) = upsert_environment(&mut registry, changed, None, env, cwd)?;
    write_env_registry(&mut registry, env, cwd)?;
    Ok(changed)
}

pub(crate) fn save_environment_with_validated_launcher(
    mut meta: EnvMeta,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let registration = DevSourceRegistration::acquire(&meta.name, meta.dev.as_ref(), env, cwd)?;
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    registration.recheck(&meta.name, meta.dev.as_ref(), &registry.envs)?;
    meta = canonicalize_launcher_binding(meta, env, cwd)?;
    let (meta, _admission) = upsert_environment(&mut registry, meta, None, env, cwd)?;
    write_env_registry(&mut registry, env, cwd)?;

    Ok(meta)
}

pub(crate) fn save_environment_with_validated_runtime(
    meta: EnvMeta,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    save_environment_with_validated_runtime_guard(meta, None, env, cwd)
}

pub(crate) fn save_environment_with_validated_runtime_guard(
    mut meta: EnvMeta,
    runtime_guard: Option<&super::RuntimeMutationGuard>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let registration = DevSourceRegistration::acquire(&meta.name, meta.dev.as_ref(), env, cwd)?;
    let _lock = lock_env_registry(env, cwd)?;
    let mut registry = load_env_registry(env, cwd)?;
    registration.recheck(&meta.name, meta.dev.as_ref(), &registry.envs)?;
    let _admitted = admit_runtime_binding(
        &registry,
        &meta.name,
        meta.default_runtime.as_deref(),
        runtime_guard,
        env,
        cwd,
    )?;
    if let Some(runtime_name) = meta.default_runtime.as_deref() {
        meta.default_runtime =
            Some(super::runtimes::get_runtime_verified(runtime_name, env, cwd)?.name);
    }
    let (meta, _admission) = upsert_environment(&mut registry, meta, runtime_guard, env, cwd)?;
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

pub(crate) fn validate_gateway_port(port: u32) -> Result<(), String> {
    if !(1..=u16::MAX as u32).contains(&port) {
        return Err("gateway port must be between 1 and 65535".to_string());
    }
    Ok(())
}

pub fn create_environment(
    options: CreateEnvironmentOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    create_environment_with_runtime_validation(options, false, env, cwd)
}

fn ensure_root_outside_environments(
    name: &str,
    root: &Path,
    envs: &[EnvMeta],
) -> Result<(), String> {
    if envs.is_empty() {
        return Ok(());
    }
    let resolve = |root: &Path| {
        super::dev_registration::registration_path(root).map_err(|error| {
            format!(
                "failed to resolve environment root {}: {error}",
                display_path(root)
            )
        })
    };
    let (target, mut target_entries) = resolve(root)?;
    target_entries.insert(target.clone());
    for current in envs {
        let (registered, mut registered_entries) = resolve(Path::new(&current.root))?;
        registered_entries.insert(registered.clone());
        // Compare both the resolved roots and the links that keep them reachable.
        // A disjoint target reached through a link inside another root is unsafe too.
        for (container, entries) in [
            (&registered, &target_entries),
            (&target, &registered_entries),
        ] {
            for entry in entries {
                if super::dev_sources::projected_path_contains(container, entry)? {
                    return Err(format!(
                        "environment {name} root {} overlaps environment {} root {}; choose a separate environment root",
                        display_path(root),
                        current.name,
                        current.root
                    ));
                }
            }
        }
    }
    Ok(())
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
    if let Some(port) = options.gateway_port {
        validate_gateway_port(port)?;
    }
    let name = validate_name(&options.name, "Environment name")?;
    let registration = DevSourceRegistration::acquire(&name, options.dev.as_ref(), env, cwd)?;
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
    registration.recheck(&name, options.dev.as_ref(), &registry.envs)?;
    let _runtime_admission = admit_runtime_binding(
        &registry,
        &name,
        options.default_runtime.as_deref(),
        None,
        env,
        cwd,
    )?;
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

    ensure_root_outside_dev_sources(&name, &root, &registry.envs)?;
    if let Some(source) = options
        .dev
        .as_ref()
        .and_then(|dev| dev.borrowed_source_root())
    {
        super::dev_sources::ensure_root_outside_borrowed_source(
            &name,
            &root,
            &name,
            Path::new(source),
        )?;
    }
    ensure_root_outside_environments(&name, &root, &registry.envs)?;
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

    let gateway_port_auto_assigned = options.gateway_port.is_none();
    let gateway_port = Some(match options.gateway_port {
        Some(port) => port,
        None => choose_available_gateway_port(DEFAULT_GATEWAY_PORT, &registry.envs, env)?,
    });

    ensure_dir(&paths.root)?;
    ensure_dir(&paths.state_dir)?;
    ensure_dir(&paths.workspace_dir)?;

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
        dev_ui_port: None,
        protected: options.protected,
        created_at,
        updated_at: created_at,
        last_used_at: None,
    };
    if meta.dev.is_some()
        && !super::openclaw_config::initialize_new_dev_openclaw_config(
            &paths,
            gateway_port.expect("new environments have a selected Gateway port"),
        )?
    {
        return Err(format!(
            "OpenClaw config appeared while creating dev env {}; preserving it without registering the env: {}",
            meta.name,
            display_path(&paths.config_path)
        ));
    }
    let (meta, _admission) = upsert_environment(&mut registry, meta, None, env, cwd)?;
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

    let _runtime_admission = admit_runtime_binding(
        &registry,
        &name,
        source.default_runtime.as_deref(),
        None,
        env,
        cwd,
    )?;
    let root = if let Some(root) = options.root.as_deref() {
        resolve_absolute_path(root, env, cwd)?
    } else {
        default_env_root(&name, env, cwd)?
    };
    ensure_root_outside_dev_sources(&name, &root, &registry.envs)?;
    ensure_root_outside_environments(&name, &root, &registry.envs)?;
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
    validate_nonportable_runtime_state_paths(&source_paths)?;
    if matches!(policy, CloneEnvironmentPolicy::Standard) {
        reject_include_owned_sandbox_origin(&source_paths.config_path)?;
        reject_include_owned_agent_workspaces(&source_paths.config_path)?;
    }
    let source_workspaces = super::openclaw_workspaces::resolve_env_openclaw_copy_paths(
        &source_paths,
        env,
        OpenClawWorkspaceRuntime::for_env(&source.name, source.gateway_port),
    )?;
    source_workspaces.archive_relative_roots(&source_paths.root)?;
    let gateway_port = choose_cloned_gateway_port(&source, &registry.envs, env)?;
    let result = (|| {
        copy_dir_with_sqlite_snapshots(
            &source_paths.root,
            &target_paths.root,
            &source_paths.state_dir,
        )?;
        validate_nonportable_runtime_state_paths(&target_paths)?;
        let canonical_source_root =
            fs::canonicalize(&source_paths.root).map_err(|error| error.to_string())?;
        let git_contexts = super::openclaw_state::git_config::copied_workspace_git_contexts(
            &source_paths,
            &target_paths,
            &source_workspaces,
        )?;
        let copied_worktrees = super::openclaw_state::relocate_copied_worktrees(
            &target_paths,
            &[
                (&source_paths.root, &target_paths.root),
                (&canonical_source_root, &target_paths.root),
            ],
            git_contexts,
            &[],
        )?;
        let created_at = now_utc();
        let target_runtime = OpenClawWorkspaceRuntime::for_env(&name, Some(gateway_port));
        if matches!(policy, CloneEnvironmentPolicy::Standard) {
            reject_include_owned_sandbox_origin(&target_paths.config_path)?;
        }
        let mut copied_configs = source_workspaces
            .config_include_paths()
            .map(|path| {
                path.strip_prefix(&source_paths.root)
                    .map(|relative| target_paths.root.join(relative))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !copied_configs.is_empty() {
            // Use the admitted source graph before any target traversal.
            // Include the root so normalized absolute directives use the
            // same mapping as directives inside copied fragments.
            copied_configs.push(target_paths.config_path.clone());
            super::openclaw_config::rewrite_copied_include_directives(
                &copied_configs,
                &[&source_paths.root, &canonical_source_root],
                &target_paths.root,
            )?;
        }
        let config_rewrite = match policy {
            CloneEnvironmentPolicy::Standard => {
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
                    &source_paths.root,
                )?;
                verify_copied_workspaces_remain_preserved(
                    &source_workspaces,
                    &source_paths.root,
                    &target_paths,
                    env,
                    target_runtime,
                    &source_paths.root,
                )?;
                super::openclaw_state::clear_copied_runtime_state_preserving_paths(
                    &target_paths,
                    env,
                    target_runtime,
                    &copied_worktrees.preserved_roots,
                    &copied_worktrees.config_paths,
                )?;
                outcome
            }
            CloneEnvironmentPolicy::Simulation
                if openclaw_config_uses_includes(&target_paths.config_path)? =>
            {
                super::openclaw_config::rewrite_included_agent_directories_for_target(
                    &target_paths.config_path,
                    &source_workspaces,
                    &source_paths.root,
                    &target_paths.root,
                )?;
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
                    &source_paths.root,
                )?;
                clear_simulation_runtime_state_preserving_includes(
                    &target_paths,
                    env,
                    target_runtime,
                    &copied_worktrees.preserved_roots,
                    &copied_worktrees.config_paths,
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
                    &source_paths.root,
                )?;
                verify_copied_workspaces_remain_preserved(
                    &source_workspaces,
                    &source_paths.root,
                    &target_paths,
                    env,
                    target_runtime,
                    &source_paths.root,
                )?;
                super::openclaw_state::clear_copied_runtime_state_preserving_paths(
                    &target_paths,
                    env,
                    target_runtime,
                    &copied_worktrees.preserved_roots,
                    &copied_worktrees.config_paths,
                )?;
                outcome
            }
        };

        super::openclaw_state::relocate_cloned_plugin_index_paths(&source_paths, &target_paths)?;
        let copied_paths = super::openclaw_workspaces::resolve_env_openclaw_copy_paths(
            &target_paths,
            env,
            target_runtime,
        )?;
        // Sessions may store canonical paths even when the environment root
        // was registered through an alias (for example /var on macOS).
        super::openclaw_state::relocate_copied_databases(
            &target_paths,
            &[&source_paths.root, &canonical_source_root],
            super::openclaw_state::DatabasePathRelocation::ClonedSessions,
            Some(&copied_paths),
        )?;
        super::openclaw_state::clear_cloned_agent_database_leases(&target_paths)?;
        copied_worktrees.finish(&target_paths)?;

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
            dev_ui_port: None,
            protected: source.protected,
            created_at,
            updated_at: created_at,
            last_used_at: None,
        };
        let (meta, _admission) = upsert_environment(&mut registry, meta, None, env, cwd)?;
        write_env_registry(&mut registry, env, cwd)?;
        Ok(CloneEnvironmentResult {
            meta,
            cleared_sandbox_origin: config_rewrite.cleared_sandbox_origin,
            sandbox_port: config_rewrite.sandbox_port,
        })
    })();

    if result.is_err() {
        let _ = super::common::remove_owned_tree(&target_paths.root);
    }

    result
}

fn clear_simulation_runtime_state_preserving_includes(
    target_paths: &super::layout::EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
    preserved_roots: &BTreeSet<PathBuf>,
    preserved_files: &BTreeSet<PathBuf>,
) -> Result<(), String> {
    let include_paths =
        openclaw_config_include_paths(&target_paths.config_path, &target_paths.state_dir)?;
    if include_paths.is_empty() {
        super::openclaw_state::clear_copied_runtime_state_preserving_paths(
            target_paths,
            env,
            runtime,
            preserved_roots,
            preserved_files,
        )?;
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
        super::openclaw_state::clear_copied_runtime_state_preserving_paths(
            target_paths,
            env,
            runtime,
            preserved_roots,
            preserved_files,
        )?;
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
    source_agent_root: &Path,
) -> Result<(), String> {
    let resolve_copy_paths = if source_workspaces.agent_directory_roots().next().is_some() {
        super::openclaw_workspaces::resolve_env_openclaw_copy_paths
    } else {
        resolve_env_openclaw_workspaces
    };
    let target_workspaces = resolve_copy_paths(target_paths, env, target_runtime)?;
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
    for (agent_id, source_agent) in source_workspaces.agent_directories() {
        let Ok(relative) = source_agent
            .strip_prefix(source_root)
            .or_else(|_| source_agent.strip_prefix(source_agent_root))
        else {
            continue;
        };
        let copied_agent = clean_path(&target_paths.root.join(relative));
        if path_exists(&copied_agent)
            && target_workspaces.agent_directory(agent_id) != Some(copied_agent.as_path())
        {
            return Err(format!(
                "rewritten OpenClaw agent configuration would stop preserving copied agent data at {}; flatten or make the agent directory independent of changing environment identity",
                display_path(&copied_agent)
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
) -> Result<u32, String> {
    let effective_ports = resolve_effective_gateway_ports(envs, env);
    let preferred = effective_ports
        .get(&source.name)
        .cloned()
        .transpose()?
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

    let runtime = OpenClawWorkspaceRuntime::for_env(&meta.name, meta.gateway_port);
    let archive_options = if options.include_state {
        openclaw_env_state_archive_options(&env_paths)?
    } else {
        openclaw_env_archive_options(&env_paths, env, runtime)?
    };
    let canonical_root = fs::canonicalize(&env_paths.root).map_err(|error| error.to_string())?;
    let metadata = EnvArchiveMetadata {
        include_state: options.include_state,
        kind: "ocm-env-archive".to_string(),
        format_version: 1,
        exported_at: now_utc(),
        env: ArchivedEnvMeta {
            name: meta.name.clone(),
            source_root: Some(meta.root.clone()),
            canonical_source_root: Some(display_path(&canonical_root)),
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

    let result = write_env_archive_from_resolved_root(
        &metadata,
        &canonical_root,
        &output_path,
        archive_options,
    );
    if result.is_err() && !options.include_state {
        let _ = fs::remove_file(&output_path);
    }
    result?;

    Ok(EnvExportSummary {
        include_state: options.include_state,
        name: meta.name,
        root: meta.root,
        archive_path: display_path(&output_path),
        default_runtime: meta.default_runtime,
        default_launcher: meta.default_launcher,
        protected: meta.protected,
    })
}

// These are identities recorded on the exporting host, not paths to open on
// this host. In particular, POSIX import must accept Windows drive/UNC roots.
fn archive_source_root_is_absolute(root: &Path) -> bool {
    let Some(root) = root.to_str().filter(|root| !root.contains('\0')) else {
        return false;
    };
    let bytes = root.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    let unc = root.strip_prefix(r"\\").is_some_and(|rest| {
        let mut parts = rest.split(['/', '\\']);
        parts.next().is_some_and(|part| !part.is_empty())
            && parts.next().is_some_and(|part| !part.is_empty())
    });
    root.starts_with('/') || drive || unc
}

#[cfg(windows)]
fn ordinary_windows_archive_root(root: &Path) -> Option<PathBuf> {
    let root = root.to_str()?;
    if let Some(unc) = root.strip_prefix(r"\\?\UNC\") {
        return Some(PathBuf::from(format!(r"\\{unc}")));
    }
    let drive = root.strip_prefix(r"\\?\")?;
    let bytes = drive.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\'))
    .then(|| PathBuf::from(drive))
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
        let _ = super::common::remove_owned_tree(&staging_dir);
    }

    let result = (|| {
        super::common::ensure_private_dir(&staging_dir)?;
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

        if extracted.metadata.include_state {
            crate::infra::archive::restore_env_archive_directory_permissions(
                &archive_path,
                &staging_dir,
            )?;
        }

        let mut source_roots: Vec<&Path> = extracted
            .metadata
            .env
            .source_root
            .as_deref()
            .map(Path::new)
            .into_iter()
            .collect();
        if let Some(canonical) = extracted.metadata.env.canonical_source_root.as_deref() {
            let canonical = Path::new(canonical);
            if source_roots
                .first()
                .is_none_or(|root| !archive_source_root_is_absolute(root))
                || !archive_source_root_is_absolute(canonical)
                || canonical
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err(
                    "archive canonical source root must be absolute and accompany an absolute sourceRoot"
                        .to_string(),
                );
            }
            source_roots.push(canonical);
        }
        // Rust records extended-length Windows paths, while Node realpath
        // returns ordinary drive/UNC spelling. Both denote the captured root.
        // Only new identity-bearing archives gain these equivalent spellings;
        // legacy archives keep their existing relocation behavior.
        #[cfg(windows)]
        let ordinary_roots: Vec<_> = if extracted.metadata.env.canonical_source_root.is_some() {
            source_roots
                .iter()
                .filter_map(|root| ordinary_windows_archive_root(root))
                .collect()
        } else {
            Vec::new()
        };
        #[cfg(windows)]
        source_roots.extend(ordinary_roots.iter().map(PathBuf::as_path));
        // Match each original reference once, preferring its most specific root.
        source_roots.sort_by_key(|root| std::cmp::Reverse(root.as_os_str().len()));
        source_roots.dedup();
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

        let _runtime_admission = admit_runtime_binding(
            &registry,
            &name,
            extracted.metadata.env.default_runtime.as_deref(),
            None,
            env,
            cwd,
        )?;
        let root = if let Some(root) = options.root.as_deref() {
            resolve_absolute_path(root, env, cwd)?
        } else {
            default_env_root(&name, env, cwd)?
        };
        ensure_root_outside_dev_sources(&name, &root, &registry.envs)?;
        ensure_root_outside_environments(&name, &root, &registry.envs)?;
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
        let config_directory_permissions = if extracted.metadata.include_state {
            match fs::symlink_metadata(&extracted_paths.state_dir) {
                Ok(metadata) if metadata.is_dir() => Some(metadata.permissions()),
                Ok(_) => {
                    return Err("OpenClaw config directory must be a real directory".to_string());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.to_string()),
            }
        } else {
            validate_nonportable_runtime_state_paths(&extracted_paths)?;
            None
        };
        reject_include_owned_sandbox_origin(&extracted_paths.config_path)?;
        reject_include_owned_agent_workspaces(&extracted_paths.config_path)?;
        let extracted_workspaces = super::openclaw_workspaces::resolve_archived_openclaw_paths(
            &extracted_paths,
            &source_roots,
            env,
            OpenClawWorkspaceRuntime::for_env(&source_name, extracted.metadata.env.gateway_port),
            extracted.metadata.include_state,
        )?;
        let preferred_gateway_port = extracted
            .metadata
            .env
            .gateway_port
            .unwrap_or(DEFAULT_GATEWAY_PORT);
        let gateway_port =
            choose_available_gateway_port(preferred_gateway_port, &registry.envs, env)?;
        let imported = (|| {
            let target_runtime = OpenClawWorkspaceRuntime::for_env(&name, Some(gateway_port));
            if extracted.metadata.include_state {
                super::common::copy_dir_preserving_permissions(
                    &extracted.root_dir,
                    &target_paths.root,
                )?;
            } else {
                copy_dir_recursive(&extracted.root_dir, &target_paths.root)?;
            }
            if extracted.metadata.include_state {
                if config_directory_permissions.is_some() {
                    // The copied directory can be read-only. Rewrite inside a private,
                    // writable directory, then restore its archived permissions.
                    super::common::ensure_private_dir(&target_paths.state_dir)?;
                }
            } else {
                validate_nonportable_runtime_state_paths(&target_paths)?;
            }
            reject_include_owned_sandbox_origin(&target_paths.config_path)?;
            let copied_includes = extracted_workspaces
                .config_include_paths()
                .map(|path| {
                    path.strip_prefix(&extracted.root_dir)
                        .map(|relative| target_paths.root.join(relative))
                        .map_err(|error| error.to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            super::openclaw_config::rewrite_copied_include_directives(
                &copied_includes,
                &source_roots,
                &target_paths.root,
            )?;
            let config_rewrite = super::openclaw_config::rewrite_archived_openclaw_config(
                &target_paths,
                &source_roots,
                extracted
                    .metadata
                    .env
                    .canonical_source_root
                    .as_deref()
                    .map(Path::new),
                Some(gateway_port),
                sandbox_origin.as_deref(),
            )?;
            rewrite_identity_bound_workspace_paths_for_target(
                &target_paths.config_path,
                &extracted_workspaces,
                &extracted.root_dir,
                &target_paths.root,
                extracted
                    .metadata
                    .env
                    .source_root
                    .as_deref()
                    .map(Path::new)
                    .unwrap_or(&extracted.root_dir),
            )?;
            verify_copied_workspaces_remain_preserved(
                &extracted_workspaces,
                &extracted.root_dir,
                &target_paths,
                env,
                target_runtime,
                extracted
                    .metadata
                    .env
                    .source_root
                    .as_deref()
                    .map(Path::new)
                    .unwrap_or(&extracted.root_dir),
            )?;
            let state_rewrite = if extracted.metadata.include_state {
                super::openclaw_state::relocate_archived_runtime_state(
                    &target_paths,
                    &source_roots,
                    env,
                    target_runtime,
                )?
            } else {
                let copied_git = super::openclaw_state::prepare_archived_worktrees(
                    &target_paths,
                    &source_roots,
                )?;
                super::openclaw_state::clear_copied_runtime_state_preserving_paths(
                    &target_paths,
                    env,
                    target_runtime,
                    &copied_git.preserved_roots,
                    &copied_git.config_paths,
                )?;
                let copied_paths = super::openclaw_workspaces::resolve_env_openclaw_copy_paths(
                    &target_paths,
                    env,
                    target_runtime,
                )?;
                super::openclaw_state::relocate_copied_databases(
                    &target_paths,
                    &source_roots,
                    super::openclaw_state::DatabasePathRelocation::ClonedSessions,
                    Some(&copied_paths),
                )?;
                copied_git.finish(&target_paths)?;
                Default::default()
            };
            if let Some(permissions) = &config_directory_permissions {
                fs::set_permissions(&target_paths.state_dir, permissions.clone())
                    .map_err(|error| error.to_string())?;
            }

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
                dev_ui_port: None,
                protected: extracted.metadata.env.protected,
                created_at,
                updated_at: created_at,
                last_used_at: None,
            };
            let (meta, _admission) = upsert_environment(&mut registry, meta, None, env, cwd)?;
            write_env_registry(&mut registry, env, cwd)?;
            Ok((meta, config_rewrite, state_rewrite))
        })();

        match imported {
            Ok((meta, config_rewrite, state_rewrite)) => Ok(ImportEnvironmentResult {
                summary: EnvImportSummary {
                    config_paths_rewritten: extracted.metadata.include_state.then_some(true),
                    state_paths_rewritten: extracted
                        .metadata
                        .include_state
                        .then_some(state_rewrite.changed),
                    database_paths_rewritten: extracted
                        .metadata
                        .include_state
                        .then_some(state_rewrite.databases),
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
                if extracted.metadata.include_state {
                    if let Err(cleanup) = super::common::remove_owned_tree(&target_paths.root) {
                        return Err(format!(
                            "{error}; failed to remove incomplete imported environment: {cleanup}"
                        ));
                    }
                } else {
                    let _ = fs::remove_dir_all(&target_paths.root);
                }
                Err(error)
            }
        }
    })();

    let _ = super::common::remove_owned_tree(&staging_dir);
    result
}

pub fn remove_environment(
    name: &str,
    force: bool,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvMeta, String> {
    let _operation = lock_environment_operation(name, env, cwd)?;
    remove_environment_locked(name, force, env, cwd, |_| Ok(()))
}

pub(crate) fn ensure_environment_removal_preserves_registered_roots(
    target: &EnvMeta,
    envs: &[EnvMeta],
) -> Result<(), String> {
    let removal = DevSourceRemoval::environment(target)?;
    for other in envs.iter().filter(|meta| meta.name != target.name) {
        let (registered, mut entries) =
            super::dev_registration::registration_path(Path::new(&other.root))?;
        entries.insert(registered);
        for entry in entries {
            if removal.contains_registered_path(&entry)? {
                return Err(format!(
                    "cannot remove environment {}: its cleanup contains registered environment {} root {} or a link needed to reach it; remove that environment separately first",
                    target.name, other.name, other.root
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn ensure_runtime_cleanup_preserves_registered_roots(
    target: &EnvMeta,
    envs: &[EnvMeta],
    paths: &BTreeSet<PathBuf>,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let removals = paths
        .iter()
        .map(|path| DevSourceRemoval::entry(path))
        .collect::<Result<Vec<_>, _>>()?;
    for other in envs.iter().filter(|meta| meta.name != target.name) {
        let registered =
            super::dev_registration::inspect_registration_path(Path::new(&other.root))?;
        for entry in registered.entries.iter().chain(registered.location.iter()) {
            for removal in &removals {
                if removal.contains_registered_path(entry)? {
                    return Err(format!(
                        "cannot clean up environment {}: its runtime reset contains registered environment {} root {} or a link needed to reach it; preserve any needed data and remove that environment separately before retrying",
                        target.name, other.name, other.root
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn remove_environment_locked(
    name: &str,
    force: bool,
    env: &BTreeMap<String, String>,
    cwd: &Path,
    before_remove: impl FnOnce(&EnvMeta) -> Result<(), String>,
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

    ensure_environment_removal_preserves_registered_roots(&meta, &registry.envs)?;
    super::dev_sources::ensure_environment_removal_preserves_dev_sources(
        &meta,
        &registry.envs,
        &source_service,
    )?;
    // Preliminary cleanup shares this registry lock through final deletion.
    // It must not call back into registry-mutating operations.
    before_remove(&meta)?;

    if let Some((repo_root, worktree_root)) = meta.dev.as_ref().and_then(|dev| dev.owned_worktree())
    {
        remove_openclaw_worktree(Path::new(repo_root), Path::new(worktree_root))?;
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
    super::dev_ui_ports::remove(&meta, env, cwd)?;

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

    use crate::env::{CreateEnvironmentOptions, EnvDevMeta};
    use serde_json::{Value, json};

    use super::{
        CloneEnvironmentOptions, NEXT_IMPORT_ID, OpenClawWorkspaceRuntime, Ordering,
        clear_simulation_runtime_state_preserving_includes, clone_environment_for_simulation,
        create_environment, create_private_simulation_staging_dir, derive_env_paths,
        get_environment, remove_environment, restore_environment_service_policy, save_environment,
        set_environment_service_policy,
    };

    #[test]
    fn private_config_dev_is_published_with_new_environment() {
        let root = tempfile::tempdir().unwrap();
        let env = BTreeMap::from([
            (
                "HOME".to_string(),
                root.path().join("home").display().to_string(),
            ),
            (
                "OCM_HOME".to_string(),
                root.path().join("ocm-home").display().to_string(),
            ),
        ]);
        for (name, dev) in [
            (
                "dev",
                Some(EnvDevMeta::Owned {
                    repo_root: root.path().join("repo").display().to_string(),
                    worktree_root: root.path().join("worktree").display().to_string(),
                }),
            ),
            ("ordinary", None),
        ] {
            let meta = create_environment(
                CreateEnvironmentOptions {
                    name: name.to_string(),
                    root: None,
                    gateway_port: Some(if dev.is_some() { 19789 } else { 19821 }),
                    service_enabled: false,
                    service_running: false,
                    default_runtime: None,
                    default_launcher: None,
                    dev,
                    protected: false,
                },
                &env,
                root.path(),
            )
            .unwrap();
            let published = get_environment(name, &env, root.path()).unwrap();
            assert_eq!(published.root, meta.root);
            let paths = derive_env_paths(Path::new(&meta.root));
            assert_eq!(paths.config_path.is_file(), meta.dev.is_some());
            if meta.dev.is_some() {
                let config: Value =
                    serde_json::from_slice(&fs::read(&paths.config_path).unwrap()).unwrap();
                let token = config["gateway"]["auth"]["token"].as_str().unwrap();
                assert_eq!(token.len(), 64);
                assert!(!serde_json::to_string(&published).unwrap().contains(token));
            }
        }
    }

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
                    list: [
                      {{ id: "main", default: true, workspace: "{}", agentDir: {{ $include: "./agent-dir.json" }} }},
                      {{ id: "outside", agentDir: "${{OPENCLAW_STATE_DIR}}/private/${{OCM_ACTIVE_ENV}}" }},
                    ],
                  }},
                  mcp: {{ apps: {{
                    sandboxPort: 19790,
                    sandboxOrigin: "https://source.example.test",
                  }} }},
                  nested: {{ $include: {} }},
                  plugin: {{ agentDir: "${{OPENCLAW_STATE_DIR}}/workspace/${{OCM_ACTIVE_ENV}}" }},
                }}
"#,
                source_paths.workspace_dir.display(),
                source_paths.workspace_dir.display(),
                serde_json::to_string(&include_dir.join("nested.json5")).unwrap()
            ),
        )
        .unwrap();
        fs::write(
            include_dir.join("nested.json5"),
            "{\n  // Nested JSON5 includes must be preserved.\n  enabled: true,\n}\n",
        )
        .unwrap();
        let source_base_before = fs::read(include_dir.join("base.json")).unwrap();
        let source_nested_before = fs::read(include_dir.join("nested.json5")).unwrap();
        let agent_template = "${OPENCLAW_STATE_DIR}/workspace/${OCM_ACTIVE_ENV}";
        let agent_include = serde_json::to_string(agent_template).unwrap();
        fs::write(include_dir.join("agent-dir.json"), &agent_include).unwrap();
        for directory in ["workspace/source", "private/source"] {
            fs::create_dir_all(source_paths.state_dir.join(directory)).unwrap();
            fs::write(
                source_paths.state_dir.join(directory).join("models.json"),
                "{}",
            )
            .unwrap();
        }
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

        let local_plugin = source_paths.root.join("local-plugin");
        fs::create_dir_all(&local_plugin).unwrap();
        fs::write(local_plugin.join("index.js"), "export default {};\n").unwrap();
        let external_project = root.join("plugin-project");
        fs::create_dir_all(&external_project).unwrap();
        let database = source_paths.state_dir.join("state/openclaw.sqlite");
        fs::create_dir_all(database.parent().unwrap()).unwrap();
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        connection.execute(
            "CREATE TABLE config_machine_state (state_key TEXT PRIMARY KEY, value_json TEXT NOT NULL)",
            [],
        ).unwrap();
        connection.execute(
            "INSERT INTO config_machine_state (state_key, value_json) VALUES ('plugins.installedIndex', ?1)",
            [json!({"revision": 42, "index": {"installRecords": {
                "local": {"source": "path", "sourcePath": external_project,
                    "installPath": local_plugin},
                "missing": {"source": "npm", "spec": "absent-plugin@1.0.0",
                    "installPath": source_paths.state_dir.join("extensions/absent")}
            }}}).to_string()],
        ).unwrap();
        connection.execute_batch(
            "CREATE TABLE agent_database_leases (lease_id TEXT PRIMARY KEY, owner_pid INTEGER NOT NULL);
             INSERT INTO agent_database_leases VALUES ('copied-source-owner', 1);",
        ).unwrap();
        connection
            .execute_batch("CREATE TABLE session_nodes (entry_json TEXT NOT NULL);")
            .unwrap();
        connection
            .execute(
                "INSERT INTO session_nodes VALUES (?1)",
                [json!({"sessionRoot": source_paths.workspace_dir, "spawnedCwd": source_paths.workspace_dir}).to_string()],
            )
            .unwrap();
        let source_database_before = fs::read(&database).unwrap();
        let source_wal_before = fs::read(database.with_extension("sqlite-wal")).unwrap();
        let source_connection = connection;

        let registry = super::env_registry_path(&env, &cwd).unwrap();
        let registry_before = fs::read(&registry).unwrap();
        let nested = source_paths.root.join("nested-simulation");
        let rejected = clone_environment_for_simulation(
            CloneEnvironmentOptions {
                source_name: "source".to_string(),
                name: "blocked-simulation".to_string(),
                root: Some(nested.display().to_string()),
            },
            &env,
            &cwd,
        )
        .unwrap_err();
        assert!(
            rejected.contains("overlaps environment source root"),
            "{rejected}"
        );
        assert!(!nested.exists());
        assert_eq!(fs::read(&registry).unwrap(), registry_before);

        let root_config = fs::read(&source_paths.config_path).unwrap();
        let shared = json!({
            "$include": "./config/base.json",
            "plugins": {"entries": {"demo": {"config": {"outputDir": {"$include": "./config/agent-dir.json"}}}}}
        });
        let shared_raw = serde_json::to_vec(&shared).unwrap();
        fs::write(&source_paths.config_path, &shared_raw).unwrap();
        let error = clone_environment_for_simulation(
            CloneEnvironmentOptions {
                source_name: "source".to_string(),
                name: "shared-simulation".to_string(),
                root: None,
            },
            &env,
            &cwd,
        )
        .unwrap_err();
        assert!(error.contains("shared OpenClaw include"), "{error}");
        assert!(!root.join("ocm-home/envs/shared-simulation").exists());
        assert_eq!(fs::read(&source_paths.config_path).unwrap(), shared_raw);
        assert_eq!(
            fs::read_to_string(include_dir.join("agent-dir.json")).unwrap(),
            agent_include
        );
        let effective = super::super::openclaw_workspaces::load_effective_openclaw_config(
            &source_paths.config_path,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            effective.value["plugins"]["entries"]["demo"]["config"]["outputDir"],
            agent_template
        );
        fs::write(&source_paths.config_path, root_config).unwrap();

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
        let connection =
            rusqlite::Connection::open(target_paths.state_dir.join("state/openclaw.sqlite"))
                .unwrap();
        let lease_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM agent_database_leases", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(lease_count, 0);
        let session: String = connection
            .query_row("SELECT entry_json FROM session_nodes", [], |row| row.get(0))
            .unwrap();
        let session: Value = serde_json::from_str(&session).unwrap();
        for field in ["sessionRoot", "spawnedCwd"] {
            assert_eq!(
                session[field],
                target_paths.workspace_dir.display().to_string()
            );
        }

        let document: String = connection.query_row(
            "SELECT value_json FROM config_machine_state WHERE state_key = 'plugins.installedIndex'",
            [],
            |row| row.get(0),
        ).unwrap();
        drop(connection);
        let document: Value = serde_json::from_str(&document).unwrap();
        assert_eq!(document["revision"], 42);
        let records = &document["index"]["installRecords"];
        assert_eq!(
            records["local"]["sourcePath"],
            external_project.display().to_string()
        );
        assert_eq!(
            records["local"]["installPath"],
            target_paths.root.join("local-plugin").display().to_string()
        );
        assert_eq!(
            records["missing"]["installPath"],
            target_paths
                .state_dir
                .join("extensions/absent")
                .display()
                .to_string()
        );
        assert!(!target_paths.state_dir.join("extensions/absent").exists());
        fs::write(
            target_paths.root.join("local-plugin/index.js"),
            "clone-only\n",
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(local_plugin.join("index.js")).unwrap(),
            "export default {};\n"
        );
        assert_eq!(fs::read(&database).unwrap(), source_database_before);
        assert_eq!(
            fs::read(database.with_extension("sqlite-wal")).unwrap(),
            source_wal_before
        );
        drop(source_connection);
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
        assert_eq!(
            included["nested"]["$include"],
            target_paths
                .state_dir
                .join("config/nested.json5")
                .display()
                .to_string()
        );
        assert_eq!(
            fs::read(include_dir.join("base.json")).unwrap(),
            source_base_before
        );
        assert_eq!(
            fs::read(include_dir.join("nested.json5")).unwrap(),
            source_nested_before
        );
        let target_inventory = super::super::openclaw_workspaces::resolve_env_openclaw_copy_paths(
            &target_paths,
            &env,
            OpenClawWorkspaceRuntime::for_env("simulation", simulation.gateway_port),
        )
        .unwrap();
        for (agent, directory) in [("main", "workspace/source"), ("outside", "private/source")] {
            assert_eq!(
                target_inventory.agent_directory(agent),
                Some(target_paths.state_dir.join(directory).as_path())
            );
            assert_eq!(
                fs::read(target_paths.state_dir.join(directory).join("models.json")).unwrap(),
                b"{}"
            );
            assert_eq!(
                fs::read(source_paths.state_dir.join(directory).join("models.json")).unwrap(),
                b"{}"
            );
        }
        assert_eq!(included["plugin"]["agentDir"], agent_template);
        assert_eq!(
            fs::read_to_string(include_dir.join("agent-dir.json")).unwrap(),
            agent_include
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
            &std::collections::BTreeSet::new(),
            &std::collections::BTreeSet::new(),
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
