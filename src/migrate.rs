use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::env::{CreateEnvironmentOptions, EnvImportSummary, EnvironmentService};
use crate::launcher::{AddLauncherOptions, LauncherService};
use crate::store::{
    OpenClawWorkspaceRuntime, copy_dir_recursive, default_env_root, derive_env_paths, display_path,
    list_environments, normalize_new_environment_sandbox_origin, prepare_migrated_runtime_state,
    reject_include_owned_agent_workspaces, reject_include_owned_sandbox_origin,
    resolve_plain_openclaw_workspaces, resolve_user_home,
    rewrite_external_workspace_paths_for_migration, rewrite_openclaw_config_for_migration,
    validate_name,
};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MigrationSourceSummary {
    pub source_home: String,
    pub config_path: String,
    pub workspace_dir: String,
    pub exists: bool,
    pub config_exists: bool,
    pub workspace_exists: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MigrationPlanSummary {
    pub source: MigrationSourceSummary,
    pub env_name: String,
    pub env_exists: bool,
    pub target_root: String,
}

#[derive(Clone, Debug)]
pub struct MigrateHomeOptions {
    pub source_home: Option<String>,
    pub name: String,
    pub root: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct MigrateHomeResult {
    pub summary: EnvImportSummary,
    pub cleared_sandbox_origin: bool,
    pub sandbox_port: Option<u32>,
}

#[derive(Clone, Debug)]
struct MigratedLauncherSpec {
    name: String,
    command_path: String,
    needs_creation: bool,
}

#[derive(Clone, Debug)]
struct ExternalWorkspaceCopy {
    source: PathBuf,
    target: PathBuf,
}

#[derive(Clone, Debug, Default)]
struct ExternalWorkspaceCopyPlan {
    default_workspace: Option<ExternalWorkspaceCopy>,
    agent_workspaces: BTreeMap<String, ExternalWorkspaceCopy>,
}

impl ExternalWorkspaceCopyPlan {
    fn for_migration(
        source_home: &Path,
        target_paths: &crate::store::EnvPaths,
        env: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let source_home = fs::canonicalize(source_home).map_err(|error| {
            format!(
                "failed to resolve plain OpenClaw home {}: {error}",
                display_path(source_home)
            )
        })?;
        let sources = resolve_plain_openclaw_workspaces(&source_home, env)?
            .external_workspace_sources_for_migration(&source_home)?;
        let canonical_target_root = canonicalize_path_allow_missing(&target_paths.root)?;
        let mut plan = Self::default();
        if let Some(source) = sources.default_workspace {
            plan.default_workspace = Some(ExternalWorkspaceCopy {
                target: target_paths.state_dir.join("workspaces/default"),
                source,
            });
        }
        for (agent_id, source) in sources.agent_workspaces {
            plan.agent_workspaces.insert(
                agent_id.clone(),
                ExternalWorkspaceCopy {
                    target: target_paths
                        .state_dir
                        .join("workspaces/agents")
                        .join(agent_id),
                    source,
                },
            );
        }
        for workspace in plan
            .default_workspace
            .iter()
            .chain(plan.agent_workspaces.values())
        {
            if workspace.source.starts_with(&canonical_target_root)
                || canonical_target_root.starts_with(&workspace.source)
            {
                return Err(format!(
                    "external OpenClaw workspace and migration target must not overlap: workspace={} target={}",
                    display_path(&workspace.source),
                    display_path(&canonical_target_root)
                ));
            }
        }
        Ok(plan)
    }

    fn copy_and_rewrite_config(&self, config_path: &Path) -> Result<(), String> {
        remove_copied_external_default_workspace_link(config_path)?;
        if let Some(workspace) = &self.default_workspace {
            copy_dir_recursive(&workspace.source, &workspace.target)?;
        }
        for workspace in self.agent_workspaces.values() {
            copy_dir_recursive(&workspace.source, &workspace.target)?;
        }
        let agent_workspaces = self
            .agent_workspaces
            .iter()
            .map(|(agent_id, workspace)| (agent_id.clone(), workspace.target.clone()))
            .collect::<BTreeMap<_, _>>();
        rewrite_external_workspace_paths_for_migration(
            config_path,
            self.default_workspace
                .as_ref()
                .map(|workspace| workspace.target.as_path()),
            &agent_workspaces,
        )?;
        Ok(())
    }
}

fn remove_copied_external_default_workspace_link(config_path: &Path) -> Result<(), String> {
    let Some(state_dir) = config_path.parent() else {
        return Ok(());
    };
    let workspace = state_dir.join("workspace");
    let Ok(metadata) = fs::symlink_metadata(&workspace) else {
        return Ok(());
    };
    if !metadata.file_type().is_symlink() {
        return Ok(());
    }
    let resolved = fs::canonicalize(&workspace).map_err(|error| {
        format!(
            "failed to resolve copied OpenClaw workspace symlink {}: {error}",
            display_path(&workspace)
        )
    })?;
    let canonical_state_dir = fs::canonicalize(state_dir).map_err(|error| {
        format!(
            "failed to resolve imported OpenClaw state directory {}: {error}",
            display_path(state_dir)
        )
    })?;
    if resolved.starts_with(&canonical_state_dir) {
        return Ok(());
    }
    fs::remove_file(&workspace)
        .or_else(|_| fs::remove_dir(&workspace))
        .map_err(|error| {
            format!(
                "failed to remove copied external OpenClaw workspace symlink {}: {error}",
                display_path(&workspace)
            )
        })
}

pub fn default_migration_source_home(env: &BTreeMap<String, String>) -> PathBuf {
    resolve_user_home(env).join(".openclaw")
}

pub fn resolve_migration_source_home(
    explicit: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> PathBuf {
    explicit
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_migration_source_home(env))
}

pub fn inspect_migration_source(
    explicit: Option<&Path>,
    env: &BTreeMap<String, String>,
) -> MigrationSourceSummary {
    let source_home = resolve_migration_source_home(explicit, env);
    let config_path = source_home.join("openclaw.json");
    let workspace_dir = source_home.join("workspace");

    MigrationSourceSummary {
        source_home: display_path(&source_home),
        config_path: display_path(&config_path),
        workspace_dir: display_path(&workspace_dir),
        exists: source_home.exists(),
        config_exists: config_path.exists(),
        workspace_exists: workspace_dir.exists(),
    }
}

pub fn plan_migration(
    explicit_source_home: Option<&Path>,
    env_name: &str,
    explicit_root: Option<&str>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<MigrationPlanSummary, String> {
    let env_name = validate_name(env_name, "Environment name")?;
    let target_root = resolve_migration_target_root(explicit_root, &env_name, env, cwd)?;

    let env_exists = list_environments(env, cwd)?
        .iter()
        .any(|meta| meta.name == env_name);

    Ok(MigrationPlanSummary {
        source: inspect_migration_source(explicit_source_home, env),
        env_exists,
        env_name,
        target_root: display_path(&target_root),
    })
}

pub fn migrate_plain_openclaw_home(
    options: MigrateHomeOptions,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<EnvImportSummary, String> {
    Ok(migrate_plain_openclaw_home_with_sandbox_origin(options, None, env, cwd)?.summary)
}

pub(crate) fn migrate_plain_openclaw_home_with_sandbox_origin(
    options: MigrateHomeOptions,
    sandbox_origin: Option<&str>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<MigrateHomeResult, String> {
    let (summary, _, cleared_sandbox_origin, sandbox_port) =
        migrate_plain_openclaw_home_inner(options, sandbox_origin, env, cwd)?;
    Ok(MigrateHomeResult {
        summary,
        cleared_sandbox_origin,
        sandbox_port,
    })
}

fn migrate_plain_openclaw_home_inner(
    options: MigrateHomeOptions,
    sandbox_origin: Option<&str>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(EnvImportSummary, Option<String>, bool, Option<u32>), String> {
    let sandbox_origin = normalize_new_environment_sandbox_origin(sandbox_origin)?;
    let service = EnvironmentService::new(env, cwd);
    let env_name = validate_name(&options.name, "Environment name")?;
    let source_home =
        resolve_migration_source_home(options.source_home.as_deref().map(Path::new), env);
    if !source_home.exists() {
        return Err(format!(
            "plain OpenClaw home does not exist: {}",
            display_path(&source_home)
        ));
    }
    reject_include_owned_sandbox_origin(&source_home.join("openclaw.json"))?;
    reject_include_owned_agent_workspaces(&source_home.join("openclaw.json"))?;
    let target_root = resolve_migration_target_root(options.root.as_deref(), &env_name, env, cwd)?;
    let target_root_string = target_root
        .to_str()
        .ok_or_else(|| {
            format!(
                "migration target root must use valid UTF-8: {}",
                display_path(&target_root)
            )
        })?
        .to_string();
    reject_overlapping_migration_paths(&source_home, &target_root)?;
    let target_paths = derive_env_paths(&target_root);
    let external_workspace_plan =
        ExternalWorkspaceCopyPlan::for_migration(&source_home, &target_paths, env)?;
    let migrated_launcher = preflight_migrated_launcher(&env_name, env, cwd)?;

    let created = service.create(CreateEnvironmentOptions {
        name: env_name,
        root: Some(target_root_string),
        gateway_port: None,
        service_enabled: false,
        service_running: false,
        default_runtime: None,
        default_launcher: None,
        dev: None,
        protected: false,
    })?;
    let created_name = created.name.clone();
    let target_paths = derive_env_paths(Path::new(&created.root));

    let created = match complete_migration_import(
        created,
        &target_paths,
        &source_home,
        &external_workspace_plan,
        sandbox_origin.as_deref(),
        migrated_launcher.as_ref(),
        env,
        cwd,
    ) {
        Ok((created, created_launcher, cleared_sandbox_origin, sandbox_port)) => (
            created,
            created_launcher,
            cleared_sandbox_origin,
            sandbox_port,
        ),
        Err(error) => {
            let rollback = service.remove(&created_name, true).map(|_| ());
            return Err(with_rollback_error(
                error,
                "removing the partially imported environment",
                rollback,
            ));
        }
    };

    Ok((
        EnvImportSummary {
            name: created.0.name,
            source_name: "plain-openclaw".to_string(),
            root: created.0.root,
            archive_path: display_path(&source_home),
            default_runtime: created.0.default_runtime,
            default_launcher: created.0.default_launcher,
            protected: created.0.protected,
        },
        created.1,
        created.2,
        created.3,
    ))
}

fn rollback_migrated_launcher(
    launcher_name: Option<&str>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(), String> {
    if let Some(name) = launcher_name {
        LauncherService::new(env, cwd).remove(name)?;
    }
    Ok(())
}

fn with_rollback_error(
    primary_error: String,
    operation: &str,
    rollback: Result<(), String>,
) -> String {
    match rollback {
        Ok(()) => primary_error,
        Err(rollback_error) => {
            format!("{primary_error}\nrollback error while {operation}: {rollback_error}")
        }
    }
}

fn complete_migration_import(
    created: crate::env::EnvMeta,
    target_paths: &crate::store::EnvPaths,
    source_home: &Path,
    external_workspace_plan: &ExternalWorkspaceCopyPlan,
    sandbox_origin: Option<&str>,
    migrated_launcher: Option<&MigratedLauncherSpec>,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<(crate::env::EnvMeta, Option<String>, bool, Option<u32>), String> {
    if target_paths.state_dir.exists() {
        fs::remove_dir_all(&target_paths.state_dir).map_err(|error| error.to_string())?;
    }

    copy_dir_recursive(source_home, &target_paths.state_dir)?;
    external_workspace_plan.copy_and_rewrite_config(&target_paths.config_path)?;
    reject_include_owned_sandbox_origin(&target_paths.config_path)?;
    let config_rewrite = rewrite_openclaw_config_for_migration(
        target_paths,
        source_home,
        created.gateway_port,
        sandbox_origin,
    )?;
    prepare_migrated_runtime_state(
        target_paths,
        source_home,
        env,
        OpenClawWorkspaceRuntime::for_env(&created.name, created.gateway_port),
    )?;

    let Some(launcher) = migrated_launcher else {
        return Ok((
            created,
            None,
            config_rewrite.cleared_sandbox_origin,
            config_rewrite.sandbox_port,
        ));
    };

    let launcher_service = LauncherService::new(env, cwd);
    let created_launcher = if launcher.needs_creation {
        launcher_service.add(AddLauncherOptions {
            name: launcher.name.clone(),
            command: launcher.command_path.clone(),
            cwd: None,
            description: Some(format!(
                "Imported plain OpenClaw command for env {}",
                created.name
            )),
        })?;
        Some(launcher.name.clone())
    } else {
        None
    };

    match EnvironmentService::new(env, cwd).set_launcher(&created.name, &launcher.name) {
        Ok(meta) => Ok((
            meta,
            created_launcher,
            config_rewrite.cleared_sandbox_origin,
            config_rewrite.sandbox_port,
        )),
        Err(error) => {
            let rollback = rollback_migrated_launcher(created_launcher.as_deref(), env, cwd);
            Err(with_rollback_error(
                error,
                "removing the migrated launcher",
                rollback,
            ))
        }
    }
}

fn reject_overlapping_migration_paths(
    source_home: &Path,
    target_root: &Path,
) -> Result<(), String> {
    let source_home = canonicalize_path_allow_missing(source_home)?;
    let target_root = canonicalize_path_allow_missing(target_root)?;
    let target_state_dir =
        canonicalize_path_allow_missing(&derive_env_paths(&target_root).state_dir)?;
    let overlapping_target = [&target_root, &target_state_dir]
        .into_iter()
        .find(|target| source_home.starts_with(target) || target.starts_with(&source_home));
    if let Some(target) = overlapping_target {
        return Err(format!(
            "migration source and target must not overlap: source={} target={}",
            display_path(&source_home),
            display_path(target)
        ));
    }
    reject_filesystem_aliases(&source_home, &[&target_root, &target_state_dir])?;
    Ok(())
}

fn resolve_migration_target_root(
    explicit_root: Option<&str>,
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let unresolved = if let Some(input) = explicit_root {
        let input = input.trim();
        if input.is_empty() {
            return Err("path is required".to_string());
        }
        match input {
            "~" => resolve_user_home(env),
            _ if input.starts_with("~/") || input.starts_with("~\\") => {
                resolve_user_home(env).join(&input[2..])
            }
            _ => {
                let path = Path::new(input);
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    cwd.join(path)
                }
            }
        }
    } else {
        default_env_root(env_name, env, cwd)?
    };

    normalize_migration_target_path(&unresolved)
}

fn normalize_migration_target_path(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => match fs::symlink_metadata(&normalized) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    normalized = fs::canonicalize(&normalized).map_err(|error| {
                        format!(
                            "failed to resolve migration path {}: {error}",
                            display_path(path)
                        )
                    })?;
                    let target_metadata = fs::metadata(&normalized).map_err(|error| {
                        format!(
                            "failed to inspect migration path {}: {error}",
                            display_path(&normalized)
                        )
                    })?;
                    if !target_metadata.is_dir() {
                        return Err(format!(
                            "migration target parent traversal crosses a non-directory: {}",
                            display_path(&normalized)
                        ));
                    }
                    normalized.pop();
                }
                Ok(metadata) if metadata.is_dir() => {
                    normalized.pop();
                }
                Ok(_) => {
                    return Err(format!(
                        "migration target parent traversal crosses a non-directory: {}",
                        display_path(&normalized)
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    normalized.pop();
                }
                Err(error) => {
                    return Err(format!(
                        "failed to resolve migration path {}: {error}",
                        display_path(path)
                    ));
                }
            },
            Component::Normal(name) => normalized.push(name),
        }
    }
    Ok(normalized)
}

#[cfg(unix)]
fn reject_filesystem_aliases(source_home: &Path, targets: &[&Path]) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    let source_tree = collect_source_directory_identities(source_home)?;

    // Canonical paths do not reveal bind-mount aliases. Map an existing target
    // ancestor back to the matching source ancestor before checking overlap.
    // Source descendants matter too because a target can sit below a bind mount
    // of any directory in the tree that is about to be copied.
    for target in targets {
        for target_ancestor in target.ancestors().filter(|path| path.exists()) {
            let target_suffix = target.strip_prefix(target_ancestor).map_err(|error| {
                format!(
                    "failed to resolve migration target {}: {error}",
                    display_path(target)
                )
            })?;
            let target_metadata = fs::metadata(target_ancestor).map_err(|error| {
                format!(
                    "failed to inspect migration target ancestor {}: {error}",
                    display_path(target_ancestor)
                )
            })?;
            let target_identity = (target_metadata.dev(), target_metadata.ino());
            if source_tree.contains(&target_identity) {
                return Err(format!(
                    "migration source and target must not alias the same filesystem tree: source={} target={}",
                    display_path(source_home),
                    display_path(target)
                ));
            }

            for source_ancestor in source_home.ancestors().skip(1) {
                let source_metadata = fs::metadata(source_ancestor).map_err(|error| {
                    format!(
                        "failed to inspect migration source ancestor {}: {error}",
                        display_path(source_ancestor)
                    )
                })?;
                if source_metadata.dev() != target_metadata.dev()
                    || source_metadata.ino() != target_metadata.ino()
                {
                    continue;
                }

                let mapped_target = source_ancestor.join(target_suffix);
                if source_home.starts_with(&mapped_target) || mapped_target.starts_with(source_home)
                {
                    return Err(format!(
                        "migration source and target must not alias the same filesystem tree: source={} target={}",
                        display_path(source_home),
                        display_path(target)
                    ));
                }
            }
        }
    }

    Ok(())
}

#[cfg(unix)]
fn collect_source_directory_identities(
    source_home: &Path,
) -> Result<std::collections::HashSet<(u64, u64)>, String> {
    use std::os::unix::fs::MetadataExt;

    let mut identities = std::collections::HashSet::new();
    let mut pending = vec![source_home.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::metadata(&path).map_err(|error| {
            format!(
                "failed to inspect migration source directory {}: {error}",
                display_path(&path)
            )
        })?;
        if !metadata.is_dir() || !identities.insert((metadata.dev(), metadata.ino())) {
            continue;
        }

        for entry in fs::read_dir(&path).map_err(|error| {
            format!(
                "failed to read migration source directory {}: {error}",
                display_path(&path)
            )
        })? {
            let entry = entry.map_err(|error| error.to_string())?;
            let entry_metadata =
                fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
            if entry_metadata.is_dir() && !entry_metadata.file_type().is_symlink() {
                pending.push(entry.path());
            }
        }
    }
    Ok(identities)
}

#[cfg(not(unix))]
fn reject_filesystem_aliases(_source_home: &Path, _targets: &[&Path]) -> Result<(), String> {
    Ok(())
}

fn canonicalize_path_allow_missing(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path;
    let mut missing = Vec::<OsString>::new();
    loop {
        match fs::canonicalize(existing) {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or_else(|| {
                    format!("failed to resolve migration path: {}", display_path(path))
                })?;
                missing.push(name.to_os_string());
                existing = existing.parent().ok_or_else(|| {
                    format!("failed to resolve migration path: {}", display_path(path))
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "failed to resolve migration path {}: {error}",
                    display_path(path)
                ));
            }
        };
    }
}

fn preflight_migrated_launcher(
    env_name: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Option<MigratedLauncherSpec>, String> {
    let Some(command_path) = resolve_executable_on_path("openclaw", env) else {
        return Ok(None);
    };

    let launcher_name = format!("{env_name}.migrated");
    match LauncherService::new(env, cwd).show(&launcher_name) {
        Ok(existing) => {
            if existing.command != command_path || existing.cwd.is_some() {
                Err(format!(
                    "launcher \"{launcher_name}\" already exists with different settings; choose another env name or remove the conflicting launcher"
                ))
            } else {
                Ok(Some(MigratedLauncherSpec {
                    name: launcher_name,
                    command_path,
                    needs_creation: false,
                }))
            }
        }
        Err(error) if error.contains("does not exist") => Ok(Some(MigratedLauncherSpec {
            name: launcher_name,
            command_path,
            needs_creation: true,
        })),
        Err(error) => Err(error),
    }
}

#[cfg(not(windows))]
fn resolve_executable_on_path(command: &str, env: &BTreeMap<String, String>) -> Option<String> {
    let path_value = env.get("PATH")?;
    std::env::split_paths(path_value)
        .map(|dir| dir.join(command))
        .find(|candidate| is_executable_file(candidate))
        .map(|candidate| display_path(&candidate))
}

#[cfg(windows)]
fn resolve_executable_on_path(command: &str, env: &BTreeMap<String, String>) -> Option<String> {
    let path_value = env.get("PATH")?;
    let extensions = env
        .get("PATHEXT")
        .map(|value| {
            value
                .split(';')
                .map(str::trim)
                .filter(|extension| !extension.is_empty())
                .map(|extension| {
                    if extension.starts_with('.') {
                        extension.to_string()
                    } else {
                        format!(".{extension}")
                    }
                })
                .collect::<Vec<_>>()
        })
        .filter(|extensions| !extensions.is_empty())
        .unwrap_or_else(|| {
            [".COM", ".EXE", ".BAT", ".CMD"]
                .into_iter()
                .map(str::to_string)
                .collect()
        });

    for dir in std::env::split_paths(path_value) {
        if let Some(extension) = Path::new(command)
            .extension()
            .and_then(|value| value.to_str())
        {
            if extensions.iter().any(|candidate| {
                candidate
                    .trim_start_matches('.')
                    .eq_ignore_ascii_case(extension)
            }) {
                let exact = dir.join(command);
                if exact.is_file() {
                    return Some(display_path(&exact));
                }
            }
            continue;
        }
        for extension in &extensions {
            let candidate = dir.join(format!("{command}{extension}"));
            if candidate.is_file() {
                return Some(display_path(&candidate));
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if !path.is_file() {
        return false;
    }
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };

    // `access` applies the current process identity and the relevant owner,
    // group, or other permission class instead of accepting any execute bit.
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(all(not(unix), not(windows)))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use crate::env::{CreateEnvironmentOptions, EnvironmentService};
    use crate::launcher::{AddLauncherOptions, LauncherService};
    use crate::store::derive_env_paths;

    use super::{
        MigrateHomeOptions, default_migration_source_home, inspect_migration_source,
        migrate_plain_openclaw_home, migrate_plain_openclaw_home_with_sandbox_origin,
        plan_migration, reject_filesystem_aliases, with_rollback_error,
    };

    fn install_fake_openclaw_on_path(root: &Path, env: &mut BTreeMap<String, String>) {
        let bin_dir = root.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let openclaw = bin_dir.join("openclaw");
        fs::write(&openclaw, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&openclaw).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&openclaw, permissions).unwrap();
        }

        let existing_path = env.get("PATH").cloned().unwrap_or_default();
        let path = if existing_path.is_empty() {
            bin_dir.display().to_string()
        } else {
            format!("{}:{existing_path}", bin_dir.display())
        };
        env.insert("PATH".to_string(), path);
    }

    #[test]
    fn inspect_migration_source_defaults_to_user_openclaw_home() {
        let root = std::env::temp_dir().join("ocm-migrate-tests-default");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), root.display().to_string());

        let summary = inspect_migration_source(None, &env);
        assert_eq!(
            summary.source_home,
            default_migration_source_home(&env).display().to_string()
        );
        assert!(!summary.exists);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn inspect_migration_source_reports_existing_config_and_workspace() {
        let root = std::env::temp_dir().join("ocm-migrate-tests-explicit");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("workspace")).unwrap();
        fs::write(root.join("openclaw.json"), "{}\n").unwrap();

        let env = BTreeMap::new();
        let summary = inspect_migration_source(Some(Path::new(&root)), &env);

        assert!(summary.exists);
        assert!(summary.config_exists);
        assert!(summary.workspace_exists);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rollback_errors_preserve_the_primary_failure_and_append_each_cleanup_failure() {
        let error = with_rollback_error(
            "failed to update imported environment".to_string(),
            "removing the migrated launcher",
            Err("launcher registry is read-only".to_string()),
        );
        let error = with_rollback_error(
            error,
            "removing the partially imported environment",
            Err("environment root is busy".to_string()),
        );

        assert_eq!(
            error,
            "failed to update imported environment\n\
             rollback error while removing the migrated launcher: launcher registry is read-only\n\
             rollback error while removing the partially imported environment: environment root is busy"
        );
    }

    #[test]
    fn successful_rollback_keeps_the_primary_failure_unchanged() {
        assert_eq!(
            with_rollback_error(
                "migration failed".to_string(),
                "removing partial state",
                Ok(())
            ),
            "migration failed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_alias_check_maps_missing_targets_from_an_existing_source_ancestor() {
        let root = std::env::temp_dir().join("ocm-migrate-tests-filesystem-alias");
        let source_home = root.join("source");
        let target = source_home.join("existing-parent/nested/env");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(source_home.join("existing-parent")).unwrap();

        let error = reject_filesystem_aliases(&source_home, &[&target]).unwrap_err();
        assert!(error.contains("must not alias the same filesystem tree"));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn migration_rejects_non_utf8_target_roots_before_mutation() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let root = std::env::temp_dir().join("ocm-migrate-tests-non-utf8-target");
        let cwd = root.join(OsString::from_vec(b"cwd-\xff".to_vec()));
        let source_home = root.join("source");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(source_home.join("workspace")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(source_home.join("openclaw.json"), "{}\n").unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), root.join("home").display().to_string());
        env.insert(
            "OCM_HOME".to_string(),
            root.join("ocm-home").display().to_string(),
        );
        let error = migrate_plain_openclaw_home(
            MigrateHomeOptions {
                source_home: Some(source_home.display().to_string()),
                name: "mira".to_string(),
                root: Some("managed".to_string()),
            },
            &env,
            &cwd,
        )
        .unwrap_err();

        assert!(error.contains("migration target root must use valid UTF-8"));
        assert!(!root.join("ocm-home/envs.json").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn plan_migration_defaults_to_the_standard_target_root() {
        let root = std::env::temp_dir().join("ocm-migrate-tests-plan");
        let cwd = root.join("cwd");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&cwd).unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), root.join("home").display().to_string());
        env.insert(
            "OCM_HOME".to_string(),
            root.join("ocm-home").display().to_string(),
        );

        let plan = plan_migration(None, "mira", None, &env, &cwd).unwrap();
        assert_eq!(plan.env_name, "mira");
        assert!(plan.target_root.ends_with("/ocm-home/envs/mira"));
        assert!(!plan.env_exists);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_plain_openclaw_home_preserves_history_and_logs_while_clearing_live_residue() {
        let root = std::env::temp_dir().join("ocm-migrate-tests-apply");
        let cwd = root.join("cwd");
        let source_home = root.join("legacy-home");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(source_home.join("workspace")).unwrap();
        fs::create_dir_all(source_home.join("logs")).unwrap();
        fs::create_dir_all(source_home.join("run")).unwrap();
        fs::create_dir_all(source_home.join("agents/main/agent")).unwrap();
        fs::create_dir_all(source_home.join("agents/main/sessions")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            source_home.join("openclaw.json"),
            format!(
                concat!(
                    "{{",
                    "\"agents\":{{\"defaults\":{{\"workspace\":\"{}\"}}}},",
                    "\"gateway\":{{\"port\":18789}},",
                    "\"mcp\":{{\"apps\":{{",
                    "\"sandboxPort\":18790,",
                    "\"sandboxOrigin\":\"https://node.example.test:18790\"",
                    "}}}}",
                    "}}\n"
                ),
                source_home.join("workspace").display()
            ),
        )
        .unwrap();
        fs::write(source_home.join("workspace/notes.txt"), "hello\n").unwrap();
        fs::write(
            source_home.join("logs/app.log"),
            format!("cwd={}\n", source_home.join("workspace").display()),
        )
        .unwrap();
        fs::write(
            source_home.join("agents/main/agent/auth-profiles.json"),
            "{}\n",
        )
        .unwrap();
        fs::write(
            source_home.join("agents/main/sessions/main.jsonl"),
            format!(
                "{{\"cwd\":\"{}\",\"log\":\"{}\"}}\n",
                source_home.join("workspace").display(),
                source_home.join("logs/app.log").display()
            ),
        )
        .unwrap();
        fs::write(
            source_home.join("openclaw.json.bak"),
            format!("backup={}\n", source_home.display()),
        )
        .unwrap();
        fs::write(source_home.join("gateway.pid"), "4242\n").unwrap();
        fs::write(source_home.join("run/live.sock"), "sock\n").unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), root.join("home").display().to_string());
        env.insert(
            "OCM_HOME".to_string(),
            root.join("ocm-home").display().to_string(),
        );
        install_fake_openclaw_on_path(&root, &mut env);
        EnvironmentService::new(&env, &cwd)
            .create(CreateEnvironmentOptions {
                name: "occupied".to_string(),
                root: None,
                gateway_port: Some(18_789),
                service_enabled: false,
                service_running: false,
                default_runtime: None,
                default_launcher: None,
                dev: None,
                protected: false,
            })
            .unwrap();

        let result = migrate_plain_openclaw_home_with_sandbox_origin(
            MigrateHomeOptions {
                source_home: Some(source_home.display().to_string()),
                name: "mira".to_string(),
                root: None,
            },
            None,
            &env,
            &cwd,
        )
        .unwrap();
        let summary = result.summary;

        let target_root = PathBuf::from(summary.root);
        let target_paths = derive_env_paths(&target_root);
        assert_eq!(summary.default_launcher.as_deref(), Some("mira.migrated"));
        let launcher = LauncherService::new(&env, &cwd)
            .show("mira.migrated")
            .unwrap();
        assert_eq!(
            launcher.command,
            root.join("bin/openclaw").display().to_string()
        );
        assert!(target_paths.config_path.exists());
        let config: Value =
            serde_json::from_str(&fs::read_to_string(&target_paths.config_path).unwrap()).unwrap();
        let migrated_gateway_port = config["gateway"]["port"].as_u64().unwrap();
        assert_ne!(migrated_gateway_port, 18_789);
        assert_eq!(
            config["mcp"]["apps"]["sandboxPort"].as_u64(),
            Some(migrated_gateway_port + 1)
        );
        assert!(config["mcp"]["apps"]["sandboxOrigin"].is_null());
        assert!(result.cleared_sandbox_origin);
        assert_eq!(result.sandbox_port, Some(migrated_gateway_port as u32 + 1));
        assert!(target_paths.workspace_dir.join("notes.txt").exists());
        assert!(
            target_paths
                .state_dir
                .join("agents/main/agent/auth-profiles.json")
                .exists()
        );
        assert!(target_paths.state_dir.join("logs/app.log").exists());
        assert!(
            target_paths
                .state_dir
                .join("agents/main/sessions/main.jsonl")
                .exists()
        );
        assert!(target_paths.state_dir.join("openclaw.json.bak").exists());
        assert!(!target_paths.state_dir.join("gateway.pid").exists());
        assert!(!target_paths.state_dir.join("run").exists());

        let session_raw = fs::read_to_string(
            target_paths
                .state_dir
                .join("agents/main/sessions/main.jsonl"),
        )
        .unwrap();
        let logs_raw = fs::read_to_string(target_paths.state_dir.join("logs/app.log")).unwrap();
        let backup_raw =
            fs::read_to_string(target_paths.state_dir.join("openclaw.json.bak")).unwrap();
        assert!(session_raw.contains(&target_paths.workspace_dir.display().to_string()));
        assert!(
            session_raw.contains(
                &target_paths
                    .state_dir
                    .join("logs/app.log")
                    .display()
                    .to_string()
            )
        );
        assert!(!session_raw.contains(&source_home.display().to_string()));
        assert!(logs_raw.contains(&target_paths.workspace_dir.display().to_string()));
        assert!(!logs_raw.contains(&source_home.display().to_string()));
        assert!(backup_raw.contains(&target_paths.state_dir.display().to_string()));
        assert!(!backup_raw.contains(&source_home.display().to_string()));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_plain_openclaw_home_stays_unbound_when_openclaw_is_not_on_path() {
        let root = std::env::temp_dir().join("ocm-migrate-tests-unbound");
        let cwd = root.join("cwd");
        let source_home = root.join("legacy-home");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(source_home.join("workspace")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(source_home.join("openclaw.json"), "{}\n").unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), root.join("home").display().to_string());
        env.insert(
            "OCM_HOME".to_string(),
            root.join("ocm-home").display().to_string(),
        );
        env.insert(
            "PATH".to_string(),
            root.join("empty-bin").display().to_string(),
        );

        let summary = migrate_plain_openclaw_home(
            MigrateHomeOptions {
                source_home: Some(source_home.display().to_string()),
                name: "mira".to_string(),
                root: None,
            },
            &env,
            &cwd,
        )
        .unwrap();

        assert_eq!(summary.default_launcher, None);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migrate_plain_openclaw_home_fails_before_creating_the_env_when_the_migrated_launcher_conflicts()
     {
        let root = std::env::temp_dir().join("ocm-migrate-tests-launcher-conflict");
        let cwd = root.join("cwd");
        let source_home = root.join("legacy-home");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(source_home.join("workspace")).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(source_home.join("openclaw.json"), "{}\n").unwrap();

        let mut env = BTreeMap::new();
        env.insert("HOME".to_string(), root.join("home").display().to_string());
        env.insert(
            "OCM_HOME".to_string(),
            root.join("ocm-home").display().to_string(),
        );
        install_fake_openclaw_on_path(&root, &mut env);

        LauncherService::new(&env, &cwd)
            .add(AddLauncherOptions {
                name: "mira.migrated".to_string(),
                command: "pnpm openclaw".to_string(),
                cwd: None,
                description: None,
            })
            .unwrap();

        let error = migrate_plain_openclaw_home(
            MigrateHomeOptions {
                source_home: Some(source_home.display().to_string()),
                name: "mira".to_string(),
                root: None,
            },
            &env,
            &cwd,
        )
        .unwrap_err();

        assert!(error.contains("launcher \"mira.migrated\" already exists"));
        assert!(!root.join("ocm-home/envs/mira.json").exists());
        assert!(!root.join("ocm-home/envs/mira").exists());

        let _ = fs::remove_dir_all(&root);
    }
}
