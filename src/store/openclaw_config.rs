use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
#[cfg(any(unix, windows))]
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;
use serde_json::json;
use url::{Host, Url};

use crate::env::EnvMeta;

use super::common::{ensure_dir, path_exists, write_file_replacing_path};
use super::gateway_ports::read_port_number;
use super::layout::{EnvPaths, clean_path, derive_env_paths, display_path};
use super::openclaw_workspaces::{
    OpenClawWorkspaceInventory, load_effective_openclaw_config, normalize_agent_id,
};

#[cfg(all(test, target_os = "macos"))]
#[path = "openclaw_config_macos_tests.rs"]
mod macos_privacy_tests;

#[derive(Clone, Debug)]
pub(crate) struct OpenClawConfigAudit {
    pub status: String,
    pub issues: Vec<String>,
    pub repair_source_root: Option<PathBuf>,
    pub repair_workspace: bool,
    pub repair_gateway_port: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct OpenClawConfigRewriteOutcome {
    pub cleared_sandbox_origin: bool,
    pub sandbox_port: Option<u32>,
}

pub(crate) fn audit_openclaw_config(meta: &EnvMeta, known_envs: &[EnvMeta]) -> OpenClawConfigAudit {
    let paths = derive_env_paths(Path::new(&meta.root));
    if !path_exists(&paths.config_path) {
        return OpenClawConfigAudit {
            status: "absent".to_string(),
            issues: Vec::new(),
            repair_source_root: None,
            repair_workspace: false,
            repair_gateway_port: false,
        };
    }

    let raw = match fs::read_to_string(&paths.config_path) {
        Ok(raw) => raw,
        Err(error) => {
            return OpenClawConfigAudit {
                status: "invalid".to_string(),
                issues: vec![format!(
                    "OpenClaw config is unreadable: {} ({error})",
                    display_path(&paths.config_path)
                )],
                repair_source_root: None,
                repair_workspace: false,
                repair_gateway_port: false,
            };
        }
    };

    let value: Value = match json5::from_str(&raw) {
        Ok(value) => value,
        Err(error) => {
            return OpenClawConfigAudit {
                status: "invalid".to_string(),
                issues: vec![format!(
                    "OpenClaw config is invalid JSON/JSON5: {} ({error})",
                    display_path(&paths.config_path)
                )],
                repair_source_root: None,
                repair_workspace: false,
                repair_gateway_port: false,
            };
        }
    };

    audit_openclaw_config_value(meta, known_envs, &paths, &value)
}

pub(crate) fn repair_openclaw_config(
    meta: &EnvMeta,
    known_envs: &[EnvMeta],
) -> Result<bool, String> {
    let paths = derive_env_paths(Path::new(&meta.root));
    let Some(mut value) = read_config_value(&paths.config_path)? else {
        return Ok(false);
    };

    let audit = audit_openclaw_config_value(meta, known_envs, &paths, &value);
    if audit.status != "drifted" {
        return Ok(false);
    }

    let mut changed = false;
    if let Some(source_root) = audit.repair_source_root.as_deref() {
        changed |= rewrite_env_root_paths(&mut value, source_root, &paths.root);
    }
    if audit.repair_workspace && workspace_field_needs_repair(&value, &paths.root) {
        changed |= rewrite_workspace_field(&mut value, &paths.workspace_dir);
    }
    if audit.repair_gateway_port {
        changed |= rewrite_gateway_port_family(&mut value, meta.gateway_port.unwrap_or_default());
    }

    if !changed {
        return Ok(false);
    }

    write_config_value(&paths.config_path, &value)?;
    Ok(true)
}

pub(crate) fn rewrite_openclaw_config_for_target(
    target_paths: &EnvPaths,
    source_root: Option<&Path>,
    gateway_port: Option<u32>,
) -> Result<(), String> {
    rewrite_openclaw_config_with_root_mapping(
        target_paths,
        source_root.map(|source_root| (source_root, target_paths.root.as_path())),
        gateway_port,
        SandboxOriginPolicy::Preserve,
    )
    .map(|_| ())
}

pub(crate) fn rewrite_openclaw_config_for_new_environment(
    target_paths: &EnvPaths,
    source_root: Option<&Path>,
    gateway_port: Option<u32>,
    sandbox_origin: Option<&str>,
) -> Result<OpenClawConfigRewriteOutcome, String> {
    rewrite_openclaw_config_with_root_mapping(
        target_paths,
        source_root.map(|source_root| (source_root, target_paths.root.as_path())),
        gateway_port,
        SandboxOriginPolicy::NewEnvironment(sandbox_origin),
    )
}

pub(crate) fn rewrite_openclaw_config_for_migration(
    target_paths: &EnvPaths,
    source_state_root: &Path,
    gateway_port: Option<u32>,
    sandbox_origin: Option<&str>,
) -> Result<OpenClawConfigRewriteOutcome, String> {
    rewrite_openclaw_config_with_root_mapping(
        target_paths,
        Some((source_state_root, target_paths.state_dir.as_path())),
        gateway_port,
        SandboxOriginPolicy::NewEnvironment(sandbox_origin),
    )
}

pub(crate) fn rewrite_external_workspace_paths_for_migration(
    config_path: &Path,
    default_workspace: Option<&Path>,
    agent_workspaces: &BTreeMap<String, PathBuf>,
) -> Result<bool, String> {
    if default_workspace.is_none() && agent_workspaces.is_empty() {
        return Ok(false);
    }
    let Some(mut value) = read_config_value(config_path)? else {
        return Ok(false);
    };
    let Some(agents) = value.get_mut("agents").and_then(Value::as_object_mut) else {
        return Ok(false);
    };

    let mut changed = false;
    if let Some(default_workspace) = default_workspace {
        changed |= set_workspace_path(agents.get_mut("defaults"), default_workspace);
    }
    if let Some(entries) = agents.get_mut("entries").and_then(Value::as_object_mut) {
        for (agent_id, entry) in entries {
            let Some(workspace) = agent_workspaces.get(&normalize_agent_id(agent_id)) else {
                continue;
            };
            changed |= set_workspace_path(Some(entry), workspace);
        }
    }
    if let Some(entries) = agents.get_mut("list").and_then(Value::as_array_mut) {
        for entry in entries {
            let Some(agent_id) = entry
                .as_object()
                .and_then(|entry| entry.get("id"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some(workspace) = agent_workspaces.get(&normalize_agent_id(agent_id)) else {
                continue;
            };
            changed |= set_workspace_path(Some(entry), workspace);
        }
    }

    if changed {
        write_config_value(config_path, &value)?;
    }
    Ok(changed)
}

fn set_workspace_path(value: Option<&mut Value>, workspace: &Path) -> bool {
    let Some(value) = value.and_then(Value::as_object_mut) else {
        return false;
    };
    let workspace = display_path(workspace);
    if value
        .get("workspace")
        .and_then(Value::as_str)
        .is_some_and(|current| current == workspace)
    {
        return false;
    }
    value.insert("workspace".to_string(), Value::String(workspace));
    true
}

pub(crate) fn normalize_new_environment_sandbox_origin(
    sandbox_origin: Option<&str>,
) -> Result<Option<String>, String> {
    sandbox_origin.map(normalize_sandbox_origin).transpose()
}

pub(crate) fn reject_include_owned_sandbox_origin(config_path: &Path) -> Result<(), String> {
    let Some(value) = read_config_value(config_path)? else {
        return Ok(());
    };
    let Some(scope) = sandbox_origin_include_scope(&value) else {
        return Ok(());
    };
    Err(format!(
        "cannot safely reset mcp.apps.sandboxOrigin because OpenClaw config uses $include at {scope}; flatten that section before creating a new OCM environment"
    ))
}

pub(crate) fn reject_include_owned_agent_workspaces(config_path: &Path) -> Result<(), String> {
    let Some(value) = read_config_value(config_path)? else {
        return Ok(());
    };
    let Some(scope) = agent_workspaces_include_scope(&value) else {
        return Ok(());
    };
    Err(format!(
        "cannot safely rewrite OpenClaw agent workspaces because config uses $include at {scope}; flatten the agents section before creating a new OCM environment"
    ))
}

pub(crate) fn openclaw_config_uses_includes(config_path: &Path) -> Result<bool, String> {
    Ok(read_config_value(config_path)?
        .as_ref()
        .is_some_and(value_contains_include))
}

pub(crate) fn openclaw_config_include_paths(
    config_path: &Path,
    state_dir: &Path,
) -> Result<Vec<PathBuf>, String> {
    let Some(resolved) = load_effective_openclaw_config(config_path)? else {
        return Ok(Vec::new());
    };
    if let Some(path) = resolved
        .include_paths
        .iter()
        .find(|path| !path.starts_with(state_dir))
    {
        return Err(format!(
            "OpenClaw include resolves outside the state directory: {}",
            display_path(path)
        ));
    }
    Ok(resolved.include_paths.into_iter().collect())
}

#[derive(Clone, Copy)]
enum SandboxOriginPolicy<'a> {
    Preserve,
    NewEnvironment(Option<&'a str>),
    Simulation,
}

fn rewrite_openclaw_config_with_root_mapping(
    target_paths: &EnvPaths,
    root_mapping: Option<(&Path, &Path)>,
    gateway_port: Option<u32>,
    sandbox_origin_policy: SandboxOriginPolicy<'_>,
) -> Result<OpenClawConfigRewriteOutcome, String> {
    let config_is_symlink = fs::symlink_metadata(&target_paths.config_path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false);
    let mut value = match read_config_value(&target_paths.config_path)? {
        Some(value) => value,
        None if matches!(
            sandbox_origin_policy,
            SandboxOriginPolicy::NewEnvironment(Some(_))
        ) =>
        {
            json!({})
        }
        None => return Ok(OpenClawConfigRewriteOutcome::default()),
    };

    let mut changed = false;
    let mut outcome = OpenClawConfigRewriteOutcome::default();
    if let Some((source_root, replacement_root)) = root_mapping {
        changed |= rewrite_env_root_paths(&mut value, source_root, replacement_root);
    }
    changed |= rewrite_workspace_field_if_env_scoped(
        &mut value,
        &target_paths.root,
        &target_paths.workspace_dir,
    );
    if let Some(gateway_port) = gateway_port {
        changed |= rewrite_gateway_port_family(&mut value, gateway_port);
    }
    if let SandboxOriginPolicy::NewEnvironment(sandbox_origin) = sandbox_origin_policy {
        changed |=
            rewrite_sandbox_origin_for_new_environment(&mut value, sandbox_origin, &mut outcome)?;
        if outcome.cleared_sandbox_origin {
            outcome.sandbox_port = effective_sandbox_port(&value, gateway_port);
        }
    } else if matches!(sandbox_origin_policy, SandboxOriginPolicy::Simulation) {
        let gateway_port =
            gateway_port.ok_or_else(|| "simulation clone requires a gateway port".to_string())?;
        changed |= apply_simulation_identity_overlay(&mut value, target_paths, gateway_port)?;
    }

    if !changed && !config_is_symlink {
        return Ok(outcome);
    }

    write_config_value(&target_paths.config_path, &value)?;
    Ok(outcome)
}

pub(crate) fn rewrite_openclaw_config_for_simulation(
    target_paths: &EnvPaths,
    source_root: Option<&Path>,
    gateway_port: u32,
) -> Result<(), String> {
    rewrite_openclaw_config_with_root_mapping(
        target_paths,
        source_root.map(|source_root| (source_root, target_paths.root.as_path())),
        Some(gateway_port),
        SandboxOriginPolicy::Simulation,
    )
    .map(|_| ())
}

pub(crate) fn rewrite_openclaw_config_includes_for_target(
    config_path: &Path,
    state_dir: &Path,
    source_root: &Path,
    target_root: &Path,
) -> Result<bool, String> {
    let include_paths = openclaw_config_include_paths(config_path, state_dir)?;
    let mut changed = false;
    for include_path in include_paths {
        let raw = fs::read_to_string(&include_path).map_err(|error| {
            format!(
                "failed to read OpenClaw include {}: {error}",
                display_path(&include_path)
            )
        })?;
        let mut value: Value = json5::from_str(&raw).map_err(|error| {
            format!(
                "failed to parse OpenClaw include {}: {error}",
                display_path(&include_path)
            )
        })?;
        if rewrite_env_root_paths(&mut value, source_root, target_root) {
            write_config_value(&include_path, &value)?;
            changed = true;
        }
    }
    Ok(changed)
}

pub(crate) fn rewrite_identity_bound_workspace_paths_for_target(
    config_path: &Path,
    source_workspaces: &OpenClawWorkspaceInventory,
    source_root: &Path,
    target_root: &Path,
) -> Result<bool, String> {
    let Some(mut value) = read_config_value(config_path)? else {
        return Ok(false);
    };
    let Some(agents) = value.get_mut("agents").and_then(Value::as_object_mut) else {
        return Ok(false);
    };

    let mut changed = false;
    let default_identity_bound = agents
        .get("defaults")
        .and_then(Value::as_object)
        .and_then(|defaults| defaults.get("workspace"))
        .and_then(Value::as_str)
        .is_some_and(workspace_uses_changing_runtime_identity);
    if default_identity_bound
        && let Some(source_workspace) = source_workspaces.default_agent_workspace()
    {
        let target_workspace = rebase_workspace_path(source_workspace, source_root, target_root)?;
        agents
            .get_mut("defaults")
            .and_then(Value::as_object_mut)
            .expect("defaults remained an object")
            .insert(
                "workspace".to_string(),
                Value::String(display_path(&target_workspace)),
            );
        changed = true;
    }

    if let Some(entries) = agents.get_mut("entries").and_then(Value::as_object_mut) {
        for (agent_id, entry) in entries {
            let Some(entry) = entry.as_object_mut() else {
                continue;
            };
            let identity_bound = entry
                .get("workspace")
                .and_then(Value::as_str)
                .is_some_and(workspace_uses_changing_runtime_identity);
            if !identity_bound {
                continue;
            }
            let Some(source_workspace) = source_workspaces.agent_workspace(agent_id) else {
                continue;
            };
            let target_workspace =
                rebase_workspace_path(source_workspace, source_root, target_root)?;
            entry.insert(
                "workspace".to_string(),
                Value::String(display_path(&target_workspace)),
            );
            changed = true;
        }
    }

    if let Some(entries) = agents.get_mut("list").and_then(Value::as_array_mut) {
        for entry in entries {
            let Some(entry) = entry.as_object_mut() else {
                continue;
            };
            let Some(agent_id) = entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            let identity_bound = entry
                .get("workspace")
                .and_then(Value::as_str)
                .is_some_and(workspace_uses_changing_runtime_identity);
            if !identity_bound {
                continue;
            }
            let Some(source_workspace) = source_workspaces.agent_workspace(agent_id) else {
                continue;
            };
            let target_workspace =
                rebase_workspace_path(source_workspace, source_root, target_root)?;
            entry.insert(
                "workspace".to_string(),
                Value::String(display_path(&target_workspace)),
            );
            changed = true;
        }
    }

    if changed {
        write_config_value(config_path, &value)?;
    }
    Ok(changed)
}

fn rebase_workspace_path(
    source_workspace: &Path,
    source_root: &Path,
    target_root: &Path,
) -> Result<PathBuf, String> {
    let relative = source_workspace.strip_prefix(source_root).map_err(|_| {
        format!(
            "cannot rewrite identity-bound OpenClaw workspace outside the source environment root: {}",
            display_path(source_workspace)
        )
    })?;
    Ok(clean_path(&target_root.join(relative)))
}

fn workspace_uses_changing_runtime_identity(value: &str) -> bool {
    contains_unescaped_env_reference(value, "OCM_ACTIVE_ENV")
        || contains_unescaped_env_reference(value, "OPENCLAW_GATEWAY_PORT")
}

fn contains_unescaped_env_reference(value: &str, name: &str) -> bool {
    let token = format!("${{{name}}}");
    let escaped = format!("$${{{name}}}");
    let mut remainder = value;
    while let Some(index) = remainder.find(&token) {
        if index == 0 || !remainder[..index].ends_with('$') {
            return true;
        }
        let escaped_index = remainder.find(&escaped).unwrap_or(index);
        remainder = &remainder[escaped_index + escaped.len()..];
    }
    false
}

pub(crate) fn ensure_minimum_local_openclaw_config(
    target_paths: &EnvPaths,
    gateway_port: u32,
) -> Result<(), String> {
    ensure_local_state_paths(target_paths)?;
    let original = read_config_value(&target_paths.config_path)?;
    let value = minimum_local_config_value(
        target_paths,
        gateway_port,
        original.clone().unwrap_or_else(|| json!({})),
    )?;
    if original.as_ref() == Some(&value) {
        return Ok(());
    }
    write_config_value(&target_paths.config_path, &value)
}

pub(super) fn initialize_new_dev_openclaw_config(
    target_paths: &EnvPaths,
    gateway_port: u32,
) -> Result<bool, String> {
    match fs::symlink_metadata(&target_paths.config_path) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    #[cfg(any(unix, windows))]
    {
        ensure_local_state_paths(target_paths)?;
        let mut value = minimum_local_config_value(target_paths, gateway_port, json!({}))?;
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|error| format!("failed to generate dev Gateway authentication: {error}"))?;
        let token = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        value["gateway"]["auth"] = json!({"mode": "token", "token": token});
        publish_new_config(
            stage_private_config(&target_paths.config_path, &value)?,
            &target_paths.config_path,
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = gateway_port;
        Err("private dev config creation is unsupported on this platform".to_string())
    }
}

fn ensure_local_state_paths(target_paths: &EnvPaths) -> Result<(), String> {
    ensure_dir(&target_paths.state_dir)?;
    ensure_dir(&target_paths.workspace_dir)?;
    ensure_dir(&target_paths.state_dir.join("sessions"))
}

fn minimum_local_config_value(
    target_paths: &EnvPaths,
    gateway_port: u32,
    mut value: Value,
) -> Result<Value, String> {
    let workspace = display_path(&target_paths.workspace_dir);
    if !value.is_object() {
        value = json!({});
    }

    let root = value
        .as_object_mut()
        .ok_or_else(|| "OpenClaw config root must be an object".to_string())?;

    let gateway = ensure_object_field(root, "gateway");
    gateway
        .entry("mode".to_string())
        .or_insert_with(|| Value::String("local".to_string()));
    gateway
        .entry("bind".to_string())
        .or_insert_with(|| Value::String("loopback".to_string()));
    gateway.insert(
        "port".to_string(),
        Value::Number(serde_json::Number::from(gateway_port)),
    );

    let agents = ensure_object_field(root, "agents");
    let defaults = ensure_object_field(agents, "defaults");
    defaults
        .entry("workspace".to_string())
        .or_insert_with(|| Value::String(workspace));

    Ok(value)
}

pub(crate) fn dev_ui_gateway_url(paths: &EnvPaths, port: u32) -> Result<String, String> {
    let config = load_effective_openclaw_config(&paths.config_path)?
        .map(|config| config.value)
        .unwrap_or_else(|| json!({}));
    let gateway = config.get("gateway");
    if gateway
        .and_then(|value| value.get("tls"))
        .and_then(|value| value.get("enabled"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return Err(
            "dev UI currently requires a local HTTP Gateway; this environment enables Gateway TLS. Use --no-ui to keep using its configured Gateway"
                .to_string(),
        );
    }
    let control_ui = gateway.and_then(|value| value.get("controlUi"));
    if control_ui
        .and_then(|value| value.get("enabled"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        return Err(
            "the environment disables the Control UI; use --no-ui to keep using its configured Gateway"
                .to_string(),
        );
    }
    let base = match control_ui.and_then(|value| value.get("basePath")) {
        None | Some(Value::Null) => "",
        Some(Value::String(value)) => value.trim().trim_matches('/'),
        Some(_) => return Err("gateway.controlUi.basePath must be a URL path".to_string()),
    };
    if base.contains(['?', '#', '\\']) || base.split('/').any(|part| matches!(part, "." | "..")) {
        return Err("gateway.controlUi.basePath must be a URL path without a query, fragment, or dot segments".to_string());
    }
    let mut url =
        Url::parse(&format!("http://127.0.0.1:{port}/")).map_err(|error| error.to_string())?;
    if !base.is_empty() {
        url.set_path(&format!("/{base}/"));
    }
    Ok(url.to_string())
}

pub(crate) fn clear_skip_bootstrap_for_openclaw_onboarding(
    target_paths: &EnvPaths,
) -> Result<bool, String> {
    if !path_exists(&target_paths.config_path) {
        return Ok(false);
    }
    let raw = fs::read_to_string(&target_paths.config_path).map_err(|error| error.to_string())?;
    let Ok(mut value) = serde_json::from_str::<Value>(&raw) else {
        return Ok(false);
    };

    let Some(defaults) = value
        .get_mut("agents")
        .and_then(Value::as_object_mut)
        .and_then(|agents| agents.get_mut("defaults"))
        .and_then(Value::as_object_mut)
    else {
        return Ok(false);
    };

    if defaults.get("skipBootstrap") != Some(&Value::Bool(true)) {
        return Ok(false);
    }

    defaults.remove("skipBootstrap");
    write_config_value(&target_paths.config_path, &value)?;
    Ok(true)
}

fn audit_openclaw_config_value(
    meta: &EnvMeta,
    known_envs: &[EnvMeta],
    paths: &EnvPaths,
    value: &Value,
) -> OpenClawConfigAudit {
    let mut issues = Vec::new();
    let foreign_roots = collect_foreign_env_roots(meta, known_envs, value);
    let inferred_roots = collect_inferred_foreign_env_roots(&paths.root, value);
    let known_roots = foreign_roots
        .values()
        .map(|(root, _)| root.clone())
        .collect::<BTreeSet<_>>();
    for (env_name, (root, count)) in &foreign_roots {
        push_issue(
            &mut issues,
            format!(
                "OpenClaw config contains {count} path(s) under env \"{env_name}\" root: {}",
                display_path(root)
            ),
        );
    }
    for (root, count) in &inferred_roots {
        if known_roots.contains(root) {
            continue;
        }
        push_issue(
            &mut issues,
            format!(
                "OpenClaw config contains {count} env-scoped path(s) outside the current env root: {}",
                display_path(root)
            ),
        );
    }

    let mut repair_workspace = false;
    if let Some(workspace) = read_workspace_field(value) {
        let workspace_path = Path::new(workspace);
        if workspace_path.is_absolute()
            && !workspace_path.starts_with(&paths.root)
            && looks_env_scoped_workspace(workspace_path)
        {
            repair_workspace = true;
            if matching_foreign_env_root(meta, known_envs, workspace_path).is_none() {
                push_issue(
                    &mut issues,
                    format!(
                        "OpenClaw config workspace points outside env root: {}",
                        display_path(workspace_path)
                    ),
                );
            }
        }
    }

    let mut repair_gateway_port = false;
    if !meta.gateway_port_auto_assigned
        && let Some(expected_port) = meta.gateway_port
        && let Some(actual_port) = read_gateway_port(value)
        && actual_port != expected_port
    {
        repair_gateway_port = true;
        push_issue(
            &mut issues,
            format!(
                "OpenClaw config gateway port {actual_port} does not match env metadata {expected_port}"
            ),
        );
    }

    let mut repair_roots = known_roots;
    repair_roots.extend(inferred_roots.keys().cloned());
    let repair_source_root = if repair_roots.len() == 1 {
        repair_roots.into_iter().next()
    } else {
        None
    };

    let status = if issues.is_empty() { "ok" } else { "drifted" }.to_string();
    OpenClawConfigAudit {
        status,
        issues,
        repair_source_root,
        repair_workspace,
        repair_gateway_port,
    }
}

fn read_config_value(config_path: &Path) -> Result<Option<Value>, String> {
    match fs::symlink_metadata(config_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }

    let raw = fs::read_to_string(config_path).map_err(|error| error.to_string())?;
    let value = json5::from_str(&raw).map_err(|error| error.to_string())?;
    Ok(Some(value))
}

fn ensure_object_field<'a>(
    object: &'a mut serde_json::Map<String, Value>,
    key: &str,
) -> &'a mut serde_json::Map<String, Value> {
    let needs_reset = !object.get(key).is_some_and(Value::is_object);
    if needs_reset {
        object.insert(key.to_string(), Value::Object(serde_json::Map::new()));
    }
    object
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("object field must exist after reset")
}

fn write_config_value(config_path: &Path, value: &Value) -> Result<(), String> {
    // Preserve private access when replacing the file. Other authored
    // files retain the existing writer's access and replacement behavior.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        match fs::symlink_metadata(config_path) {
            Ok(metadata)
                if metadata.is_file()
                    && metadata.uid() == unsafe { libc::geteuid() }
                    && metadata.permissions().mode() & 0o077 == 0 =>
            {
                let private_acl = {
                    #[cfg(target_os = "macos")]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        let file = fs::OpenOptions::new()
                            .read(true)
                            .custom_flags(libc::O_NOFOLLOW)
                            .open(config_path)
                            .map_err(|error| error.to_string())?;
                        let opened = file.metadata().map_err(|error| error.to_string())?;
                        if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
                            return Err("OpenClaw config changed while inspecting private access"
                                .to_string());
                        }
                        crate::infra::macos_security::file_has_no_extended_acl(&file)
                            .map_err(|error| error.to_string())?
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        true
                    }
                };
                if private_acl {
                    let staged = stage_private_config(config_path, value)?;
                    staged
                        .as_file()
                        .set_permissions(metadata.permissions())
                        .map_err(|error| error.to_string())?;
                    return replace_private_config(staged, config_path);
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    #[cfg(windows)]
    if crate::infra::windows_security::file_has_user_only_access(config_path)
        .map_err(|error| error.to_string())?
    {
        return replace_private_config(stage_private_config(config_path, value)?, config_path);
    }
    let mut rewritten = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    rewritten.push('\n');
    write_file_replacing_path(config_path, rewritten.as_bytes())
}

#[cfg(any(unix, windows))]
fn replace_private_config(
    staged: tempfile::NamedTempFile,
    config_path: &Path,
) -> Result<(), String> {
    let mut staged = staged.into_temp_path();
    super::common::replace_path(&staged, config_path)?;
    staged.disable_cleanup(true);
    Ok(())
}

#[cfg(any(unix, windows))]
fn stage_private_config(
    config_path: &Path,
    value: &Value,
) -> Result<tempfile::NamedTempFile, String> {
    let parent = config_path
        .parent()
        .ok_or_else(|| "OpenClaw config path has no parent".to_string())?;
    ensure_dir(parent)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix(".openclaw-config-");
    // Establish protection at creation, before writing any authentication bytes.
    #[cfg(not(any(windows, target_os = "macos")))]
    let mut staged = builder
        .tempfile_in(parent)
        .map_err(|error| error.to_string())?;
    #[cfg(windows)]
    let mut staged = builder
        .make_in(
            parent,
            crate::infra::windows_security::create_private_file_new,
        )
        .map_err(|error| error.to_string())?;
    #[cfg(target_os = "macos")]
    let mut staged = builder
        .make_in(
            parent,
            crate::infra::macos_security::create_private_file_new,
        )
        .map_err(|error| error.to_string())?;
    write_private_config(&mut staged, value)?;
    Ok(staged)
}

#[cfg(any(unix, windows))]
fn write_private_config(staged: &mut tempfile::NamedTempFile, value: &Value) -> Result<(), String> {
    // Verify protection while the empty file has a cleanup owner.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let metadata = staged
            .as_file()
            .metadata()
            .map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err("filesystem did not apply private OpenClaw config access".to_string());
        }
        #[cfg(target_os = "macos")]
        if !crate::infra::macos_security::file_has_no_extended_acl(staged.as_file())
            .map_err(|error| error.to_string())?
        {
            return Err("filesystem did not apply private OpenClaw config access".to_string());
        }
    }
    #[cfg(windows)]
    if !crate::infra::windows_security::has_user_only_access(staged.as_file())
        .map_err(|error| error.to_string())?
    {
        return Err("filesystem did not apply private OpenClaw config access".to_string());
    }
    serde_json::to_writer_pretty(&mut *staged, value).map_err(|error| error.to_string())?;
    staged.write_all(b"\n").map_err(|error| error.to_string())
}

#[cfg(any(unix, windows))]
fn publish_new_config(staged: tempfile::NamedTempFile, config_path: &Path) -> Result<bool, String> {
    match staged.persist_noclobber(config_path) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error.to_string()),
    }
}

