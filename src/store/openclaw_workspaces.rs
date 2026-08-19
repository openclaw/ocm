use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::path_exists;
use super::layout::{EnvPaths, clean_path, display_path};

const DEFAULT_AGENT_ID: &str = "main";
const MAX_INCLUDE_DEPTH: usize = 10;
const MAX_INCLUDE_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_INCLUDE_PATH_LENGTH: usize = 4096;
const BLOCKED_OBJECT_KEYS: [&str; 3] = ["__proto__", "prototype", "constructor"];

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct OpenClawWorkspaceRuntime<'a> {
    pub(crate) env_name: Option<&'a str>,
    pub(crate) gateway_port: Option<u32>,
}

impl<'a> OpenClawWorkspaceRuntime<'a> {
    pub(crate) fn for_env(env_name: &'a str, gateway_port: Option<u32>) -> Self {
        Self {
            env_name: Some(env_name),
            gateway_port,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EffectiveOpenClawConfig {
    pub(crate) value: Value,
    pub(crate) include_paths: BTreeSet<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenClawWorkspaceInventory {
    workspace_roots: BTreeSet<PathBuf>,
    config_include_paths: BTreeSet<PathBuf>,
    agent_workspace_roots: BTreeMap<String, PathBuf>,
    configured_default_workspace: Option<PathBuf>,
    configured_agent_workspace_roots: BTreeMap<String, PathBuf>,
    default_agent_id: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ExternalOpenClawWorkspaceSources {
    pub(crate) default_workspace: Option<PathBuf>,
    pub(crate) agent_workspaces: BTreeMap<String, PathBuf>,
}

impl OpenClawWorkspaceInventory {
    pub(crate) fn contains(&self, path: &Path) -> bool {
        let path = clean_path(path);
        self.workspace_roots
            .iter()
            .any(|root| path == *root || path.starts_with(root))
            || self.config_include_paths.contains(&path)
    }

    pub(crate) fn has_descendant(&self, path: &Path) -> bool {
        let path = clean_path(path);
        self.workspace_roots
            .iter()
            .chain(self.config_include_paths.iter())
            .any(|root| root.starts_with(&path))
    }

    pub(crate) fn archive_relative_roots(
        &self,
        archive_root: &Path,
    ) -> Result<BTreeSet<PathBuf>, String> {
        let archive_root = clean_path(archive_root);
        let canonical_archive_root = canonicalize_path_allow_missing(&archive_root)?;
        let mut relative_roots = BTreeSet::new();

        for preserved_path in self
            .workspace_roots
            .iter()
            .chain(self.config_include_paths.iter())
        {
            let canonical_preserved_path = canonicalize_path_allow_missing(preserved_path)?;
            if canonical_preserved_path == canonical_archive_root
                || !canonical_preserved_path.starts_with(&canonical_archive_root)
            {
                return Err(format!(
                    "cannot safely preserve configured OpenClaw workspace or config include that resolves outside the environment root: {} (resolved: {}; environment root resolves to: {})",
                    display_path(preserved_path),
                    display_path(&canonical_preserved_path),
                    display_path(&canonical_archive_root)
                ));
            }

            let canonical_relative = canonical_preserved_path
                .strip_prefix(&canonical_archive_root)
                .map_err(|error| error.to_string())?;
            if let Ok(relative) = preserved_path.strip_prefix(&archive_root) {
                if relative != canonical_relative {
                    return Err(format!(
                        "cannot safely preserve configured OpenClaw workspace or config include through a symlink: {}",
                        display_path(preserved_path)
                    ));
                }
                relative_roots.insert(relative.to_path_buf());
            }
            relative_roots.insert(canonical_relative.to_path_buf());
        }

        Ok(relative_roots)
    }

    pub(crate) fn default_agent_workspace(&self) -> Option<&Path> {
        self.agent_workspace_roots
            .get(&self.default_agent_id)
            .map(PathBuf::as_path)
    }

    pub(crate) fn agent_workspace(&self, agent_id: &str) -> Option<&Path> {
        self.agent_workspace_roots
            .get(&normalize_agent_id(agent_id))
            .map(PathBuf::as_path)
    }

    pub(crate) fn workspace_roots(&self) -> impl Iterator<Item = &Path> {
        self.workspace_roots.iter().map(PathBuf::as_path)
    }

    /// An adopted plain home may use a repository checkout outside its state
    /// directory. Those configured workspace roots are copied into the new
    /// environment; config includes remain owned by their original config tree.
    pub(crate) fn external_workspace_sources_for_migration(
        &self,
        source_root: &Path,
    ) -> Result<ExternalOpenClawWorkspaceSources, String> {
        let source_root = clean_path(source_root);
        let canonical_source_root = canonicalize_path_allow_missing(&source_root)?;

        for include_path in &self.config_include_paths {
            validate_internal_migration_path(
                include_path,
                &source_root,
                &canonical_source_root,
                "config include",
            )?;
        }

        let default_workspace = self
            .configured_default_workspace
            .as_deref()
            .map(|workspace| {
                migration_workspace_source(workspace, &source_root, &canonical_source_root)
            })
            .transpose()?
            .flatten();
        let mut agent_workspaces = BTreeMap::new();
        for (agent_id, workspace) in &self.configured_agent_workspace_roots {
            if let Some(source) =
                migration_workspace_source(workspace, &source_root, &canonical_source_root)?
            {
                agent_workspaces.insert(agent_id.clone(), source);
            }
        }

        Ok(ExternalOpenClawWorkspaceSources {
            default_workspace,
            agent_workspaces,
        })
    }
}

fn validate_internal_migration_path(
    path: &Path,
    source_root: &Path,
    canonical_source_root: &Path,
    label: &str,
) -> Result<(), String> {
    let canonical_path = canonicalize_path_allow_missing(path)?;
    if canonical_path == canonical_source_root || !canonical_path.starts_with(canonical_source_root)
    {
        return Err(format!(
            "cannot safely preserve configured OpenClaw {label} that resolves outside the environment root: {} (resolved: {}; environment root resolves to: {})",
            display_path(path),
            display_path(&canonical_path),
            display_path(canonical_source_root)
        ));
    }
    let canonical_relative = canonical_path
        .strip_prefix(canonical_source_root)
        .map_err(|error| error.to_string())?;
    if let Ok(relative) = path.strip_prefix(source_root)
        && relative != canonical_relative
    {
        return Err(format!(
            "cannot safely preserve configured OpenClaw {label} through a symlink: {}",
            display_path(path)
        ));
    }
    Ok(())
}

fn migration_workspace_source(
    workspace: &Path,
    source_root: &Path,
    canonical_source_root: &Path,
) -> Result<Option<PathBuf>, String> {
    let canonical_workspace = canonicalize_path_allow_missing(workspace)?;
    if canonical_workspace == canonical_source_root {
        return Err(format!(
            "configured OpenClaw workspace cannot be the state directory itself: {}",
            display_path(workspace)
        ));
    }
    if canonical_workspace.starts_with(canonical_source_root) {
        validate_internal_migration_path(
            workspace,
            source_root,
            canonical_source_root,
            "workspace",
        )?;
        return Ok(None);
    }
    let metadata = fs::metadata(&canonical_workspace).map_err(|error| {
        format!(
            "failed to inspect external OpenClaw workspace {}: {error}",
            display_path(workspace)
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "configured external OpenClaw workspace must be a directory: {}",
            display_path(workspace)
        ));
    }
    Ok(Some(canonical_workspace))
}

pub(crate) fn resolve_env_openclaw_workspaces(
    paths: &EnvPaths,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<OpenClawWorkspaceInventory, String> {
    let mut inventory = resolve_openclaw_workspaces(
        &paths.config_path,
        &paths.state_dir,
        &paths.openclaw_home,
        env,
        runtime,
    )?;
    inventory
        .workspace_roots
        .insert(paths.workspace_dir.clone());
    Ok(inventory)
}

pub(crate) fn resolve_plain_openclaw_workspaces(
    state_dir: &Path,
    env: &BTreeMap<String, String>,
) -> Result<OpenClawWorkspaceInventory, String> {
    let state_dir = canonicalize_path_allow_missing(state_dir)?;
    let openclaw_home = state_dir.parent().unwrap_or(&state_dir);
    let mut inventory = resolve_openclaw_workspaces(
        &state_dir.join("openclaw.json"),
        &state_dir,
        openclaw_home,
        env,
        OpenClawWorkspaceRuntime::default(),
    )?;
    inventory
        .workspace_roots
        .insert(state_dir.join("workspace"));
    Ok(inventory)
}

pub(crate) fn load_effective_openclaw_config(
    config_path: &Path,
) -> Result<Option<EffectiveOpenClawConfig>, String> {
    if !path_exists(config_path) {
        return Ok(None);
    }

    let raw = read_regular_text_file(config_path, "OpenClaw config", None)?;
    let value = parse_json5(&raw, config_path, "OpenClaw config")?;
    let config_root = config_path.parent().ok_or_else(|| {
        format!(
            "OpenClaw config path has no parent: {}",
            config_path.display()
        )
    })?;
    let mut processor = IncludeProcessor::new(config_root)?;
    let value = processor.process(value, config_path, 0, &mut vec![clean_path(config_path)])?;

    Ok(Some(EffectiveOpenClawConfig {
        value,
        include_paths: processor.include_paths,
    }))
}

fn resolve_openclaw_workspaces(
    config_path: &Path,
    state_dir: &Path,
    openclaw_home: &Path,
    env: &BTreeMap<String, String>,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> Result<OpenClawWorkspaceInventory, String> {
    let resolved = load_effective_openclaw_config(config_path)?;
    let config = resolved
        .as_ref()
        .map(|resolved| resolved.value.clone())
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut config_env = openclaw_config_env(env, openclaw_home, state_dir, config_path, runtime);
    apply_openclaw_config_env(&config, &mut config_env);
    let config = resolve_config_env_vars(config, &config_env);
    let config_include_paths = resolved
        .map(|resolved| resolved.include_paths)
        .unwrap_or_default();
    let entries = agent_entries(&config);
    let default_agent_id = resolve_default_agent_id(&entries);
    let default_workspace = config
        .pointer("/agents/defaults/workspace")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let configured_default_workspace = default_workspace
        .map(|workspace| resolve_workspace_path(workspace, openclaw_home))
        .transpose()?;

    let mut agent_ids = entries
        .iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .map(normalize_agent_id)
        .collect::<BTreeSet<_>>();
    agent_ids.insert(default_agent_id.clone());

    let mut workspace_roots = BTreeSet::new();
    let mut agent_workspace_roots = BTreeMap::new();
    let mut configured_agent_workspace_roots = BTreeMap::new();
    for agent_id in agent_ids {
        let explicit = entries
            .iter()
            .find(|entry| {
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| normalize_agent_id(id) == agent_id)
            })
            .and_then(|entry| entry.get("workspace"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let configured_workspace = explicit
            .map(|workspace| resolve_workspace_path(workspace, openclaw_home))
            .transpose()?;
        if let Some(workspace) = configured_workspace.as_ref() {
            configured_agent_workspace_roots.insert(agent_id.clone(), workspace.clone());
        }
        let raw = if let Some(explicit) = explicit {
            explicit.to_string()
        } else if agent_id == default_agent_id {
            default_workspace
                .map(str::to_string)
                .unwrap_or_else(|| display_path(&state_dir.join("workspace")))
        } else if let Some(default_workspace) = default_workspace {
            display_path(&resolve_workspace_path(default_workspace, openclaw_home)?.join(&agent_id))
        } else {
            display_path(&state_dir.join(format!("workspace-{agent_id}")))
        };
        let workspace = resolve_workspace_path(&raw, openclaw_home)?;
        if workspace == clean_path(state_dir) {
            return Err(format!(
                "configured OpenClaw workspace cannot be the state directory itself: {}",
                display_path(&workspace)
            ));
        }
        workspace_roots.insert(workspace.clone());
        agent_workspace_roots.insert(agent_id, workspace);
    }
    Ok(OpenClawWorkspaceInventory {
        workspace_roots,
        config_include_paths,
        agent_workspace_roots,
        configured_default_workspace,
        configured_agent_workspace_roots,
        default_agent_id,
    })
}

fn openclaw_config_env(
    env: &BTreeMap<String, String>,
    openclaw_home: &Path,
    state_dir: &Path,
    config_path: &Path,
    runtime: OpenClawWorkspaceRuntime<'_>,
) -> BTreeMap<String, String> {
    openclaw_config_env_with_platform_semantics(
        env,
        openclaw_home,
        state_dir,
        config_path,
        runtime,
        cfg!(windows),
    )
}

fn openclaw_config_env_with_platform_semantics(
    env: &BTreeMap<String, String>,
    openclaw_home: &Path,
    state_dir: &Path,
    config_path: &Path,
    runtime: OpenClawWorkspaceRuntime<'_>,
    case_insensitive: bool,
) -> BTreeMap<String, String> {
    let mut resolved = env
        .iter()
        .filter(|(key, _)| !is_runtime_owned_env_key(key, case_insensitive))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    insert_env_entry(
        &mut resolved,
        "OPENCLAW_HOME".to_string(),
        display_path(openclaw_home),
        case_insensitive,
    );
    insert_env_entry(
        &mut resolved,
        "OPENCLAW_STATE_DIR".to_string(),
        display_path(state_dir),
        case_insensitive,
    );
    insert_env_entry(
        &mut resolved,
        "OPENCLAW_CONFIG_PATH".to_string(),
        display_path(config_path),
        case_insensitive,
    );
    insert_env_entry(
        &mut resolved,
        "OPENCLAW_SERVICE_REPAIR_POLICY".to_string(),
        "external".to_string(),
        case_insensitive,
    );
    if let Some(env_name) = runtime.env_name {
        insert_env_entry(
            &mut resolved,
            "OCM_ACTIVE_ENV".to_string(),
            env_name.to_string(),
            case_insensitive,
        );
        insert_env_entry(
            &mut resolved,
            "OCM_ACTIVE_ENV_ROOT".to_string(),
            display_path(openclaw_home),
            case_insensitive,
        );
    }
    if let Some(gateway_port) = runtime.gateway_port {
        insert_env_entry(
            &mut resolved,
            "OPENCLAW_GATEWAY_PORT".to_string(),
            gateway_port.to_string(),
            case_insensitive,
        );
    }
    resolved
}

fn is_runtime_owned_env_key(key: &str, case_insensitive: bool) -> bool {
    let matches = |candidate: &str| {
        if case_insensitive {
            key.eq_ignore_ascii_case(candidate)
        } else {
            key == candidate
        }
    };
    let openclaw_owned = if case_insensitive {
        key.get(..9)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("OPENCLAW_"))
    } else {
        key.starts_with("OPENCLAW_")
    };
    openclaw_owned || matches("OCM_ACTIVE_ENV") || matches("OCM_ACTIVE_ENV_ROOT")
}

fn apply_openclaw_config_env(config: &Value, env: &mut BTreeMap<String, String>) {
    let Some(config_env) = config.get("env").and_then(Value::as_object) else {
        return;
    };
    let case_insensitive = cfg!(windows);
    let mut entries = BTreeMap::new();
    if let Some(vars) = config_env.get("vars").and_then(Value::as_object) {
        for (key, value) in vars {
            collect_openclaw_config_env_entry(key, value, &mut entries, case_insensitive);
        }
    }
    for (key, value) in config_env {
        if key != "shellEnv" && key != "vars" {
            collect_openclaw_config_env_entry(key, value, &mut entries, case_insensitive);
        }
    }
    for (key, value) in entries {
        if matching_env_value(env, &key, case_insensitive)
            .is_some_and(|value| !value.trim().is_empty())
        {
            continue;
        }
        insert_env_entry(env, key, value, case_insensitive);
    }
}

fn collect_openclaw_config_env_entry(
    key: &str,
    value: &Value,
    entries: &mut BTreeMap<String, String>,
    case_insensitive: bool,
) {
    let Some(value) = value.as_str().filter(|value| !value.trim().is_empty()) else {
        return;
    };
    if !is_portable_env_var_name(key) || contains_config_env_reference(value) {
        return;
    }
    insert_env_entry(
        entries,
        key.to_string(),
        value.to_string(),
        case_insensitive,
    );
}

fn insert_env_entry(
    env: &mut BTreeMap<String, String>,
    key: String,
    value: String,
    case_insensitive: bool,
) {
    if let Some(existing) = matching_env_key(env, &key, case_insensitive) {
        env.remove(&existing);
    }
    env.insert(key, value);
}

fn matching_env_value<'a>(
    env: &'a BTreeMap<String, String>,
    key: &str,
    case_insensitive: bool,
) -> Option<&'a String> {
    if let Some(value) = env.get(key) {
        return Some(value);
    }
    case_insensitive
        .then(|| {
            env.iter()
                .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
                .map(|(_, value)| value)
        })
        .flatten()
}

fn matching_env_key(
    env: &BTreeMap<String, String>,
    key: &str,
    case_insensitive: bool,
) -> Option<String> {
    if env.contains_key(key) {
        return Some(key.to_string());
    }
    case_insensitive
        .then(|| {
            env.keys()
                .find(|candidate| candidate.eq_ignore_ascii_case(key))
                .cloned()
        })
        .flatten()
}

fn contains_config_env_reference(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            index += 1;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'$') && bytes.get(index + 2) == Some(&b'{') {
            index += 2;
            continue;
        }
        if bytes.get(index + 1) == Some(&b'{') {
            let name_start = index + 2;
            if let Some(relative_end) = value[name_start..].find('}') {
                let name_end = name_start + relative_end;
                if is_openclaw_env_var_name(&value[name_start..name_end]) {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

fn is_portable_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn resolve_config_env_vars(value: Value, env: &BTreeMap<String, String>) -> Value {
    match value {
        Value::String(value) => Value::String(resolve_config_env_string(&value, env)),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| resolve_config_env_vars(value, env))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, resolve_config_env_vars(value, env)))
                .collect(),
        ),
        value => value,
    }
}

fn resolve_config_env_string(value: &str, env: &BTreeMap<String, String>) -> String {
    let mut resolved = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            let ch = value[index..]
                .chars()
                .next()
                .expect("valid string boundary");
            resolved.push(ch);
            index += ch.len_utf8();
            continue;
        }

        let (escaped, name_start) =
            if bytes.get(index + 1) == Some(&b'$') && bytes.get(index + 2) == Some(&b'{') {
                (true, index + 3)
            } else if bytes.get(index + 1) == Some(&b'{') {
                (false, index + 2)
            } else {
                resolved.push('$');
                index += 1;
                continue;
            };
        let Some(relative_end) = value[name_start..].find('}') else {
            resolved.push('$');
            index += 1;
            continue;
        };
        let name_end = name_start + relative_end;
        let name = &value[name_start..name_end];
        if !is_openclaw_env_var_name(name) {
            resolved.push('$');
            index += 1;
            continue;
        }
        if escaped {
            resolved.push_str("${");
            resolved.push_str(name);
            resolved.push('}');
        } else if let Some(env_value) =
            matching_env_value(env, name, cfg!(windows)).filter(|value| !value.is_empty())
        {
            resolved.push_str(env_value);
        } else {
            resolved.push_str("${");
            resolved.push_str(name);
            resolved.push('}');
        }
        index = name_end + 1;
    }
    resolved
}

fn is_openclaw_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_uppercase())
        && chars.all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
}

