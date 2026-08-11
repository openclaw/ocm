use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::env::EnvMeta;
use crate::infra::archive::{EnvArchiveEntryKind, EnvArchiveOptions};

use super::common::path_exists;
use super::layout::{EnvPaths, clean_path, derive_env_paths, display_path};
use super::openclaw_workspaces::{
    OpenClawWorkspaceInventory, OpenClawWorkspaceRuntime, resolve_env_openclaw_workspaces,
};

#[derive(Clone, Debug)]
pub(crate) struct OpenClawStateAudit {
    pub issues: Vec<String>,
    pub repair_runtime_state: bool,
}

pub(crate) fn audit_openclaw_state(
    meta: &EnvMeta,
    known_envs: &[EnvMeta],
    env: &BTreeMap<String, String>,
) -> OpenClawStateAudit {
    let paths = derive_env_paths(Path::new(&meta.root));
    if !path_exists(&paths.state_dir) {
        return OpenClawStateAudit {
            issues: Vec::new(),
            repair_runtime_state: false,
        };
    }
    let runtime = OpenClawWorkspaceRuntime::for_env(&meta.name, meta.gateway_port);
    let workspaces = match resolve_archivable_workspaces(&paths, env, runtime) {
        Ok(workspaces) => workspaces,
        Err(error) => {
            return OpenClawStateAudit {
                issues: vec![format!(
                    "OpenClaw workspace inventory could not be resolved safely: {error}"
                )],
                repair_runtime_state: false,
            };
        }
    };

    let mut known_refs = BTreeMap::<String, (PathBuf, usize)>::new();
    let mut inferred_refs = BTreeMap::<PathBuf, usize>::new();
    collect_runtime_state_path_refs(
        &paths,
        &workspaces,
        meta,
        known_envs,
        &mut known_refs,
        &mut inferred_refs,
    );

    let mut issues = Vec::new();
    let known_roots = known_refs
        .values()
        .map(|(root, _)| root.clone())
        .collect::<BTreeSet<_>>();
    for (env_name, (root, count)) in &known_refs {
        push_issue(
            &mut issues,
            format!(
                "OpenClaw runtime state contains {count} copied path reference(s) under env \"{env_name}\" root: {}",
                display_path(root)
            ),
        );
    }
    for (root, count) in &inferred_refs {
        if known_roots.contains(root) {
            continue;
        }
        push_issue(
            &mut issues,
            format!(
                "OpenClaw runtime state contains {count} env-scoped path reference(s) outside the current env root: {}",
                display_path(root)
            ),
        );
    }

    OpenClawStateAudit {
        repair_runtime_state: !issues.is_empty(),
        issues,
    }
}

pub(crate) fn repair_openclaw_runtime_state(
    meta: &EnvMeta,
    env: &BTreeMap<String, String>,
) -> Result<bool, String> {
    let paths = derive_env_paths(Path::new(&meta.root));
    clear_nonportable_runtime_state(
        &paths,
        env,
        OpenClawWorkspaceRuntime::for_env(&meta.name, meta.gateway_port),
    )
}