fn collect_foreign_env_roots(
    current: &EnvMeta,
    known_envs: &[EnvMeta],
    value: &Value,
) -> BTreeMap<String, (PathBuf, usize)> {
    let mut refs = BTreeMap::<String, (PathBuf, usize)>::new();
    collect_foreign_env_roots_inner(current, known_envs, value, &mut refs);
    refs
}

fn collect_inferred_foreign_env_roots(
    current_root: &Path,
    value: &Value,
) -> BTreeMap<PathBuf, usize> {
    let mut refs = BTreeMap::<PathBuf, usize>::new();
    collect_inferred_foreign_env_roots_inner(current_root, value, &mut refs);
    refs
}

fn collect_foreign_env_roots_inner(
    current: &EnvMeta,
    known_envs: &[EnvMeta],
    value: &Value,
    refs: &mut BTreeMap<String, (PathBuf, usize)>,
) {
    match value {
        Value::String(raw) => {
            let path = Path::new(raw);
            let Some((env_name, env_root)) = matching_foreign_env_root(current, known_envs, path)
            else {
                return;
            };
            let entry = refs
                .entry(env_name)
                .or_insert_with(|| (env_root.clone(), 0));
            entry.1 += 1;
        }
        Value::Array(values) => {
            for nested in values {
                collect_foreign_env_roots_inner(current, known_envs, nested, refs);
            }
        }
        Value::Object(values) => {
            for nested in values.values() {
                collect_foreign_env_roots_inner(current, known_envs, nested, refs);
            }
        }
        _ => {}
    }
}