fn agent_entries(config: &Value) -> Vec<Map<String, Value>> {
    if let Some(entries) = config.pointer("/agents/entries").and_then(Value::as_object) {
        return javascript_property_entries(entries)
            .into_iter()
            .filter(|(agent_id, _)| agent_id.as_str() != "__proto__")
            .filter_map(|(agent_id, entry)| {
                let mut entry = entry.as_object()?.clone();
                entry.insert("id".to_string(), Value::String(agent_id.clone()));
                Some(entry)
            })
            .collect();
    }
    config
        .pointer("/agents/list")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_object)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn is_blocked_object_key(key: &str) -> bool {
    BLOCKED_OBJECT_KEYS.contains(&key)
}

fn javascript_property_entries(entries: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut integer_entries = entries
        .iter()
        .filter_map(|entry @ (key, _)| javascript_array_index(key).map(|index| (index, entry)))
        .collect::<Vec<_>>();
    integer_entries.sort_unstable_by_key(|(index, _)| *index);

    integer_entries
        .into_iter()
        .map(|(_, entry)| entry)
        .chain(
            entries
                .iter()
                .filter(|(key, _)| javascript_array_index(key).is_none()),
        )
        .collect()
}

fn javascript_array_index(key: &str) -> Option<u32> {
    let index = key.parse::<u32>().ok()?;
    (index != u32::MAX && index.to_string() == key).then_some(index)
}