pub(crate) fn openclaw_env_archive_options(
    paths: &EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<EnvArchiveOptions, String> {
    let workspaces = resolve_env_openclaw_workspaces(paths, env, runtime)?;
    let included_path_roots = workspaces.archive_relative_roots(&paths.root)?;
    let mut excluded_path_roots = openclaw_archive_excluded_path_roots(paths)?;
    excluded_path_roots.extend(openclaw_workspace_archive_excluded_path_roots(
        paths,
        &workspaces,
    )?);
    Ok(EnvArchiveOptions {
        should_skip_path: should_skip_openclaw_env_archive_path,
        included_path_roots,
        excluded_path_roots,
        snapshot_sqlite_files: true,
    })
}

#[cfg(test)]
pub(crate) fn openclaw_env_snapshot_archive_options(
    paths: &EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<EnvArchiveOptions, String> {
    let mut options = openclaw_env_archive_options(paths, env, runtime)?;
    options
        .included_path_roots
        .insert(Path::new(".openclaw/state").to_path_buf());
    Ok(options)
}

pub(crate) fn should_skip_openclaw_env_archive_path(
    relative_path: &Path,
    _kind: EnvArchiveEntryKind,
) -> bool {
    let components = path_components(relative_path);
    if components.first().copied() != Some(".openclaw") {
        return false;
    }

    !is_durable_openclaw_archive_path(&components)
}

pub(crate) fn prepare_migrated_runtime_state(
    paths: &EnvPaths,
    source_state_root: &Path,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<bool, String> {
    if !path_exists(&paths.state_dir) {
        return Ok(false);
    }
    let workspaces = resolve_archivable_workspaces(paths, env, runtime)?;

    let mut changed = false;
    changed |= rewrite_runtime_state_root_refs(
        &paths.state_dir,
        &paths.config_path,
        &workspaces,
        source_state_root,
        &paths.state_dir,
    )?;
    changed |= clear_volatile_runtime_state(&paths.state_dir, &paths.config_path, &workspaces)?;
    Ok(changed)
}

pub(crate) fn clear_nonportable_runtime_state(
    paths: &EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<bool, String> {
    if !path_exists(&paths.state_dir) {
        return Ok(false);
    }
    let workspaces = resolve_archivable_workspaces(paths, env, runtime)?;

    let mut changed = false;
    for relative_path in openclaw_extension_runtime_debris_roots(&paths.state_dir)? {
        remove_path(&paths.state_dir.join(relative_path))?;
        changed = true;
    }
    let entries = fs::read_dir(&paths.state_dir).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let name = entry.file_name();
        if name == OsStr::new("openclaw.json")
            || is_durable_openclaw_state_root(&name)
            || workspaces.contains(&path)
        {
            continue;
        }
        if name == OsStr::new("agents") {
            if prune_agent_runtime_state(&path, &workspaces)? {
                changed = true;
            }
            continue;
        }
        if workspaces.has_descendant(&path) {
            changed |= clear_parent_preserving_workspaces(&path, &workspaces)?;
            continue;
        }

        remove_path(&path)?;
        changed = true;
    }

    Ok(changed)
}

fn is_durable_openclaw_state_root(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some("state") | Some("plugins") | Some("extensions") | Some("npm") | Some("git")
    )
}

fn rewrite_runtime_state_root_refs(
    root: &Path,
    config_path: &Path,
    workspaces: &OpenClawWorkspaceInventory,
    source_state_root: &Path,
    target_state_root: &Path,
) -> Result<bool, String> {
    let mut changed = false;
    let source_root = display_path(source_state_root);
    let target_root = display_path(target_state_root);
    rewrite_runtime_state_root_refs_inner(
        root,
        config_path,
        workspaces,
        &source_root,
        &target_root,
        &mut changed,
    )?;
    Ok(changed)
}

fn rewrite_runtime_state_root_refs_inner(
    root: &Path,
    config_path: &Path,
    workspaces: &OpenClawWorkspaceInventory,
    source_root: &str,
    target_root: &str,
    changed: &mut bool,
) -> Result<(), String> {
    if !path_exists(root) {
        return Ok(());
    }

    let entries = fs::read_dir(root).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path == config_path || workspaces.contains(&path) {
            continue;
        }

        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() {
            rewrite_runtime_state_root_refs_inner(
                &path,
                config_path,
                workspaces,
                source_root,
                target_root,
                changed,
            )?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }

        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let rewritten = raw.replace(source_root, target_root);
        if rewritten == raw {
            continue;
        }
        fs::write(&path, rewritten).map_err(|error| error.to_string())?;
        *changed = true;
    }

    Ok(())
}

fn clear_volatile_runtime_state(
    root: &Path,
    config_path: &Path,
    workspaces: &OpenClawWorkspaceInventory,
) -> Result<bool, String> {
    if !path_exists(root) {
        return Ok(false);
    }

    let mut changed = false;
    let entries = fs::read_dir(root).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if path == config_path || workspaces.contains(&path) {
            continue;
        }

        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() && workspaces.has_descendant(&path) {
            changed |= clear_volatile_runtime_state(&path, config_path, workspaces)?;
            continue;
        }
        let file_name = entry.file_name();
        if should_remove_volatile_runtime_path(&file_name, metadata.is_dir()) {
            remove_path(&path)?;
            changed = true;
            continue;
        }

        if metadata.is_dir() {
            changed |= clear_volatile_runtime_state(&path, config_path, workspaces)?;
        }
    }

    Ok(changed)
}