fn collect_inferred_foreign_env_roots_inner(
    current_root: &Path,
    value: &Value,
    refs: &mut BTreeMap<PathBuf, usize>,
) {
    match value {
        Value::String(raw) => {
            let path = Path::new(raw);
            let Some(env_root) = inferred_foreign_env_root(current_root, path) else {
                return;
            };
            *refs.entry(env_root).or_insert(0) += 1;
        }
        Value::Array(values) => {
            for nested in values {
                collect_inferred_foreign_env_roots_inner(current_root, nested, refs);
            }
        }
        Value::Object(values) => {
            for nested in values.values() {
                collect_inferred_foreign_env_roots_inner(current_root, nested, refs);
            }
        }
        _ => {}
    }
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

fn read_workspace_field(value: &Value) -> Option<&str> {
    value
        .get("agents")?
        .get("defaults")?
        .get("workspace")?
        .as_str()
}

fn read_gateway_port(value: &Value) -> Option<u32> {
    read_port_number(value.get("gateway")?.get("port")?)
}

fn rewrite_env_root_paths(value: &mut Value, source_root: &Path, target_root: &Path) -> bool {
    match value {
        Value::String(raw) => rewrite_env_root_string(raw, source_root, target_root),
        Value::Array(values) => {
            let mut changed = false;
            for nested in values {
                changed |= rewrite_env_root_paths(nested, source_root, target_root);
            }
            changed
        }
        Value::Object(values) => {
            let mut changed = false;
            for nested in values.values_mut() {
                changed |= rewrite_env_root_paths(nested, source_root, target_root);
            }
            changed
        }
        _ => false,
    }
}

fn rewrite_env_root_string(raw: &mut String, source_root: &Path, target_root: &Path) -> bool {
    let path = Path::new(raw);
    if !path.is_absolute() {
        return false;
    }

    let suffix = if let Ok(suffix) = path.strip_prefix(source_root) {
        suffix.to_path_buf()
    } else {
        let Some(normalized_path) = normalize_path_allow_missing(path) else {
            return false;
        };
        let Some(normalized_source_root) = normalize_path_allow_missing(source_root) else {
            return false;
        };
        let Ok(suffix) = normalized_path.strip_prefix(normalized_source_root) else {
            return false;
        };
        suffix.to_path_buf()
    };

    *raw = display_path(&clean_path(&target_root.join(suffix)));
    true
}

fn normalize_path_allow_missing(path: &Path) -> Option<PathBuf> {
    let mut existing = path;
    let mut missing = Vec::<OsString>::new();
    loop {
        match fs::canonicalize(existing) {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Some(clean_path(&resolved));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(existing.file_name()?.to_os_string());
                existing = existing.parent()?;
            }
            Err(_) => return None,
        }
    }
}