fn resolve_default_agent_id(entries: &[Map<String, Value>]) -> String {
    let selected = entries
        .iter()
        .find(|entry| entry.get("default").and_then(Value::as_bool) == Some(true))
        .or_else(|| entries.first());
    selected
        .and_then(|entry| entry.get("id"))
        .and_then(Value::as_str)
        .map(normalize_agent_id)
        .unwrap_or_else(|| DEFAULT_AGENT_ID.to_string())
}

pub(crate) fn normalize_agent_id(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return DEFAULT_AGENT_ID.to_string();
    }
    let lowercase = trimmed.to_lowercase();
    let valid = trimmed.len() <= 64
        && trimmed
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric())
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if valid {
        return lowercase;
    }

    let mut normalized = String::new();
    let mut replacing = false;
    for ch in lowercase.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            normalized.push(ch);
            replacing = false;
        } else if !replacing {
            normalized.push('-');
            replacing = true;
        }
    }
    let normalized = normalized
        .trim_matches('-')
        .chars()
        .take(64)
        .collect::<String>();
    if normalized.is_empty() {
        DEFAULT_AGENT_ID.to_string()
    } else {
        normalized
    }
}

fn resolve_workspace_path(raw: &str, openclaw_home: &Path) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.contains('\0') {
        return Err("OpenClaw workspace path must not contain null bytes".to_string());
    }
    let expanded = match trimmed {
        "~" => openclaw_home.to_path_buf(),
        _ if trimmed.starts_with("~/") || trimmed.starts_with("~\\") => {
            let mut expanded = openclaw_home.as_os_str().to_os_string();
            expanded.push(&trimmed[1..]);
            PathBuf::from(expanded)
        }
        _ => PathBuf::from(trimmed),
    };
    if !expanded.is_absolute() {
        return Err(format!(
            "cannot safely preserve relative OpenClaw workspace path \"{trimmed}\"; configure an absolute path or a path under ~"
        ));
    }
    Ok(clean_path(&expanded))
}