fn clear_parent_preserving_workspaces(
    root: &Path,
    workspaces: &OpenClawWorkspaceInventory,
) -> Result<bool, String> {
    if workspaces.contains(root) {
        return Ok(false);
    }

    let mut changed = false;
    let entries = fs::read_dir(root).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if workspaces.contains(&path) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.is_dir() && workspaces.has_descendant(&path) {
            changed |= clear_parent_preserving_workspaces(&path, workspaces)?;
            continue;
        }
        remove_path(&path)?;
        changed = true;
    }
    Ok(changed)
}

fn prune_agent_runtime_state(
    agents_root: &Path,
    workspaces: &OpenClawWorkspaceInventory,
) -> Result<bool, String> {
    if !path_exists(agents_root) {
        return Ok(false);
    }

    let mut changed = false;
    let agent_entries = fs::read_dir(agents_root).map_err(|error| error.to_string())?;
    for entry in agent_entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let agent_root = entry.path();
        if workspaces.contains(&agent_root) {
            continue;
        }
        let metadata = fs::symlink_metadata(&agent_root).map_err(|error| error.to_string())?;
        if !metadata.is_dir() {
            remove_path(&agent_root)?;
            changed = true;
            continue;
        }

        let child_entries = fs::read_dir(&agent_root).map_err(|error| error.to_string())?;
        for child in child_entries {
            let child = child.map_err(|error| error.to_string())?;
            let child_path = child.path();
            if child.file_name() == OsStr::new("agent") || workspaces.contains(&child_path) {
                continue;
            }
            if workspaces.has_descendant(&child_path) {
                changed |= clear_parent_preserving_workspaces(&child_path, workspaces)?;
                continue;
            }
            remove_path(&child_path)?;
            changed = true;
        }

        let mut remaining = fs::read_dir(&agent_root).map_err(|error| error.to_string())?;
        if remaining.next().is_none() {
            remove_path(&agent_root)?;
            changed = true;
        }
    }

    let mut remaining = fs::read_dir(agents_root).map_err(|error| error.to_string())?;
    if remaining.next().is_none() {
        remove_path(agents_root)?;
        changed = true;
    }

    Ok(changed)
}

fn resolve_archivable_workspaces(
    paths: &EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<OpenClawWorkspaceInventory, String> {
    let workspaces = resolve_env_openclaw_workspaces(paths, env, runtime)?;
    workspaces.archive_relative_roots(&paths.root)?;
    Ok(workspaces)
}

fn should_remove_volatile_runtime_path(name: &OsStr, is_dir: bool) -> bool {
    if is_dir {
        return matches!(
            name.to_str(),
            Some("run") | Some("tmp") | Some("temp") | Some("locks")
        );
    }

    matches!(
        Path::new(name).extension().and_then(OsStr::to_str),
        Some("pid") | Some("lock") | Some("sock") | Some("socket")
    ) || matches!(
        name.to_str(),
        Some("pid") | Some("lock") | Some("sock") | Some("socket")
    )
}

fn path_components(path: &Path) -> Vec<&str> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect()
}

fn is_durable_openclaw_archive_path(components: &[&str]) -> bool {
    matches!(
        components,
        [".openclaw"]
            | [".openclaw", "workspace", ..]
            | [".openclaw", "openclaw.json"]
            | [".openclaw", "agents", ..]
            | [".openclaw", "credentials", ..]
            | [".openclaw", "cron", ..]
            | [".openclaw", "devices", ..]
            | [".openclaw", "identity", ..]
            | [".openclaw", "memory", ..]
            | [".openclaw", "plugins", ..]
            | [".openclaw", "extensions", ..]
            | [".openclaw", "npm", ..]
            | [".openclaw", "git", ..]
    )
}