fn rewrite_workspace_field_if_env_scoped(
    value: &mut Value,
    target_root: &Path,
    target_workspace_dir: &Path,
) -> bool {
    let Some(workspace) = workspace_value_mut(value) else {
        return false;
    };
    let workspace_path = Path::new(workspace);
    if !workspace_path.is_absolute() || workspace_path.starts_with(target_root) {
        return false;
    }
    if !looks_env_scoped_workspace(workspace_path) {
        return false;
    }

    *workspace = display_path(target_workspace_dir);
    true
}

fn rewrite_workspace_field(value: &mut Value, target_workspace_dir: &Path) -> bool {
    let Some(workspace) = workspace_value_mut(value) else {
        return false;
    };
    let target = display_path(target_workspace_dir);
    if workspace == &target {
        return false;
    }
    *workspace = target;
    true
}

fn workspace_field_needs_repair(value: &Value, target_root: &Path) -> bool {
    let Some(workspace) = read_workspace_field(value) else {
        return false;
    };
    let workspace_path = Path::new(workspace);
    workspace_path.is_absolute()
        && !workspace_path.starts_with(target_root)
        && looks_env_scoped_workspace(workspace_path)
}

fn workspace_value_mut(value: &mut Value) -> Option<&mut String> {
    let agents = value.get_mut("agents")?.as_object_mut()?;
    let defaults = agents.get_mut("defaults")?.as_object_mut()?;
    match defaults.get_mut("workspace")? {
        Value::String(raw) => Some(raw),
        _ => None,
    }
}

fn rewrite_gateway_port(value: &mut Value, gateway_port: u32) -> bool {
    let Some(root) = value.as_object_mut() else {
        return false;
    };
    let Some(gateway) = root.get_mut("gateway").and_then(Value::as_object_mut) else {
        return false;
    };
    if !matches!(gateway.get("port"), Some(Value::Number(_))) {
        return false;
    }
    if gateway.get("port").and_then(Value::as_u64) == Some(gateway_port as u64) {
        return false;
    }

    gateway.insert("port".to_string(), Value::from(gateway_port));
    true
}

fn rewrite_gateway_port_family(value: &mut Value, gateway_port: u32) -> bool {
    let source_gateway_port = read_gateway_port(value);
    let mut changed = source_gateway_port
        .is_some_and(|source| rewrite_port_coupled_mcp_app_sandbox(value, source, gateway_port));
    changed |= rewrite_gateway_port(value, gateway_port);
    changed
}