struct IncludeProcessor {
    root_dir: PathBuf,
    canonical_root_dir: PathBuf,
    include_paths: BTreeSet<PathBuf>,
}

impl IncludeProcessor {
    fn new(root_dir: &Path) -> Result<Self, String> {
        let root_dir = clean_path(root_dir);
        let canonical_root_dir = canonicalize_path_allow_missing(&root_dir)?;
        Ok(Self {
            root_dir,
            canonical_root_dir,
            include_paths: BTreeSet::new(),
        })
    }

    fn process(
        &mut self,
        value: Value,
        containing_file: &Path,
        depth: usize,
        stack: &mut Vec<PathBuf>,
    ) -> Result<Value, String> {
        match value {
            Value::Array(values) => values
                .into_iter()
                .map(|value| self.process(value, containing_file, depth, stack))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            Value::Object(mut object) => {
                let Some(include) = object.remove("$include") else {
                    let mut resolved = Map::new();
                    for (key, value) in object {
                        resolved.insert(key, self.process(value, containing_file, depth, stack)?);
                    }
                    return Ok(Value::Object(resolved));
                };

                let included = self.resolve_include(include, containing_file, depth, stack)?;
                if object.is_empty() {
                    return Ok(included);
                }
                if !included.is_object() {
                    return Err(
                        "OpenClaw $include sibling keys require included content to be an object"
                            .to_string(),
                    );
                }

                let mut siblings = Map::new();
                for (key, value) in object {
                    siblings.insert(key, self.process(value, containing_file, depth, stack)?);
                }
                Ok(deep_merge(included, Value::Object(siblings)))
            }
            value => Ok(value),
        }
    }