fn openclaw_archive_excluded_path_roots(paths: &EnvPaths) -> Result<BTreeSet<PathBuf>, String> {
    Ok(openclaw_extension_runtime_debris_roots(&paths.state_dir)?
        .into_iter()
        .map(|path| Path::new(".openclaw").join(path))
        .collect())
}

fn openclaw_workspace_archive_excluded_path_roots(
    paths: &EnvPaths,
    workspaces: &OpenClawWorkspaceInventory,
) -> Result<BTreeSet<PathBuf>, String> {
    let archive_root = clean_path(&paths.root);
    workspaces
        .workspace_roots()
        .map(|workspace_root| {
            let workspace_root = clean_path(workspace_root);
            let relative_root = workspace_root.strip_prefix(&archive_root).map_err(|_| {
                format!(
                    "configured OpenClaw workspace is outside the environment root: {}",
                    display_path(&workspace_root)
                )
            })?;
            Ok(relative_root.join(".openclaw/tmp"))
        })
        .collect()
}

fn openclaw_extension_runtime_debris_roots(state_dir: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let extensions_root = state_dir.join("extensions");
    let entries = match fs::read_dir(&extensions_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut excluded = BTreeSet::new();

    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let plugin_root = entry.path();
        let metadata = fs::symlink_metadata(&plugin_root).map_err(|error| error.to_string())?;
        if !metadata.is_dir() {
            continue;
        }
        let children = fs::read_dir(&plugin_root)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        let has_runtime_deps_marker = children
            .iter()
            .any(|child| is_openclaw_runtime_dependency_marker(&child.file_name()));
        let plugin_relative_root = Path::new("extensions").join(entry.file_name());

        for child in children {
            let name = child.file_name();
            if is_openclaw_legacy_dependency_debris(&name)
                || (name == OsStr::new("node_modules") && has_runtime_deps_marker)
            {
                excluded.insert(plugin_relative_root.join(name));
            }
        }
    }

    Ok(excluded)
}

fn is_openclaw_runtime_dependency_marker(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name == ".openclaw-runtime-deps.json"
            || name == ".openclaw-runtime-deps-stamp.json"
            || name.starts_with(".openclaw-runtime-deps-")
    })
}

fn is_openclaw_legacy_dependency_debris(name: &OsStr) -> bool {
    is_openclaw_runtime_dependency_marker(name)
        || name == OsStr::new(".openclaw-pnpm-store")
        || name == OsStr::new(".openclaw-install-backups")
        || name
            .to_str()
            .is_some_and(|name| name.starts_with(".openclaw-install-stage"))
}

fn collect_runtime_state_path_refs(
    paths: &EnvPaths,
    workspaces: &OpenClawWorkspaceInventory,
    current: &EnvMeta,
    known_envs: &[EnvMeta],
    known_refs: &mut BTreeMap<String, (PathBuf, usize)>,
    inferred_refs: &mut BTreeMap<PathBuf, usize>,
) {
    visit_runtime_state_files(
        &paths.state_dir,
        &paths.config_path,
        workspaces,
        &mut |path| {
            let Ok(raw) = fs::read_to_string(path) else {
                return;
            };

            for token in candidate_path_tokens(&raw) {
                let candidate = Path::new(token);
                if let Some((env_name, env_root)) =
                    matching_foreign_env_root(current, known_envs, candidate)
                {
                    let entry = known_refs
                        .entry(env_name)
                        .or_insert_with(|| (env_root.clone(), 0));
                    entry.1 += 1;
                    continue;
                }
                if let Some(env_root) = inferred_foreign_env_root(&paths.root, candidate) {
                    *inferred_refs.entry(env_root).or_insert(0) += 1;
                }
            }
        },
    );
}