fn rewrite_port_coupled_mcp_app_sandbox(
    value: &mut Value,
    source_gateway_port: u32,
    target_gateway_port: u32,
) -> bool {
    if source_gateway_port == target_gateway_port {
        return false;
    }
    let Some(source_sandbox_port) = source_gateway_port.checked_add(1) else {
        return false;
    };
    let Some(target_sandbox_port) = target_gateway_port.checked_add(1) else {
        return false;
    };
    if source_sandbox_port > u16::MAX as u32 || target_sandbox_port > u16::MAX as u32 {
        return false;
    }
    let Some(apps) = value
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
        .and_then(|mcp| mcp.get_mut("apps"))
        .and_then(Value::as_object_mut)
    else {
        return false;
    };

    let configured_sandbox_port = apps.get("sandboxPort").map(read_port_number);
    let follows_gateway_port = match configured_sandbox_port {
        None => true,
        Some(Some(port)) => port == source_sandbox_port,
        Some(None) => false,
    };

    let mut changed = false;
    if configured_sandbox_port == Some(Some(source_sandbox_port)) {
        apps.insert("sandboxPort".to_string(), Value::from(target_sandbox_port));
        changed = true;
    }
    if follows_gateway_port && let Some(Value::String(origin)) = apps.get_mut("sandboxOrigin") {
        changed |= rewrite_loopback_origin_port(origin, source_sandbox_port, target_sandbox_port);
    }
    changed
}

fn rewrite_loopback_origin_port(origin: &mut String, source_port: u32, target_port: u32) -> bool {
    let preserve_trailing_slash = origin.ends_with('/');
    let Ok(mut parsed) = Url::parse(origin) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.port_or_known_default() != Some(source_port as u16)
    {
        return false;
    }

    let is_loopback = match parsed.host() {
        Some(Host::Domain(host)) => host.trim_end_matches('.').eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => {
            address.is_loopback()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback())
        }
        None => false,
    };
    if !is_loopback || parsed.set_port(Some(target_port as u16)).is_err() {
        return false;
    }

    let mut rewritten = parsed.to_string();
    if !preserve_trailing_slash && rewritten.ends_with('/') {
        rewritten.pop();
    }
    *origin = rewritten;
    true
}

fn rewrite_sandbox_origin_for_new_environment(
    value: &mut Value,
    replacement: Option<&str>,
    outcome: &mut OpenClawConfigRewriteOutcome,
) -> Result<bool, String> {
    if let Some(replacement) = replacement {
        let replacement = normalize_sandbox_origin(replacement)?;
        let root = value
            .as_object_mut()
            .ok_or_else(|| "OpenClaw config root must be an object".to_string())?;
        let mcp = ensure_object_field(root, "mcp");
        let apps = ensure_object_field(mcp, "apps");
        if apps.get("sandboxOrigin").and_then(Value::as_str) == Some(replacement.as_str()) {
            return Ok(false);
        }
        apps.insert("sandboxOrigin".to_string(), Value::String(replacement));
        return Ok(true);
    }

    let Some(apps) = value
        .get_mut("mcp")
        .and_then(Value::as_object_mut)
        .and_then(|mcp| mcp.get_mut("apps"))
        .and_then(Value::as_object_mut)
    else {
        return Ok(false);
    };
    let Some(origin) = apps.get("sandboxOrigin").and_then(Value::as_str) else {
        return Ok(false);
    };
    if is_loopback_sandbox_origin(origin) {
        return Ok(false);
    }

    outcome.cleared_sandbox_origin = true;
    apps.remove("sandboxOrigin");
    Ok(true)
}

fn effective_sandbox_port(value: &Value, gateway_port: Option<u32>) -> Option<u32> {
    match value
        .get("mcp")
        .and_then(Value::as_object)
        .and_then(|mcp| mcp.get("apps"))
        .and_then(Value::as_object)
        .and_then(|apps| apps.get("sandboxPort"))
    {
        Some(configured) => read_port_number(configured),
        None => gateway_port
            .and_then(|port| port.checked_add(1))
            .filter(|port| *port <= u16::MAX as u32),
    }
}

fn apply_simulation_identity_overlay(
    value: &mut Value,
    target_paths: &EnvPaths,
    gateway_port: u32,
) -> Result<bool, String> {
    let sandbox_port = gateway_port
        .checked_add(1)
        .filter(|port| *port <= u16::MAX as u32)
        .ok_or_else(|| "simulation sandbox port is outside the valid port range".to_string())?;
    let root = value
        .as_object_mut()
        .ok_or_else(|| "OpenClaw config root must be an object".to_string())?;
    let mut changed = false;

    let gateway = ensure_object_field(root, "gateway");
    if gateway.get("port").and_then(Value::as_u64) != Some(gateway_port as u64) {
        gateway.insert("port".to_string(), Value::from(gateway_port));
        changed = true;
    }

    let agents = ensure_object_field(root, "agents");
    let defaults = ensure_object_field(agents, "defaults");
    let workspace = display_path(&target_paths.workspace_dir);
    if defaults.get("workspace").and_then(Value::as_str) != Some(workspace.as_str()) {
        defaults.insert("workspace".to_string(), Value::String(workspace));
        changed = true;
    }

    let mcp = ensure_object_field(root, "mcp");
    let apps = ensure_object_field(mcp, "apps");
    if apps.get("sandboxPort").and_then(Value::as_u64) != Some(sandbox_port as u64) {
        apps.insert("sandboxPort".to_string(), Value::from(sandbox_port));
        changed = true;
    }
    let sandbox_origin = format!("http://127.0.0.1:{sandbox_port}");
    if apps.get("sandboxOrigin").and_then(Value::as_str) != Some(sandbox_origin.as_str()) {
        apps.insert("sandboxOrigin".to_string(), Value::String(sandbox_origin));
        changed = true;
    }

    Ok(changed)
}

fn normalize_sandbox_origin(origin: &str) -> Result<String, String> {
    let parsed =
        Url::parse(origin).map_err(|error| format!("invalid --sandbox-origin: {error}"))?;
    if !is_valid_sandbox_origin_url(&parsed) {
        return Err(
            "--sandbox-origin must be an HTTP(S) origin without a path, query, or credentials"
                .to_string(),
        );
    }
    Ok(parsed.origin().ascii_serialization())
}

fn is_loopback_sandbox_origin(origin: &str) -> bool {
    let Ok(parsed) = Url::parse(origin) else {
        return false;
    };
    if !is_valid_sandbox_origin_url(&parsed) {
        return false;
    }
    match parsed.host() {
        Some(Host::Domain(host)) => host.trim_end_matches('.').eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => {
            address.is_loopback()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback())
        }
        None => false,
    }
}

fn is_valid_sandbox_origin_url(parsed: &Url) -> bool {
    matches!(parsed.scheme(), "http" | "https")
        && parsed.host().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.path() == "/"
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

fn sandbox_origin_include_scope(value: &Value) -> Option<&'static str> {
    let root = value.as_object()?;
    if root.contains_key("$include") {
        return Some("the config root");
    }
    let mcp = root.get("mcp")?.as_object()?;
    if mcp.contains_key("$include") {
        return Some("mcp");
    }
    let apps = mcp.get("apps")?.as_object()?;
    if apps.contains_key("$include") {
        return Some("mcp.apps");
    }
    apps.get("sandboxOrigin")
        .and_then(Value::as_object)
        .is_some_and(|origin| origin.contains_key("$include"))
        .then_some("mcp.apps.sandboxOrigin")
}

fn agent_workspaces_include_scope(value: &Value) -> Option<&'static str> {
    let root = value.as_object()?;
    if root.contains_key("$include") {
        return Some("the config root");
    }
    let agents = root.get("agents")?;
    let Some(agents) = agents.as_object() else {
        return value_contains_include(agents).then_some("agents");
    };
    if agents.contains_key("$include") {
        return Some("agents");
    }
    if agents
        .get("defaults")
        .is_some_and(workspace_defaults_contains_include)
    {
        return Some("agents.defaults.workspace");
    }
    if agents
        .get("entries")
        .is_some_and(agent_entries_contains_include)
    {
        return Some("agents.entries");
    }
    agents
        .get("list")
        .is_some_and(agent_list_contains_include)
        .then_some("agents.list")
}

fn workspace_defaults_contains_include(value: &Value) -> bool {
    let Some(defaults) = value.as_object() else {
        return value_contains_include(value);
    };
    defaults.contains_key("$include")
        || defaults
            .get("workspace")
            .is_some_and(value_contains_include)
}

fn agent_entries_contains_include(value: &Value) -> bool {
    let Some(entries) = value.as_object() else {
        return value_contains_include(value);
    };
    entries.contains_key("$include")
        || entries.values().any(|entry| {
            let Some(entry) = entry.as_object() else {
                return value_contains_include(entry);
            };
            entry.contains_key("$include")
                || entry.get("workspace").is_some_and(value_contains_include)
        })
}

fn agent_list_contains_include(value: &Value) -> bool {
    let Some(entries) = value.as_array() else {
        return value_contains_include(value);
    };
    entries.iter().any(|entry| {
        let Some(entry) = entry.as_object() else {
            return value_contains_include(entry);
        };
        entry.contains_key("$include") || entry.get("workspace").is_some_and(value_contains_include)
    })
}

fn value_contains_include(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(value_contains_include),
        Value::Object(values) => {
            values.contains_key("$include") || values.values().any(value_contains_include)
        }
        _ => false,
    }
}