    fn resolve_include(
        &mut self,
        include: Value,
        containing_file: &Path,
        depth: usize,
        stack: &mut Vec<PathBuf>,
    ) -> Result<Value, String> {
        match include {
            Value::String(path) => self.load_include(&path, containing_file, depth, stack),
            Value::Array(paths) => {
                let mut merged = Value::Object(Map::new());
                for path in paths {
                    let path = path.as_str().ok_or_else(|| {
                        "OpenClaw $include arrays must contain only paths".to_string()
                    })?;
                    merged = deep_merge(
                        merged,
                        self.load_include(path, containing_file, depth, stack)?,
                    );
                }
                Ok(merged)
            }
            _ => Err("OpenClaw $include must be a path or an array of paths".to_string()),
        }
    }

    fn load_include(
        &mut self,
        include_path: &str,
        containing_file: &Path,
        depth: usize,
        stack: &mut Vec<PathBuf>,
    ) -> Result<Value, String> {
        if depth >= MAX_INCLUDE_DEPTH {
            return Err(format!(
                "maximum OpenClaw $include depth ({MAX_INCLUDE_DEPTH}) exceeded at {include_path}"
            ));
        }
        let resolved = self.resolve_include_path(include_path, containing_file)?;
        if stack.contains(&resolved) {
            let chain = stack
                .iter()
                .chain(std::iter::once(&resolved))
                .map(|path| display_path(path))
                .collect::<Vec<_>>()
                .join(" -> ");
            return Err(format!("circular OpenClaw $include detected: {chain}"));
        }

        let raw =
            read_regular_text_file(&resolved, "OpenClaw include", Some(MAX_INCLUDE_FILE_BYTES))?;
        let parsed = parse_json5(&raw, &resolved, "OpenClaw include")?;
        self.include_paths.insert(resolved.clone());
        stack.push(resolved.clone());
        let result = self.process(parsed, &resolved, depth + 1, stack);
        stack.pop();
        result
    }

    fn resolve_include_path(
        &self,
        include_path: &str,
        containing_file: &Path,
    ) -> Result<PathBuf, String> {
        if include_path.contains('\0') {
            return Err("OpenClaw $include path must not contain null bytes".to_string());
        }
        if include_path.len() >= MAX_INCLUDE_PATH_LENGTH {
            return Err(format!(
                "OpenClaw $include path exceeds {MAX_INCLUDE_PATH_LENGTH} characters"
            ));
        }
        let include_path = Path::new(include_path);
        let resolved = if include_path.is_absolute() {
            clean_path(include_path)
        } else {
            clean_path(
                &containing_file
                    .parent()
                    .unwrap_or(&self.root_dir)
                    .join(include_path),
            )
        };
        if display_path(&resolved).len() >= MAX_INCLUDE_PATH_LENGTH {
            return Err(format!(
                "resolved OpenClaw $include path exceeds {MAX_INCLUDE_PATH_LENGTH} characters"
            ));
        }
        if !resolved.starts_with(&self.root_dir) {
            return Err(format!(
                "OpenClaw $include path escapes the config directory: {}",
                display_path(&resolved)
            ));
        }
        let canonical = canonicalize_path_allow_missing(&resolved)?;
        if !canonical.starts_with(&self.canonical_root_dir) {
            return Err(format!(
                "OpenClaw $include path resolves outside the config directory: {}",
                display_path(&resolved)
            ));
        }
        Ok(resolved)
    }
}

fn read_regular_text_file(
    path: &Path,
    label: &str,
    max_bytes: Option<u64>,
) -> Result<String, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);

    let mut file = options
        .open(path)
        .map_err(|error| format!("failed to open {label} {}: {error}", display_path(path)))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to inspect {label} {}: {error}", display_path(path)))?;
    if !metadata.is_file() {
        return Err(format!(
            "{label} must be a regular file: {}",
            display_path(path)
        ));
    }
    if max_bytes.is_some_and(|max_bytes| metadata.len() > max_bytes) {
        return Err(format!(
            "{label} exceeds {} bytes: {}",
            max_bytes.unwrap_or_default(),
            display_path(path)
        ));
    }

    let mut raw = String::new();
    if let Some(max_bytes) = max_bytes {
        file.take(max_bytes + 1)
            .read_to_string(&mut raw)
            .map_err(|error| format!("failed to read {label} {}: {error}", display_path(path)))?;
        if raw.len() as u64 > max_bytes {
            return Err(format!(
                "{label} exceeds {max_bytes} bytes: {}",
                display_path(path)
            ));
        }
    } else {
        file.read_to_string(&mut raw)
            .map_err(|error| format!("failed to read {label} {}: {error}", display_path(path)))?;
    }
    Ok(raw)
}

fn parse_json5(raw: &str, path: &Path, label: &str) -> Result<Value, String> {
    json5::from_str(raw)
        .map_err(|error| format!("failed to parse {label} {}: {error}", display_path(path)))
}

fn deep_merge(base: Value, override_value: Value) -> Value {
    match (base, override_value) {
        (Value::Array(mut base), Value::Array(override_values)) => {
            base.extend(override_values);
            Value::Array(base)
        }
        (Value::Object(base), Value::Object(override_values)) => {
            let mut base = sanitize_plain_object(base);
            let override_values = sanitize_plain_object(override_values);
            for (key, value) in override_values {
                if let Some(current) = base.get_mut(&key) {
                    *current = deep_merge(std::mem::take(current), value);
                } else {
                    base.insert(key, value);
                }
            }
            Value::Object(base)
        }
        (_, Value::Object(override_values)) => {
            Value::Object(sanitize_plain_object(override_values))
        }
        (_, override_value) => override_value,
    }
}