fn visit_runtime_state_files(
    root: &Path,
    config_path: &Path,
    workspaces: &OpenClawWorkspaceInventory,
    on_file: &mut dyn FnMut(&Path),
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == config_path || workspaces.contains(&path) {
            continue;
        }

        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            visit_runtime_state_files(&path, config_path, workspaces, on_file);
        } else if metadata.is_file() {
            on_file(&path);
        }
    }
}

fn candidate_path_tokens(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
            )
    })
    .filter(|token| !token.is_empty())
}

fn matching_foreign_env_root(
    current: &EnvMeta,
    known_envs: &[EnvMeta],
    path: &Path,
) -> Option<(String, PathBuf)> {
    if !path.is_absolute() {
        return None;
    }

    for env in known_envs {
        if env.name == current.name {
            continue;
        }
        let env_root = clean_path(Path::new(&env.root));
        if path.starts_with(&env_root) {
            return Some((env.name.clone(), env_root));
        }
    }
    None
}

fn inferred_foreign_env_root(current_root: &Path, path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }

    let path = clean_path(path);
    if path.starts_with(current_root) {
        return None;
    }

    let mut prefix = PathBuf::new();
    for component in path.components() {
        if component.as_os_str() == ".openclaw" {
            if prefix.as_os_str().is_empty() {
                return None;
            }
            return Some(clean_path(&prefix));
        }
        prefix.push(component.as_os_str());
    }

    None
}

fn remove_path(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.is_dir() {
        fs::remove_dir_all(path).map_err(|error| error.to_string())
    } else {
        fs::remove_file(path).map_err(|error| error.to_string())
    }
}