fn looks_env_scoped_workspace(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    components
        .windows(2)
        .any(|window| matches!(window, [".openclaw", "workspace"]))
}

fn push_issue(issues: &mut Vec<String>, issue: String) {
    if !issues.iter().any(|current| current == &issue) {
        issues.push(issue);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use time::OffsetDateTime;

    use super::*;

    fn meta(name: &str, root: &str, gateway_port: Option<u32>) -> EnvMeta {
        EnvMeta {
            upgrade_independent_paths: Vec::new(),
            kind: "ocm-env".to_string(),
            name: name.to_string(),
            root: root.to_string(),
            gateway_port,
            gateway_port_auto_assigned: false,
            service_enabled: true,
            service_running: true,
            default_runtime: None,
            default_launcher: None,
            dev: None,
            dev_ui_port: None,
            protected: false,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            last_used_at: None,
        }
    }

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ocm-openclaw-config-{label}-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ))
    }

    #[test]
    fn private_config_dev_is_unique_and_survives_rewrites() {
        let root = tempfile::tempdir().unwrap();
        let paths = derive_env_paths(&root.path().join("first"));
        assert!(initialize_new_dev_openclaw_config(&paths, 19789).unwrap());
        let original = fs::read(&paths.config_path).unwrap();
        let value: Value = serde_json::from_slice(&original).unwrap();
        let token = value["gateway"]["auth"]["token"].as_str().unwrap();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(value["gateway"]["auth"]["mode"], "token");
        assert!(paths.state_dir.join("sessions").is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&paths.config_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        #[cfg(windows)]
        assert!(
            crate::infra::windows_security::file_has_user_only_access(&paths.config_path).unwrap()
        );
        #[cfg(target_os = "macos")]
        assert!(
            crate::infra::macos_security::file_has_no_extended_acl(
                &fs::File::open(&paths.config_path).unwrap()
            )
            .unwrap()
        );
        assert!(!initialize_new_dev_openclaw_config(&paths, 19800).unwrap());
        assert!(fs::read(&paths.config_path).unwrap() == original);

        let other = derive_env_paths(&root.path().join("second"));
        assert!(initialize_new_dev_openclaw_config(&other, 19801).unwrap());
        let other_value = read_config_value(&other.config_path).unwrap().unwrap();
        assert!(other_value["gateway"]["auth"]["token"].as_str().unwrap() != token);

        ensure_minimum_local_openclaw_config(&paths, 19802).unwrap();
        let updated = read_config_value(&paths.config_path).unwrap().unwrap();
        assert!(updated["gateway"]["auth"]["token"].as_str().unwrap() == token);
        assert_eq!(updated["gateway"]["port"], 19802);
    }

    #[test]
    fn private_config_dev_initialization_preserves_authored_files() {
        let root = tempfile::tempdir().unwrap();
        for (index, raw) in [
            "// authored config without auth\n{}\n",
            r#"{"gateway":{"auth":{}}}"#,
            r#"{"gateway":{"auth":null}}"#,
            r#"{"gateway":{"auth":{"mode":"none"}}}"#,
            r#"{"gateway":{"auth":{"mode":"trusted-proxy","trustedProxy":{"userHeader":"x-user"}}}}"#,
            r#"{"gateway":{"auth":{"mode":"token","token":"synthetic-existing-token"}}}"#,
            r#"{"gateway":{"auth":{"mode":"password","password":"synthetic-existing-password"}}}"#,
            r#"{"gateway":{"auth":{"mode":"token","token":{"source":"env","provider":"default","id":"TOKEN_REF"}}}}"#,
            r#"{"gateway":{"auth":{"mode":"password","password":{"source":"file","provider":"vault","id":"password"}}}}"#,
            r#"{"$include":"./authored-config.json5"}"#,
            "null\n",
            "not valid JSON\n",
        ].into_iter().enumerate() {
            let paths = derive_env_paths(&root.path().join(index.to_string()));
            fs::create_dir_all(&paths.state_dir).unwrap();
            fs::write(&paths.config_path, raw).unwrap();
            assert!(!initialize_new_dev_openclaw_config(&paths, 19789).unwrap());
            assert!(fs::read(&paths.config_path).unwrap() == raw.as_bytes(), "authored file changed in case {index}");
        }
    }

    #[test]
    fn private_config_dev_publication_preserves_a_racing_file() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("openclaw.json");
        let staged = stage_private_config(
            &config,
            &json!({"gateway": {"auth": {"mode": "token", "token": "synthetic-new-token"}}}),
        )
        .unwrap();
        let authored = b"// concurrent writer wins\n{gateway: {auth: {mode: 'none'}}}\n";
        fs::write(&config, authored).unwrap();
        assert!(!publish_new_config(staged, &config).unwrap());
        assert!(fs::read(&config).unwrap() == authored);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn private_config_replacement_preserves_unix_modes() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let mut unprotected = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        let unprotected_path = unprotected.path().to_path_buf();
        unprotected
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(
            write_private_config(
                &mut unprotected,
                &json!({"gateway": {"auth": {"token": "synthetic-unwritten-token"}}})
            )
            .is_err()
        );
        assert_eq!(unprotected.as_file().metadata().unwrap().len(), 0);
        drop(unprotected);
        assert!(!unprotected_path.exists());
        let config = root.path().join("private.json");
        fs::write(&config, "{}").unwrap();
        for mode in [0o600, 0o400] {
            fs::set_permissions(&config, fs::Permissions::from_mode(mode)).unwrap();
            write_config_value(&config, &json!({"gateway": {"port": 19789}})).unwrap();
            assert_eq!(
                fs::metadata(&config).unwrap().permissions().mode() & 0o777,
                mode
            );
        }

        let authored = root.path().join("authored.json");
        let control = root.path().join("control.json");
        fs::write(&authored, "{}").unwrap();
        fs::set_permissions(&authored, fs::Permissions::from_mode(0o644)).unwrap();
        write_file_replacing_path(&control, b"{}\n").unwrap();
        write_config_value(&authored, &json!({"gateway": {"port": 19790}})).unwrap();
        assert_eq!(
            fs::metadata(&authored).unwrap().permissions().mode(),
            fs::metadata(&control).unwrap().permissions().mode()
        );
    }

    #[cfg(windows)]
    #[test]
    fn private_config_replacement_preserves_windows_acl() {
        let root = tempfile::tempdir().unwrap();
        let paths = derive_env_paths(&root.path().join("custom-root"));
        fs::create_dir_all(&paths.state_dir).unwrap();
        let granted = std::process::Command::new("icacls")
            .arg(&paths.state_dir)
            .args(["/grant", "*S-1-1-0:(OI)(CI)F"])
            .output()
            .unwrap();
        assert!(
            granted.status.success(),
            "could not prepare the custom-root ACL"
        );
        let mut unprotected = tempfile::Builder::new()
            .tempfile_in(&paths.state_dir)
            .unwrap();
        let unprotected_path = unprotected.path().to_path_buf();
        assert!(
            write_private_config(
                &mut unprotected,
                &json!({"gateway": {"auth": {"token": "synthetic-unwritten-token"}}}),
            )
            .is_err()
        );
        assert_eq!(unprotected.as_file().metadata().unwrap().len(), 0);
        drop(unprotected);
        assert!(!unprotected_path.exists());
        let staged = stage_private_config(
            &paths.config_path,
            &json!({"gateway": {"auth": {"mode": "token", "token": "synthetic-existing-token"}}}),
        )
        .unwrap();
        crate::infra::windows_security::assert_private_file_security(staged.as_file());
        replace_private_config(staged, &paths.config_path).unwrap();
        let original = read_config_value(&paths.config_path).unwrap().unwrap();
        ensure_minimum_local_openclaw_config(&paths, 19800).unwrap();
        assert!(
            crate::infra::windows_security::file_has_user_only_access(&paths.config_path).unwrap()
        );
        let updated = read_config_value(&paths.config_path).unwrap().unwrap();
        assert!(updated["gateway"]["auth"] == original["gateway"]["auth"]);

        let authored = paths.state_dir.join("authored.json");
        fs::write(&authored, b"{\"gateway\":{\"auth\":{\"mode\":\"none\"}}}\n").unwrap();
        assert!(!crate::infra::windows_security::file_has_user_only_access(&authored).unwrap());
        write_config_value(
            &authored,
            &json!({"gateway": {"auth": {"mode": "none"}, "port": 19801}}),
        )
        .unwrap();
        assert!(!crate::infra::windows_security::file_has_user_only_access(&authored).unwrap());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_config_replacement_excludes_inherited_macos_acl() {
        use crate::infra::macos_security::file_has_no_extended_acl;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let paths = derive_env_paths(&root.path().join("custom-root"));
        fs::create_dir_all(&paths.state_dir).unwrap();
        let granted = std::process::Command::new("chmod")
            .args([
                "+a",
                "everyone allow read,readattr,readextattr,readsecurity,file_inherit",
            ])
            .arg(&paths.state_dir)
            .output()
            .unwrap();
        assert!(
            granted.status.success(),
            "could not prepare the custom-root ACL"
        );
        let authored = paths.state_dir.join("authored.json");
        let control = paths.state_dir.join("control.json");
        for path in [&authored, &control] {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(path)
                .unwrap();
            file.write_all(b"{}\n").unwrap();
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            assert!(!file_has_no_extended_acl(&file).unwrap());
        }
        let staged = stage_private_config(
            &paths.config_path,
            &json!({"gateway": {"auth": {"mode": "token", "token": "synthetic-existing-token"}}}),
        )
        .unwrap();
        assert!(file_has_no_extended_acl(staged.as_file()).unwrap());
        replace_private_config(staged, &paths.config_path).unwrap();
        assert!(file_has_no_extended_acl(&fs::File::open(&paths.config_path).unwrap()).unwrap());
        let original = read_config_value(&paths.config_path).unwrap().unwrap();
        ensure_minimum_local_openclaw_config(&paths, 19800).unwrap();
        assert!(file_has_no_extended_acl(&fs::File::open(&paths.config_path).unwrap()).unwrap());
        let updated = read_config_value(&paths.config_path).unwrap().unwrap();
        assert!(updated["gateway"]["auth"] == original["gateway"]["auth"]);

        write_file_replacing_path(&control, b"{}\n").unwrap();
        write_config_value(&authored, &json!({"gateway": {"auth": {"mode": "none"}}})).unwrap();
        assert!(!file_has_no_extended_acl(&fs::File::open(&authored).unwrap()).unwrap());
        assert!(!file_has_no_extended_acl(&fs::File::open(&control).unwrap()).unwrap());
        assert_eq!(
            fs::metadata(&authored).unwrap().permissions().mode(),
            fs::metadata(&control).unwrap().permissions().mode()
        );
        assert!(!file_has_no_extended_acl(&fs::File::open(&paths.state_dir).unwrap()).unwrap());
    }

    #[test]
    fn minimum_config_preserves_existing_agent_collection_shape() {
        for (label, agents, retained_key, absent_key) in [
            ("list", json!({"list": []}), "list", "entries"),
            ("entries", json!({"entries": {}}), "entries", "list"),
        ] {
            let root = test_root(label);
            let paths = derive_env_paths(&root);
            fs::create_dir_all(&paths.state_dir).unwrap();
            write_config_value(&paths.config_path, &json!({"agents": agents})).unwrap();

            ensure_minimum_local_openclaw_config(&paths, 19789).unwrap();

            let value = read_config_value(&paths.config_path).unwrap().unwrap();
            assert!(value["agents"].get(retained_key).is_some());
            assert!(value["agents"].get(absent_key).is_none());
            assert_eq!(value["gateway"]["port"], 19789);
            assert_eq!(
                value["agents"]["defaults"]["workspace"],
                display_path(&paths.workspace_dir)
            );

            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn inferred_foreign_env_root_uses_the_openclaw_home_prefix() {
        let current_root = Path::new("/tmp/ocm/envs/target");
        let inferred = inferred_foreign_env_root(
            current_root,
            Path::new("/tmp/ocm/envs/source/.openclaw/workspace/notes.txt"),
        );
        assert_eq!(inferred, Some(PathBuf::from("/tmp/ocm/envs/source")));
    }

    #[test]
    fn audit_can_repair_one_inferred_foreign_env_root_without_metadata() {
        let current = meta("target", "/tmp/ocm/envs/target", Some(19790));
        let paths = derive_env_paths(Path::new(&current.root));
        let value = json!({
            "agents": {
                "defaults": {
                    "workspace": "/tmp/ocm/envs/source/.openclaw/workspace"
                }
            },
            "memory": {
                "logPath": "/tmp/ocm/envs/source/.openclaw/logs/gateway.log"
            },
            "gateway": {
                "port": 19789
            }
        });

        let audit = audit_openclaw_config_value(&current, &[], &paths, &value);
        assert_eq!(audit.status, "drifted");
        assert_eq!(
            audit.repair_source_root,
            Some(PathBuf::from("/tmp/ocm/envs/source"))
        );
        assert!(audit.repair_workspace);
        assert!(audit.repair_gateway_port);
        assert!(audit.issues.iter().any(|issue| issue.contains(
            "OpenClaw config contains 2 env-scoped path(s) outside the current env root"
        )));
    }

    #[test]
    fn audit_allows_config_to_override_an_auto_assigned_port() {
        let mut current = meta("target", "/tmp/ocm/envs/target", Some(19790));
        current.gateway_port_auto_assigned = true;
        let paths = derive_env_paths(Path::new(&current.root));
        let value = json!({"gateway": {"port": 19789}});

        let audit = audit_openclaw_config_value(&current, &[], &paths, &value);

        assert_eq!(audit.status, "ok");
        assert!(!audit.repair_gateway_port);
    }

    #[test]
    fn gateway_port_rewrite_updates_coupled_sandbox_port_and_preserves_public_origin() {
        let mut value = json!({
            "gateway": {"port": 19789},
            "mcp": {
                "apps": {
                    "sandboxPort": 19790,
                    "sandboxOrigin": "https://node.example.test:19790/"
                }
            }
        });

        assert!(rewrite_gateway_port_family(&mut value, 19900));
        assert_eq!(value["gateway"]["port"], 19900);
        assert_eq!(value["mcp"]["apps"]["sandboxPort"], 19901);
        assert_eq!(
            value["mcp"]["apps"]["sandboxOrigin"],
            "https://node.example.test:19790/"
        );
    }

    #[test]
    fn gateway_port_rewrite_preserves_independent_mcp_app_sandbox_fields() {
        let mut value = json!({
            "gateway": {"port": 19789},
            "mcp": {
                "apps": {
                    "sandboxPort": 25000,
                    "sandboxOrigin": "https://mcp-apps.example.test"
                }
            }
        });

        assert!(rewrite_gateway_port_family(&mut value, 19900));
        assert_eq!(value["gateway"]["port"], 19900);
        assert_eq!(value["mcp"]["apps"]["sandboxPort"], 25000);
        assert_eq!(
            value["mcp"]["apps"]["sandboxOrigin"],
            "https://mcp-apps.example.test"
        );
    }

    #[test]
    fn gateway_port_rewrite_leaves_explicit_origin_without_sandbox_port_unchanged() {
        let mut value = json!({
            "gateway": {"port": 19789},
            "mcp": {
                "apps": {
                    "sandboxOrigin": "https://node.example.test:19790"
                }
            }
        });

        assert!(rewrite_gateway_port_family(&mut value, 19900));
        assert_eq!(value["gateway"]["port"], 19900);
        assert!(value["mcp"]["apps"]["sandboxPort"].is_null());
        assert_eq!(
            value["mcp"]["apps"]["sandboxOrigin"],
            "https://node.example.test:19790"
        );
    }

    #[test]
    fn gateway_port_rewrite_updates_coupled_loopback_origin() {
        let mut value = json!({
            "gateway": {"port": 19789},
            "mcp": {
                "apps": {
                    "sandboxPort": 19790,
                    "sandboxOrigin": "http://127.0.0.1:19790/"
                }
            }
        });

        assert!(rewrite_gateway_port_family(&mut value, 19900));
        assert_eq!(value["gateway"]["port"], 19900);
        assert_eq!(value["mcp"]["apps"]["sandboxPort"], 19901);
        assert_eq!(
            value["mcp"]["apps"]["sandboxOrigin"],
            "http://127.0.0.1:19901/"
        );
    }

    #[test]
    fn gateway_port_rewrite_updates_default_loopback_origin_without_explicit_sandbox_port() {
        let mut value = json!({
            "gateway": {"port": 19789},
            "mcp": {
                "apps": {
                    "sandboxOrigin": "http://[::1]:19790"
                }
            }
        });

        assert!(rewrite_gateway_port_family(&mut value, 19900));
        assert_eq!(value["gateway"]["port"], 19900);
        assert!(value["mcp"]["apps"]["sandboxPort"].is_null());
        assert_eq!(value["mcp"]["apps"]["sandboxOrigin"], "http://[::1]:19901");
    }

    #[test]
    fn new_environment_origin_policy_clears_a_public_origin() {
        let mut value = json!({
            "mcp": {
                "apps": {
                    "sandboxOrigin": "https://source.example.test"
                }
            }
        });
        let mut outcome = OpenClawConfigRewriteOutcome::default();

        assert!(
            rewrite_sandbox_origin_for_new_environment(&mut value, None, &mut outcome).unwrap()
        );
        assert!(value["mcp"]["apps"]["sandboxOrigin"].is_null());
        assert!(outcome.cleared_sandbox_origin);
    }

    #[test]
    fn new_environment_origin_policy_clears_an_invalid_loopback_url() {
        let mut value = json!({
            "mcp": {
                "apps": {
                    "sandboxOrigin": "http://localhost:18790/apps"
                }
            }
        });
        let mut outcome = OpenClawConfigRewriteOutcome::default();

        assert!(
            rewrite_sandbox_origin_for_new_environment(&mut value, None, &mut outcome).unwrap()
        );
        assert!(value["mcp"]["apps"]["sandboxOrigin"].is_null());
        assert!(outcome.cleared_sandbox_origin);
    }

    #[test]
    fn new_environment_origin_policy_sets_an_explicit_target_origin() {
        let mut value = json!({});
        let mut outcome = OpenClawConfigRewriteOutcome::default();

        assert!(
            rewrite_sandbox_origin_for_new_environment(
                &mut value,
                Some("HTTPS://target.example.test:443/"),
                &mut outcome,
            )
            .unwrap()
        );
        assert_eq!(
            value["mcp"]["apps"]["sandboxOrigin"],
            "https://target.example.test"
        );
        assert!(!outcome.cleared_sandbox_origin);
    }

    #[test]
    fn new_environment_origin_policy_rejects_a_non_origin_url() {
        let mut value = json!({});
        let mut outcome = OpenClawConfigRewriteOutcome::default();

        let error = rewrite_sandbox_origin_for_new_environment(
            &mut value,
            Some("https://target.example.test/apps"),
            &mut outcome,
        )
        .unwrap_err();

        assert_eq!(
            error,
            "--sandbox-origin must be an HTTP(S) origin without a path, query, or credentials"
        );
        assert_eq!(value, json!({}));
    }

    #[test]
    fn sandbox_origin_include_scope_only_flags_governing_includes() {
        assert_eq!(
            sandbox_origin_include_scope(&json!({"$include": "./base.json5"})),
            Some("the config root")
        );
        assert_eq!(
            sandbox_origin_include_scope(&json!({"mcp": {"$include": "./mcp.json5"}})),
            Some("mcp")
        );
        assert_eq!(
            sandbox_origin_include_scope(&json!({"mcp": {"apps": {"$include": "./apps.json5"}}})),
            Some("mcp.apps")
        );
        assert_eq!(
            sandbox_origin_include_scope(
                &json!({"mcp": {"apps": {"sandboxOrigin": {"$include": "./origin.json"}}}})
            ),
            Some("mcp.apps.sandboxOrigin")
        );
        assert_eq!(
            sandbox_origin_include_scope(
                &json!({"agents": {"$include": "./agents.json5"}, "mcp": {"apps": {}}})
            ),
            None
        );
    }

    #[test]
    fn agent_workspace_include_scope_only_flags_agent_governing_includes() {
        assert_eq!(
            agent_workspaces_include_scope(&json!({"$include": "./base.json5"})),
            Some("the config root")
        );
        assert_eq!(
            agent_workspaces_include_scope(&json!({"agents": {"$include": "./agents.json5"}})),
            Some("agents")
        );
        assert_eq!(
            agent_workspaces_include_scope(
                &json!({"agents": {"defaults": {"$include": "./defaults.json5"}}})
            ),
            Some("agents.defaults.workspace")
        );
        assert_eq!(
            agent_workspaces_include_scope(
                &json!({"agents": {"entries": {"$include": "./agents.json5"}}})
            ),
            Some("agents.entries")
        );
        assert_eq!(
            agent_workspaces_include_scope(
                &json!({"agents": {"entries": {"main": {"$include": "./main.json5"}}}})
            ),
            Some("agents.entries")
        );
        assert_eq!(
            agent_workspaces_include_scope(
                &json!({"agents": {"list": [{"$include": "./main.json5"}]}})
            ),
            Some("agents.list")
        );
        assert_eq!(
            agent_workspaces_include_scope(
                &json!({"agents": {"defaults": {"workspace": "/tmp/workspace"}}})
            ),
            None
        );
        assert_eq!(
            agent_workspaces_include_scope(
                &json!({"mcp": {"$include": "./mcp.json5"}, "agents": {}})
            ),
            None
        );
        assert_eq!(
            agent_workspaces_include_scope(
                &json!({"agents": {"defaults": {"memorySearch": {"$include": "./memory.json5"}}}})
            ),
            None
        );
    }

    #[test]
    fn identity_bound_workspace_detection_ignores_escaped_placeholders() {
        assert!(workspace_uses_changing_runtime_identity(
            "${OCM_ACTIVE_ENV_ROOT}/team/${OCM_ACTIVE_ENV}-${OPENCLAW_GATEWAY_PORT}"
        ));
        assert!(!workspace_uses_changing_runtime_identity(
            "${OCM_ACTIVE_ENV_ROOT}/team/$${OCM_ACTIVE_ENV}-$${OPENCLAW_GATEWAY_PORT}"
        ));
    }

    #[test]
    fn effective_sandbox_port_prefers_a_valid_custom_listener() {
        let value = json!({"mcp": {"apps": {"sandboxPort": 25000}}});
        assert_eq!(effective_sandbox_port(&value, Some(20011)), Some(25000));
        assert_eq!(effective_sandbox_port(&json!({}), Some(20011)), Some(20012));
    }

    #[test]
    fn simulation_overlay_overrides_include_owned_identity_fields() {
        let target_paths = derive_env_paths(Path::new("/tmp/target"));
        let mut value = json!({"$include": "./base.json5"});

        assert!(apply_simulation_identity_overlay(&mut value, &target_paths, 20011).unwrap());
        assert_eq!(value["$include"], "./base.json5");
        assert_eq!(value["gateway"]["port"], 20011);
        assert_eq!(value["mcp"]["apps"]["sandboxPort"], 20012);
        assert_eq!(
            value["mcp"]["apps"]["sandboxOrigin"],
            "http://127.0.0.1:20012"
        );
        assert_eq!(
            value["agents"]["defaults"]["workspace"],
            "/tmp/target/.openclaw/workspace"
        );
    }

    #[test]
    fn gateway_port_rewrite_canonicalizes_equivalent_loopback_origins() {
        for (origin, expected) in [
            ("HTTP://LOCALHOST:19790/", "http://localhost:19901/"),
            ("http://127.1:19790", "http://127.0.0.1:19901"),
            ("http://[0:0:0:0:0:0:0:1]:19790", "http://[::1]:19901"),
            (
                "http://[::ffff:127.0.0.1]:19790",
                "http://[::ffff:7f00:1]:19901",
            ),
        ] {
            let mut value = json!({
                "gateway": {"port": 19789},
                "mcp": {"apps": {"sandboxOrigin": origin}}
            });

            assert!(rewrite_gateway_port_family(&mut value, 19900));
            assert_eq!(value["gateway"]["port"], 19900);
            assert_eq!(value["mcp"]["apps"]["sandboxOrigin"], expected);
        }
    }

    #[test]
    fn gateway_port_rewrite_preserves_loopback_origin_for_custom_sandbox_port() {
        let mut value = json!({
            "gateway": {"port": 19789},
            "mcp": {
                "apps": {
                    "sandboxPort": 25000,
                    "sandboxOrigin": "http://localhost:19790"
                }
            }
        });

        assert!(rewrite_gateway_port_family(&mut value, 19900));
        assert_eq!(value["gateway"]["port"], 19900);
        assert_eq!(value["mcp"]["apps"]["sandboxPort"], 25000);
        assert_eq!(
            value["mcp"]["apps"]["sandboxOrigin"],
            "http://localhost:19790"
        );
    }

    #[test]
    fn gateway_port_rewrite_accepts_integral_json_number_encodings() {
        for raw in [
            r#"{
                "gateway": {"port": 19789.0},
                "mcp": {
                    "apps": {
                        "sandboxPort": 19790.0,
                        "sandboxOrigin": "http://localhost:19790"
                    }
                }
            }"#,
            r#"{
                "gateway": {"port": 1.9789e4},
                "mcp": {
                    "apps": {
                        "sandboxPort": 1.979e4,
                        "sandboxOrigin": "http://localhost:19790"
                    }
                }
            }"#,
        ] {
            let mut value: Value = serde_json::from_str(raw).unwrap();

            assert!(rewrite_gateway_port_family(&mut value, 19900));
            assert_eq!(value["gateway"]["port"], 19900);
            assert_eq!(value["mcp"]["apps"]["sandboxPort"], 19901);
            assert_eq!(
                value["mcp"]["apps"]["sandboxOrigin"],
                "http://localhost:19901"
            );
        }
    }

    #[test]
    fn gateway_port_rewrite_preserves_unknown_sandbox_port_values() {
        for sandbox_port in [json!(19790.5), json!("19790")] {
            let mut value = json!({
                "gateway": {"port": 19789},
                "mcp": {
                    "apps": {
                        "sandboxPort": sandbox_port,
                        "sandboxOrigin": "http://localhost:19790"
                    }
                }
            });

            assert!(rewrite_gateway_port_family(&mut value, 19900));
            assert_eq!(value["gateway"]["port"], 19900);
            assert_eq!(value["mcp"]["apps"]["sandboxPort"], sandbox_port);
            assert_eq!(
                value["mcp"]["apps"]["sandboxOrigin"],
                "http://localhost:19790"
            );
        }
    }
}