fn sanitize_plain_object(object: Map<String, Value>) -> Map<String, Value> {
    object
        .into_iter()
        .filter(|(key, _)| !is_blocked_object_key(key))
        .map(|(key, value)| {
            let value = match value {
                Value::Object(nested) => Value::Object(sanitize_plain_object(nested)),
                value => value,
            };
            (key, value)
        })
        .collect()
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
                return Ok(clean_path(&resolved));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = existing
                    .file_name()
                    .ok_or_else(|| format!("failed to resolve path: {}", display_path(path)))?;
                missing.push(name.to_os_string());
                existing = existing
                    .parent()
                    .ok_or_else(|| format!("failed to resolve path: {}", display_path(path)))?;
            }
            Err(error) => {
                return Err(format!(
                    "failed to resolve path {}: {error}",
                    display_path(path)
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::ffi::CString;
    #[cfg(unix)]
    use std::os::unix::ffi::OsStrExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ocm-openclaw-workspaces-{label}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn test_env() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    #[test]
    fn effective_config_resolves_json5_nested_includes_and_merges_arrays() {
        let root = test_root("includes");
        let state_dir = root.join(".openclaw");
        fs::create_dir_all(state_dir.join("config")).unwrap();
        fs::write(
            state_dir.join("openclaw.json"),
            "{ $include: ['./config/base.json5', './config/agents.json5'], agents: { list: [{ id: 'local' }] } }",
        )
        .unwrap();
        fs::write(
            state_dir.join("config/base.json5"),
            "{ agents: { defaults: { workspace: '~/teams' } } }",
        )
        .unwrap();
        fs::write(
            state_dir.join("config/agents.json5"),
            "{ $include: './nested.json5', agents: { list: [{ id: 'ops' }] } }",
        )
        .unwrap();
        fs::write(
            state_dir.join("config/nested.json5"),
            "{ agents: { list: [{ id: 'main', default: true }] } }",
        )
        .unwrap();

        let resolved = load_effective_openclaw_config(&state_dir.join("openclaw.json"))
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved.value.pointer("/agents/defaults/workspace"),
            Some(&Value::String("~/teams".to_string()))
        );
        let ids = resolved
            .value
            .pointer("/agents/list")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|entry| entry.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["main", "ops", "local"]);
        assert_eq!(resolved.include_paths.len(), 3);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_inventory_matches_openclaw_agent_resolution() {
        let root = test_root("inventory");
        let paths = super::super::layout::derive_env_paths(&root);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(
            &paths.config_path,
            r#"{
              "agents": {
                "defaults": { "workspace": "~/teams" },
                "entries": {
                  "Primary": { "default": true },
                  "ops-team": {},
                  "Custom": { "workspace": "~/.openclaw/team/custom" }
                }
              }
            }"#,
        )
        .unwrap();

        let inventory = resolve_env_openclaw_workspaces(
            &paths,
            &test_env(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert_eq!(
            inventory.workspace_roots,
            BTreeSet::from([
                root.join(".openclaw/workspace"),
                root.join("teams"),
                root.join("teams/ops-team"),
                root.join(".openclaw/team/custom"),
            ])
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn keyed_workspace_inventory_preserves_the_first_entry_as_the_implicit_default() {
        let root = test_root("inventory-key-order");
        let paths = super::super::layout::derive_env_paths(&root);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(
            &paths.config_path,
            r#"{
              "agents": {
                "defaults": { "workspace": "~/teams" },
                "entries": {
                  "zeta": { "workspace": "~/zeta" },
                  "alpha": { "workspace": "~/alpha" }
                }
              }
            }"#,
        )
        .unwrap();

        let inventory = resolve_env_openclaw_workspaces(
            &paths,
            &test_env(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert_eq!(inventory.default_agent_id, "zeta");
        assert_eq!(
            inventory.default_agent_workspace(),
            Some(root.join("zeta").as_path())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn keyed_workspace_inventory_matches_javascript_integer_key_order() {
        let root = test_root("inventory-key-integer-order");
        let paths = super::super::layout::derive_env_paths(&root);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(
            &paths.config_path,
            r#"{
              "agents": {
                "entries": {
                  "10": {},
                  "2": {}
                }
              }
            }"#,
        )
        .unwrap();

        let inventory = resolve_env_openclaw_workspaces(
            &paths,
            &test_env(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert_eq!(inventory.default_agent_id, "2");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn keyed_workspace_inventory_preserves_direct_constructor_and_prototype_keys() {
        let root = test_root("inventory-direct-object-keys");
        let paths = super::super::layout::derive_env_paths(&root);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(
            &paths.config_path,
            r#"{
              "agents": {
                "defaults": { "workspace": "~/teams" },
                "entries": {
                  "constructor": { "workspace": "~/constructor" },
                  "prototype": { "workspace": "~/prototype" },
                  "__proto__": { "workspace": "~/ignored-proto" },
                  "ops": {}
                }
              }
            }"#,
        )
        .unwrap();

        let inventory = resolve_env_openclaw_workspaces(
            &paths,
            &test_env(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert_eq!(inventory.default_agent_id, "constructor");
        assert_eq!(
            inventory.default_agent_workspace(),
            Some(root.join("constructor").as_path())
        );
        assert_eq!(
            inventory.agent_workspace("prototype"),
            Some(root.join("prototype").as_path())
        );
        assert_eq!(inventory.agent_workspace("__proto__"), None);
        assert!(!inventory.contains(&root.join("ignored-proto")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn keyed_workspace_inventory_matches_openclaw_include_sanitization() {
        let root = test_root("inventory-included-object-keys");
        let paths = super::super::layout::derive_env_paths(&root);
        fs::create_dir_all(paths.state_dir.join("config")).unwrap();
        fs::write(
            &paths.config_path,
            r#"{
              "$include": "./config/agents.json5",
              "env": {}
            }"#,
        )
        .unwrap();
        fs::write(
            paths.state_dir.join("config/agents.json5"),
            r#"{
              "agents": {
                "defaults": { "workspace": "~/teams" },
                "entries": {
                  "constructor": { "workspace": "~/ignored" },
                  "prototype": { "workspace": "~/ignored-prototype" },
                  "__proto__": { "workspace": "~/ignored-proto" },
                  "ops": {}
                }
              }
            }"#,
        )
        .unwrap();

        let inventory = resolve_env_openclaw_workspaces(
            &paths,
            &test_env(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert_eq!(inventory.default_agent_id, "ops");
        assert_eq!(
            inventory.default_agent_workspace(),
            Some(root.join("teams").as_path())
        );
        assert_eq!(inventory.agent_workspace("constructor"), None);
        assert_eq!(inventory.agent_workspace("prototype"), None);
        assert_eq!(inventory.agent_workspace("__proto__"), None);
        assert!(!inventory.contains(&root.join("ignored")));
        assert!(!inventory.contains(&root.join("ignored-prototype")));
        assert!(!inventory.contains(&root.join("ignored-proto")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn keyed_workspace_inventory_preserves_included_key_position_on_override() {
        let root = test_root("inventory-key-include-order");
        let paths = super::super::layout::derive_env_paths(&root);
        fs::create_dir_all(paths.state_dir.join("config")).unwrap();
        fs::write(
            &paths.config_path,
            r#"{
              "$include": "./config/agents.json5",
              "agents": {
                "entries": {
                  "primary": { "name": "override" }
                }
              }
            }"#,
        )
        .unwrap();
        fs::write(
            paths.state_dir.join("config/agents.json5"),
            r#"{
              agents: {
                entries: {
                  primary: {},
                  ops: {}
                }
              }
            }"#,
        )
        .unwrap();

        let inventory = resolve_env_openclaw_workspaces(
            &paths,
            &test_env(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert_eq!(inventory.default_agent_id, "primary");
        assert_eq!(
            inventory.agent_workspace("ops"),
            Some(paths.state_dir.join("workspace-ops").as_path())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_inventory_resolves_openclaw_config_env_substitution() {
        let root = test_root("env-substitution");
        let paths = super::super::layout::derive_env_paths(&root);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(
            &paths.config_path,
            r#"{
              "agents": {
                "defaults": {
                  "workspace": "${OPENCLAW_HOME}/.openclaw/team"
                },
                "list": [
                  { "id": "main", "default": true },
                  { "id": "ops", "workspace": "${CUSTOM_WORKSPACE_ROOT}/ops" }
                ]
              }
            }"#,
        )
        .unwrap();
        let env = BTreeMap::from([(
            "CUSTOM_WORKSPACE_ROOT".to_string(),
            display_path(&paths.state_dir.join("custom")),
        )]);
        let mut config: Value =
            serde_json::from_str(&fs::read_to_string(&paths.config_path).unwrap()).unwrap();
        config["env"] = serde_json::json!({
            "vars": {
                "CONFIG_WORKSPACE_ROOT": display_path(&paths.state_dir.join("configured")),
                "IGNORED_REFERENCE": "${CUSTOM_WORKSPACE_ROOT}"
            }
        });
        config["agents"]["list"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": "config",
                "workspace": "${CONFIG_WORKSPACE_ROOT}/agent"
            }));
        fs::write(
            &paths.config_path,
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();

        let inventory =
            resolve_env_openclaw_workspaces(&paths, &env, OpenClawWorkspaceRuntime::default())
                .unwrap();
        assert!(inventory.contains(&paths.state_dir.join("team")));
        assert!(inventory.contains(&paths.state_dir.join("custom/ops")));
        assert!(inventory.contains(&paths.state_dir.join("configured/agent")));
        assert_eq!(
            resolve_config_env_string(
                "$${OPENCLAW_HOME}/${OPENCLAW_HOME}",
                &openclaw_config_env(
                    &env,
                    &paths.openclaw_home,
                    &paths.state_dir,
                    &paths.config_path,
                    OpenClawWorkspaceRuntime::default()
                )
            ),
            format!("${{OPENCLAW_HOME}}/{}", display_path(&paths.openclaw_home))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_inventory_uses_ocm_runtime_identity_instead_of_stale_caller_values() {
        let root = test_root("runtime-identity");
        let paths = super::super::layout::derive_env_paths(&root);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(
            &paths.config_path,
            r#"{
              "agents": {
                "defaults": {
                  "workspace": "${OCM_ACTIVE_ENV_ROOT}/.openclaw/team/${OCM_ACTIVE_ENV}-${OPENCLAW_GATEWAY_PORT}"
                }
              }
            }"#,
        )
        .unwrap();
        let env = BTreeMap::from([
            ("OCM_ACTIVE_ENV".to_string(), "stale".to_string()),
            (
                "OCM_ACTIVE_ENV_ROOT".to_string(),
                "/tmp/stale-root".to_string(),
            ),
            ("OPENCLAW_GATEWAY_PORT".to_string(), "1".to_string()),
        ]);

        let inventory = resolve_env_openclaw_workspaces(
            &paths,
            &env,
            OpenClawWorkspaceRuntime::for_env("source", Some(19_789)),
        )
        .unwrap();

        assert!(inventory.contains(&paths.state_dir.join("team/source-19789")));
        assert!(!inventory.contains(Path::new("/tmp/stale-root")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_identity_replaces_differently_cased_windows_env_keys() {
        let root = test_root("runtime-identity-windows-casing");
        let paths = super::super::layout::derive_env_paths(&root);
        let env = BTreeMap::from([
            ("openclaw_home".to_string(), "C:\\stale".to_string()),
            ("openclaw_gateway_port".to_string(), "1".to_string()),
            ("ocm_active_env".to_string(), "stale".to_string()),
            (
                "ocm_active_env_root".to_string(),
                "C:\\stale-root".to_string(),
            ),
        ]);

        let resolved = openclaw_config_env_with_platform_semantics(
            &env,
            &paths.openclaw_home,
            &paths.state_dir,
            &paths.config_path,
            OpenClawWorkspaceRuntime::for_env("source", Some(19_789)),
            true,
        );

        assert_eq!(resolved.len(), 7);
        assert_eq!(
            matching_env_value(&resolved, "OPENCLAW_HOME", true).map(String::as_str),
            Some(display_path(&paths.openclaw_home).as_str())
        );
        assert_eq!(
            matching_env_value(&resolved, "OPENCLAW_GATEWAY_PORT", true).map(String::as_str),
            Some("19789")
        );
        assert_eq!(
            matching_env_value(&resolved, "OCM_ACTIVE_ENV", true).map(String::as_str),
            Some("source")
        );
        assert_eq!(
            matching_env_value(&resolved, "OCM_ACTIVE_ENV_ROOT", true).map(String::as_str),
            Some(display_path(&paths.openclaw_home).as_str())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn config_env_top_level_entries_override_vars_entries() {
        let config = serde_json::json!({
            "env": {
                "vars": {
                    "WORKSPACE_ROOT": "/from-vars",
                    "ONLY_VARS": "/only-vars"
                },
                "WORKSPACE_ROOT": "/from-top-level"
            }
        });
        let mut env = BTreeMap::new();

        apply_openclaw_config_env(&config, &mut env);

        assert_eq!(
            env.get("WORKSPACE_ROOT").map(String::as_str),
            Some("/from-top-level")
        );
        assert_eq!(env.get("ONLY_VARS").map(String::as_str), Some("/only-vars"));
    }

    #[test]
    fn config_env_preserves_host_precedence_with_windows_key_semantics() {
        let mut entries =
            BTreeMap::from([("workspace_root".to_string(), "/from-host".to_string())]);
        assert_eq!(
            matching_env_value(&entries, "WORKSPACE_ROOT", true).map(String::as_str),
            Some("/from-host")
        );

        insert_env_entry(
            &mut entries,
            "WORKSPACE_ROOT".to_string(),
            "/from-config".to_string(),
            true,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(
            matching_env_value(&entries, "workspace_root", true).map(String::as_str),
            Some("/from-config")
        );
    }

    #[test]
    fn workspace_inventory_rejects_external_and_symlink_escaped_paths() {
        let root = test_root("external");
        let paths = super::super::layout::derive_env_paths(&root);
        fs::create_dir_all(&paths.state_dir).unwrap();
        let external = test_root("external-target");
        fs::create_dir_all(&external).unwrap();
        fs::write(
            &paths.config_path,
            format!(
                "{{\"agents\":{{\"defaults\":{{\"workspace\":\"{}\"}}}}}}",
                external.display()
            ),
        )
        .unwrap();
        let inventory = resolve_env_openclaw_workspaces(
            &paths,
            &test_env(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert!(
            inventory.archive_relative_roots(&paths.root).is_err(),
            "external workspace must be rejected"
        );

        #[cfg(unix)]
        {
            let linked = paths.state_dir.join("linked");
            std::os::unix::fs::symlink(&external, &linked).unwrap();
            fs::write(
                &paths.config_path,
                format!(
                    "{{\"agents\":{{\"defaults\":{{\"workspace\":\"{}\"}}}}}}",
                    linked.display()
                ),
            )
            .unwrap();
            let inventory = resolve_env_openclaw_workspaces(
                &paths,
                &test_env(),
                OpenClawWorkspaceRuntime::default(),
            )
            .unwrap();
            assert!(
                inventory.archive_relative_roots(&paths.root).is_err(),
                "workspace symlink escaping the env root must be rejected"
            );
        }

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(external);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_inventory_rejects_internal_workspace_symlinks() {
        let root = test_root("internal-symlink");
        let paths = super::super::layout::derive_env_paths(&root);
        let target = paths.state_dir.join("team/ops");
        let linked = paths.state_dir.join("linked");
        fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        fs::write(
            &paths.config_path,
            format!(
                "{{\"agents\":{{\"defaults\":{{\"workspace\":\"{}\"}}}}}}",
                linked.display()
            ),
        )
        .unwrap();

        let inventory = resolve_env_openclaw_workspaces(
            &paths,
            &test_env(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert!(inventory.contains(&linked));
        let error = inventory.archive_relative_roots(&paths.root).unwrap_err();
        assert!(error.contains("through a symlink"), "{error}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_inventory_rejects_relative_paths() {
        let root = test_root("relative");
        let paths = super::super::layout::derive_env_paths(&root);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(
            &paths.config_path,
            r#"{"agents":{"defaults":{"workspace":"relative/workspace"}}}"#,
        )
        .unwrap();

        let error = resolve_env_openclaw_workspaces(
            &paths,
            &test_env(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap_err();
        assert!(
            error.contains("relative OpenClaw workspace path"),
            "{error}"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_inventory_does_not_infer_prefix_lookalikes() {
        let root = test_root("lookalikes");
        let paths = super::super::layout::derive_env_paths(&root);
        fs::create_dir_all(&paths.state_dir).unwrap();
        fs::write(&paths.config_path, "{}").unwrap();

        let inventory = resolve_env_openclaw_workspaces(
            &paths,
            &test_env(),
            OpenClawWorkspaceRuntime::default(),
        )
        .unwrap();
        assert!(inventory.contains(&paths.workspace_dir));
        assert!(!inventory.contains(&paths.state_dir.join("workspace-attestations")));
        assert!(!inventory.contains(&paths.state_dir.join("workspace-cache")));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn effective_config_rejects_circular_includes() {
        let root = test_root("circular");
        let state_dir = root.join(".openclaw");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            state_dir.join("openclaw.json"),
            r#"{"$include":"./a.json5"}"#,
        )
        .unwrap();
        fs::write(
            state_dir.join("a.json5"),
            r#"{"$include":"./openclaw.json"}"#,
        )
        .unwrap();

        let error = load_effective_openclaw_config(&state_dir.join("openclaw.json")).unwrap_err();
        assert!(error.contains("circular OpenClaw $include"), "{error}");

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn effective_config_rejects_non_regular_includes_without_blocking() {
        let root = test_root("fifo-include");
        let state_dir = root.join(".openclaw");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            state_dir.join("openclaw.json"),
            r#"{"$include":"./agents.json5"}"#,
        )
        .unwrap();
        let fifo = state_dir.join("agents.json5");
        let fifo_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);

        let error = load_effective_openclaw_config(&state_dir.join("openclaw.json")).unwrap_err();
        assert!(error.contains("must be a regular file"), "{error}");

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn workspace_home_expansion_preserves_backslashes_on_unix() {
        assert_eq!(
            resolve_workspace_path(r"~\team", Path::new("/tmp/openclaw-home")).unwrap(),
            PathBuf::from(r"/tmp/openclaw-home\team")
        );
    }
}