fn push_issue(issues: &mut Vec<String>, issue: String) {
    if !issues.iter().any(|current| current == &issue) {
        issues.push(issue);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use time::OffsetDateTime;

    use super::*;

    fn meta(name: &str, root: &str) -> EnvMeta {
        EnvMeta {
            kind: "ocm-env".to_string(),
            name: name.to_string(),
            root: root.to_string(),
            gateway_port: None,
            gateway_port_auto_assigned: false,
            service_enabled: true,
            service_running: true,
            default_runtime: None,
            default_launcher: None,
            dev: None,
            protected: false,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            last_used_at: None,
        }
    }

    #[test]
    fn audit_detects_foreign_env_refs_in_runtime_state() {
        let temp =
            std::env::temp_dir().join(format!("ocm-openclaw-state-audit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let source_root = temp.join("source");
        let target_root = temp.join("target");
        fs::create_dir_all(target_root.join(".openclaw/agents/main/sessions")).unwrap();
        fs::write(
            target_root.join(".openclaw/agents/main/sessions/main.jsonl"),
            format!(
                "{{\"cwd\":\"{}\"}}\n",
                source_root.join(".openclaw/workspace").display()
            ),
        )
        .unwrap();

        let current = meta("target", &display_path(&target_root));
        let known_envs = vec![meta("source", &display_path(&source_root)), current.clone()];
        let audit = audit_openclaw_state(&current, &known_envs, &BTreeMap::new());
        assert!(audit.repair_runtime_state);
        assert!(audit.issues.iter().any(|issue| issue.contains(
            "OpenClaw runtime state contains 1 copied path reference(s) under env \"source\" root"
        )));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn clear_nonportable_runtime_state_preserves_config_workspaces_auth_and_plugin_payloads() {
        let temp =
            std::env::temp_dir().join(format!("ocm-openclaw-state-clear-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let paths = derive_env_paths(&temp);
        fs::create_dir_all(paths.workspace_dir.join("notes")).unwrap();
        fs::create_dir_all(paths.state_dir.join("workspace-clawforce/skills")).unwrap();
        fs::write(
            &paths.config_path,
            r#"{"agents":{"list":[{"id":"main","default":true},{"id":"clawforce"}]}}"#,
        )
        .unwrap();
        fs::create_dir_all(paths.state_dir.join("agents/main/agent")).unwrap();
        fs::create_dir_all(paths.state_dir.join("agents/main/sessions")).unwrap();
        fs::write(
            paths.state_dir.join("agents/main/agent/auth-profiles.json"),
            "{}\n",
        )
        .unwrap();
        for path in [
            "plugins/installs.json",
            "extensions/demo/openclaw.plugin.json",
            "extensions/demo/node_modules/package/index.js",
            "extensions/generated/openclaw.plugin.json",
            "npm/projects/demo/package-lock.json",
            "git/git-demo/repo/.git/HEAD",
        ] {
            let path = paths.state_dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "keep\n").unwrap();
        }
        for path in [
            "extensions/generated/.openclaw-runtime-deps.json",
            "extensions/generated/.openclaw-install-backups/backup.json",
            "extensions/generated/node_modules/package/index.js",
        ] {
            let path = paths.state_dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "remove\n").unwrap();
        }
        fs::write(paths.state_dir.join("logs.txt"), "log\n").unwrap();
        fs::write(paths.workspace_dir.join("notes/todo.txt"), "keep\n").unwrap();
        fs::write(
            paths.state_dir.join("workspace-clawforce/skills/social.md"),
            "keep secondary workspace\n",
        )
        .unwrap();
        fs::create_dir_all(paths.state_dir.join("state")).unwrap();
        fs::write(
            paths.state_dir.join("state/openclaw.sqlite"),
            "paired device state must survive\n",
        )
        .unwrap();

        let changed = clear_nonportable_runtime_state(
            &paths,
            &BTreeMap::new(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert!(changed);
        assert!(paths.config_path.exists());
        assert!(paths.workspace_dir.join("notes/todo.txt").exists());
        assert!(
            paths
                .state_dir
                .join("workspace-clawforce/skills/social.md")
                .exists()
        );
        assert!(paths.state_dir.join("state/openclaw.sqlite").exists());
        assert!(
            paths
                .state_dir
                .join("agents/main/agent/auth-profiles.json")
                .exists()
        );
        for path in [
            "plugins/installs.json",
            "extensions/demo/openclaw.plugin.json",
            "extensions/demo/node_modules/package/index.js",
            "extensions/generated/openclaw.plugin.json",
            "npm/projects/demo/package-lock.json",
            "git/git-demo/repo/.git/HEAD",
        ] {
            assert!(paths.state_dir.join(path).exists(), "{path}");
        }
        for path in [
            "extensions/generated/.openclaw-runtime-deps.json",
            "extensions/generated/.openclaw-install-backups",
            "extensions/generated/node_modules",
        ] {
            assert!(!paths.state_dir.join(path).exists(), "{path}");
        }
        assert!(!paths.state_dir.join("agents/main/sessions").exists());
        assert!(!paths.state_dir.join("logs.txt").exists());

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn prepare_migrated_runtime_state_preserves_history_and_rewrites_root_refs() {
        let temp =
            std::env::temp_dir().join(format!("ocm-openclaw-state-migrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let source_state_root = temp.join("legacy-home/.openclaw");
        let paths = derive_env_paths(temp.join("target"));
        fs::create_dir_all(paths.workspace_dir.join("notes")).unwrap();
        fs::create_dir_all(paths.state_dir.join("workspace-clawforce/tmp")).unwrap();
        fs::create_dir_all(paths.state_dir.join("agents/main/agent")).unwrap();
        fs::create_dir_all(paths.state_dir.join("agents/main/sessions")).unwrap();
        fs::create_dir_all(paths.state_dir.join("logs")).unwrap();
        fs::create_dir_all(paths.state_dir.join("run")).unwrap();
        fs::write(
            &paths.config_path,
            r#"{"agents":{"list":[{"id":"main","default":true},{"id":"clawforce"}]}}"#,
        )
        .unwrap();
        fs::write(paths.workspace_dir.join("notes/todo.txt"), "keep\n").unwrap();
        fs::write(
            paths.state_dir.join("workspace-clawforce/tmp/durable.txt"),
            "keep workspace tmp\n",
        )
        .unwrap();
        fs::write(
            paths.state_dir.join("agents/main/agent/auth-profiles.json"),
            "{}\n",
        )
        .unwrap();
        fs::write(
            paths.state_dir.join("agents/main/sessions/main.jsonl"),
            format!(
                "{{\"cwd\":\"{}\",\"log\":\"{}\"}}\n",
                source_state_root.join("workspace").display(),
                source_state_root.join("logs/gateway.log").display()
            ),
        )
        .unwrap();
        fs::write(
            paths.state_dir.join("logs/gateway.log"),
            format!("source={}\n", source_state_root.join("workspace").display()),
        )
        .unwrap();
        fs::write(
            paths.state_dir.join("openclaw.json.bak"),
            format!("backup={}\n", source_state_root.display()),
        )
        .unwrap();
        fs::write(paths.state_dir.join("gateway.pid"), "4242\n").unwrap();
        fs::write(paths.state_dir.join("run/live.sock"), "sock\n").unwrap();

        let changed = prepare_migrated_runtime_state(
            &paths,
            &source_state_root,
            &BTreeMap::new(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert!(changed);
        assert!(paths.workspace_dir.join("notes/todo.txt").exists());
        assert!(
            paths
                .state_dir
                .join("workspace-clawforce/tmp/durable.txt")
                .exists()
        );
        assert!(
            paths
                .state_dir
                .join("agents/main/sessions/main.jsonl")
                .exists()
        );
        assert!(paths.state_dir.join("logs/gateway.log").exists());
        assert!(paths.state_dir.join("openclaw.json.bak").exists());
        assert!(!paths.state_dir.join("gateway.pid").exists());
        assert!(!paths.state_dir.join("run").exists());

        let session_raw =
            fs::read_to_string(paths.state_dir.join("agents/main/sessions/main.jsonl")).unwrap();
        let logs_raw = fs::read_to_string(paths.state_dir.join("logs/gateway.log")).unwrap();
        let backup_raw = fs::read_to_string(paths.state_dir.join("openclaw.json.bak")).unwrap();
        assert!(session_raw.contains(&display_path(&paths.state_dir.join("workspace"))));
        assert!(session_raw.contains(&display_path(&paths.state_dir.join("logs/gateway.log"))));
        assert!(!session_raw.contains(&display_path(&source_state_root)));
        assert!(logs_raw.contains(&display_path(&paths.state_dir.join("workspace"))));
        assert!(!logs_raw.contains(&display_path(&source_state_root)));
        assert!(backup_raw.contains(&display_path(&paths.state_dir)));
        assert!(!backup_raw.contains(&display_path(&source_state_root)));

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn archive_policy_omits_openclaw_tmp_inside_configured_workspaces() {
        let temp =
            std::env::temp_dir().join(format!("ocm-openclaw-workspace-tmp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let archive_path = temp.with_extension("tar");
        let extract_dir = temp.with_extension("extracted");
        let _ = fs::remove_file(&archive_path);
        let _ = fs::remove_dir_all(&extract_dir);
        let paths = derive_env_paths(&temp);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(
            &paths.config_path,
            format!(
                r#"{{"agents":{{"defaults":{{"workspace":"{}"}}}}}}"#,
                paths.state_dir.join("team").display()
            ),
        )
        .unwrap();

        for path in [
            "workspace/notes.txt",
            "workspace/tmp/keep.txt",
            "workspace/.openclaw/tmp/cache.bin",
            "workspace/.openclaw/tmp-keep/data.bin",
            "team/notes.txt",
            "team/.openclaw/tmp/cache.bin",
        ] {
            let path = paths.state_dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "fixture\n").unwrap();
        }

        let options = openclaw_env_snapshot_archive_options(
            &paths,
            &BTreeMap::new(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        crate::infra::archive::write_env_archive_with_options(
            &serde_json::json!({ "kind": "test" }),
            &paths.root,
            &archive_path,
            options,
        )
        .unwrap();
        let extracted = crate::infra::archive::extract_env_archive::<serde_json::Value>(
            &archive_path,
            &extract_dir,
        )
        .unwrap();

        for path in [
            "workspace/notes.txt",
            "workspace/tmp/keep.txt",
            "workspace/.openclaw/tmp-keep/data.bin",
            "team/notes.txt",
        ] {
            assert!(
                extracted.root_dir.join(".openclaw").join(path).exists(),
                "{path}"
            );
        }
        for path in [
            "workspace/.openclaw/tmp/cache.bin",
            "team/.openclaw/tmp/cache.bin",
        ] {
            assert!(
                !extracted.root_dir.join(".openclaw").join(path).exists(),
                "{path}"
            );
        }

        let _ = fs::remove_dir_all(&temp);
        let _ = fs::remove_file(&archive_path);
        let _ = fs::remove_dir_all(&extract_dir);
    }

    #[test]
    fn archive_policy_uses_configured_workspace_roots_without_prefix_guessing() {
        let temp =
            std::env::temp_dir().join(format!("ocm-openclaw-archive-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let paths = derive_env_paths(&temp);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(
            &paths.config_path,
            format!(
                r#"{{"agents":{{"defaults":{{"workspace":"{}"}},"list":[{{"id":"primary","default":true}},{{"id":"ops"}}]}}}}"#,
                paths.state_dir.join("team").display()
            ),
        )
        .unwrap();

        let export_options = openclaw_env_archive_options(
            &paths,
            &BTreeMap::new(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        let options = openclaw_env_snapshot_archive_options(
            &paths,
            &BTreeMap::new(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert!(
            options
                .included_path_roots
                .contains(Path::new(".openclaw/team"))
        );
        assert!(
            options
                .included_path_roots
                .contains(Path::new(".openclaw/workspace"))
        );
        assert!(
            options
                .included_path_roots
                .contains(Path::new(".openclaw/state"))
        );
        for options in [&export_options, &options] {
            assert!(
                options
                    .excluded_path_roots
                    .contains(Path::new(".openclaw/team/.openclaw/tmp"))
            );
            assert!(
                options
                    .excluded_path_roots
                    .contains(Path::new(".openclaw/workspace/.openclaw/tmp"))
            );
        }
        assert!(options.snapshot_sqlite_files);
        assert!(export_options.snapshot_sqlite_files);
        assert!(
            !export_options
                .included_path_roots
                .contains(Path::new(".openclaw/state"))
        );
        assert!(should_skip_openclaw_env_archive_path(
            Path::new(".openclaw/state/openclaw.sqlite"),
            EnvArchiveEntryKind::File
        ));
        assert!(!should_skip_openclaw_env_archive_path(
            Path::new(".openclaw/workspace/notes.txt"),
            EnvArchiveEntryKind::File
        ));
        for path in [
            ".openclaw/plugins/installs.json",
            ".openclaw/extensions/demo/package.json",
            ".openclaw/npm/projects/demo/package-lock.json",
            ".openclaw/npm/projects/demo/node_modules/demo/index.js",
            ".openclaw/git/git-demo/repo/.git/HEAD",
            ".openclaw/git/git-demo/repo/openclaw.plugin.json",
        ] {
            assert!(
                !should_skip_openclaw_env_archive_path(Path::new(path), EnvArchiveEntryKind::File),
                "{path} should be archived"
            );
        }
        for path in [
            ".openclaw/plugin-runtime-deps/node_modules/demo/index.js",
            ".openclaw/bundled-plugin-runtime-deps/node_modules/demo/index.js",
            ".openclaw/.openclaw-pnpm-store/v3/files/cache",
            ".openclaw/.openclaw-npm-cache/_cacache/index",
        ] {
            assert!(
                should_skip_openclaw_env_archive_path(Path::new(path), EnvArchiveEntryKind::File),
                "{path} should not be archived"
            );
        }
        assert!(should_skip_openclaw_env_archive_path(
            Path::new(".openclaw/workspace-attestations/manifest.json"),
            EnvArchiveEntryKind::File
        ));
        assert!(should_skip_openclaw_env_archive_path(
            Path::new(".openclaw/workspace-cache/data.json"),
            EnvArchiveEntryKind::File
        ));

        let _ = fs::remove_dir_all(&temp);
    }
}
